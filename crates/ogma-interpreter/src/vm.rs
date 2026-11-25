//! M1 register-bytecode execution engine.
//!
//! The interpreter consumes verifier-produced decoded instructions and the
//! shared Runtime. It deliberately has no parser, compiler or Core IR dependency.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::rc::Rc;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::{Duration, Instant};

use ogma_bytecode::{
    BinaryOperation, CallKind, ConstantKind, DecodedRegisterInstruction, Diagnostic,
    RegisterOperation, UnaryOperation, VerifiedArtifact, verify_register_artifact_from_manifest,
};
use ogma_runtime::{
    BindingDefinition, BindingId, CallActivation, CaptureDefinition, CaptureMode,
    FunctionDefinition, FunctionId, FunctionKind, Heap, HeapError, ScopeToken, Value, ValueType,
};
use serde_json::Value as JsonValue;

/// Cancellation signal shared with a host control thread.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    /// Creates a non-cancelled signal.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Requests cooperative cancellation.
    pub fn cancel(&self) { self.cancelled.store(true, Ordering::Release); }

    /// Returns whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool { self.cancelled.load(Ordering::Acquire) }
}

/// A cooperative VM interruption.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionInterruption {
    /// The instruction/fuel budget was exhausted.
    FuelExhausted,
    /// The host cancellation signal was set.
    Cancelled,
    /// The monotonic deadline or time budget elapsed.
    DeadlineExceeded,
}

/// Observable completion of a synchronous M1 invocation.
#[derive(Clone, Copy, Debug)]
pub enum ExecutionCompletion {
    /// A normal JavaScript return value.
    Return(Value),
    /// A propagated JavaScript thrown value.
    Throw(Value),
    /// Cooperative interruption at a VM poll point.
    Interrupted(ExecutionInterruption),
}

/// Host execution limits and cooperative interrupt policy.
#[derive(Clone, Debug)]
pub struct ExecutionOptions {
    /// Maximum VM instructions dispatched across the complete call tree.
    pub instruction_budget: u64,
    /// Host call-depth ceiling, further restricted by the artifact manifest.
    pub max_call_depth: u32,
    /// Maximum dispatches between ordinary cancellation/deadline polls.
    ///
    /// Function entry, call boundaries, loop back-edges and potentially
    /// allocating/runtime operations are polled immediately.
    pub interrupt_poll_interval: u32,
    /// Optional host-controlled cancellation signal.
    pub cancellation: Option<CancellationToken>,
    /// Optional absolute deadline from the host monotonic clock.
    pub deadline: Option<Instant>,
    /// Optional time budget measured from the beginning of execute().
    pub time_budget: Option<Duration>,
    /// Global receiver supplied by an embedding host.
    pub global_this: Value,
}

impl Default for ExecutionOptions {
    fn default() -> Self {
        Self {
            instruction_budget: 1_000_000,
            max_call_depth: 256,
            interrupt_poll_interval: 64,
            cancellation: None,
            deadline: None,
            time_budget: None,
            global_this: Value::undefined(),
        }
    }
}

/// Source and lexical metadata for one dispatched instruction.
#[derive(Clone, Debug)]
pub struct InstructionLocation {
    /// Stable function identity.
    pub function_id: String,
    /// Human-readable function name.
    pub function_name: String,
    /// Byte offset in the function code section.
    pub byte_offset: u32,
    /// Code-generation identity, if present.
    pub code_generation_id: Option<String>,
    /// Code-generation version, if present.
    pub code_generation_version: Option<String>,
    /// Source provenance record, when available.
    pub source: Option<JsonValue>,
    /// Lexical scopes covering this byte offset.
    pub lexical_scopes: Vec<JsonValue>,
    /// Variable storage locations covering this byte offset.
    pub variable_locations: Vec<JsonValue>,
}

/// Completion and deterministic accounting for one execution.
#[derive(Clone, Debug)]
pub struct ExecutionReport {
    /// Normal, thrown or interrupted completion.
    pub completion: ExecutionCompletion,
    /// Number of dispatched VM instructions.
    pub instructions_executed: u64,
    /// Remaining instruction fuel.
    pub fuel_remaining: u64,
    /// Maximum VM call depth reached, including the entry frame.
    pub max_call_depth_reached: u32,
    /// Source location of the last dispatched instruction.
    pub last_location: Option<InstructionLocation>,
}

/// Error deliberately raised by a host capability callback.
#[derive(Debug)]
pub enum HostError {
    /// Capability is not registered or is denied.
    Denied,
    /// Host propagates this JavaScript thrown value.
    Throw(Value),
}

/// Synchronous host capability dispatcher on the Runtime owner thread.
pub trait Host {
    /// Invokes a named capability with evaluated arguments.
    fn call(
        &mut self,
        heap: &mut Heap,
        capability: &str,
        arguments: &[Value],
    ) -> Result<Value, HostError>;
}

/// Host implementation which denies every capability call.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoHost;

impl Host for NoHost {
    fn call(
        &mut self,
        _heap: &mut Heap,
        _capability: &str,
        _arguments: &[Value],
    ) -> Result<Value, HostError> {
        Err(HostError::Denied)
    }
}

/// Artifact loading, Runtime or resource failure.
#[derive(Debug)]
pub enum InterpreterError {
    /// Portable bytecode verification failed.
    Bytecode(Vec<Diagnostic>),
    /// Artifact metadata is inconsistent or unsupported.
    InvalidMetadata(String),
    /// Duplicate stable function identity.
    DuplicateFunction(String),
    /// Closure target or invoked Runtime function is absent from the module.
    MissingFunction(String),
    /// Shared Runtime operation failed.
    Runtime(HeapError),
    /// Host capability was denied.
    HostCapabilityDenied(String),
    /// Invalid register or internal register category.
    InvalidRegister(u16),
    /// Missing constant-pool item.
    InvalidConstant(u32),
    /// Constant category is not represented by the current Runtime Value model.
    UnsupportedConstant(ConstantKind),
    /// Operation is outside the current synchronous M1 subset.
    UnsupportedOperation(&'static str),
    /// Host or artifact call-depth ceiling was exceeded.
    CallDepthExceeded { limit: u32 },
    /// Invalid execution configuration.
    InvalidOptions(&'static str),
    /// Invalid branch target or VM continuation.
    InvalidControlFlow(u32),
    /// Stable Runtime identity space was exhausted.
    IdentitySpaceExceeded,
}

impl fmt::Display for InterpreterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for InterpreterError {}

impl From<HeapError> for InterpreterError {
    fn from(error: HeapError) -> Self { Self::Runtime(error) }
}

/// Immutable collection of independently verified OGMB function artifacts.
///
/// The loader accepts serialized bytes and re-runs verification. Caller-
/// supplied decoded instructions and Core IR are never execution inputs.
pub struct ExecutableModule {
    functions: BTreeMap<String, Rc<FunctionImage>>,
    runtime_functions: HashMap<u32, String>,
}

impl ExecutableModule {
    /// Loads one or more serialized OGMB function artifacts.
    pub fn load<I, B>(artifacts: I) -> Result<Self, InterpreterError>
    where
        I: IntoIterator<Item = B>,
        B: AsRef<[u8]>,
    {
        let mut raw_functions = BTreeMap::<String, RawFunction>::new();
        for bytes in artifacts {
            let verified = verify_register_artifact_from_manifest(bytes.as_ref())
                .map_err(InterpreterError::Bytecode)?;
            let raw = RawFunction::parse(verified)?;
            if raw_functions.contains_key(&raw.id) {
                return Err(InterpreterError::DuplicateFunction(raw.id));
            }
            raw_functions.insert(raw.id.clone(), raw);
        }
        if raw_functions.is_empty() {
            return Err(InterpreterError::InvalidMetadata(
                "an executable module requires at least one function artifact".to_owned(),
            ));
        }

        let mut environments = HashMap::<String, String>::new();
        for (function_id, function) in &raw_functions {
            if environments.insert(function.environment_id.clone(), function_id.clone()).is_some() {
                return Err(InterpreterError::InvalidMetadata(format!(
                    "duplicate lexical environment identity {:?}", function.environment_id
                )));
            }
        }
        for function in raw_functions.values() {
            let parent = match function.parent_environment.as_deref() {
                Some(parent_environment) => {
                    let parent_id = environments.get(parent_environment).ok_or_else(|| {
                        InterpreterError::InvalidMetadata(format!(
                            "function {:?} refers to missing parent environment {:?}",
                            function.id, parent_environment
                        ))
                    })?;
                    Some(&raw_functions[parent_id])
                }
                None => None,
            };
            if !function.captures.is_empty() && parent.is_none() {
                return Err(InterpreterError::InvalidMetadata(format!(
                    "capturing function {:?} has no lexical parent environment",
                    function.id
                )));
            }
            for capture in &function.captures {
                let owner_id = environments.get(&capture.source_environment).ok_or_else(|| {
                    InterpreterError::InvalidMetadata(format!(
                        "capture {:?} refers to missing source environment {:?}",
                        capture.id, capture.source_environment
                    ))
                })?;
                let owner = &raw_functions[owner_id];
                if !owner.bindings.iter().any(|binding| binding.id == capture.binding) {
                    return Err(InterpreterError::InvalidMetadata(format!(
                        "capture {:?} refers to missing binding {:?}", capture.id, capture.binding
                    )));
                }
                let parent = parent.ok_or_else(|| InterpreterError::InvalidMetadata(
                    "capture has no lexical parent".to_owned(),
                ))?;
                let direct = function.parent_environment.as_deref()
                    == Some(capture.source_environment.as_str())
                    && parent.bindings.iter().any(|binding| binding.id == capture.binding);
                let forwarded = parent.captures.iter().any(|item| {
                    item.source_environment == capture.source_environment
                        && item.binding == capture.binding
                });
                if !direct && !forwarded {
                    return Err(InterpreterError::InvalidMetadata(format!(
                        "capture {:?} is not visible from its immediate parent environment",
                        capture.id
                    )));
                }
            }
        }

        let mut binding_keys = BTreeSet::<(String, String)>::new();
        for function in raw_functions.values() {
            for binding in &function.bindings {
                binding_keys.insert((function.environment_id.clone(), binding.id.clone()));
            }
            for capture in &function.captures {
                binding_keys.insert((
                    capture.source_environment.clone(),
                    capture.binding.clone(),
                ));
                binding_keys.insert((
                    closure_environment_key(&function.id),
                    capture.id.clone(),
                ));
            }
        }
        let mut binding_ids = HashMap::<(String, String), BindingId>::new();
        for (index, key) in binding_keys.into_iter().enumerate() {
            let numeric = u32::try_from(index + 1)
                .map_err(|_| InterpreterError::IdentitySpaceExceeded)?;
            binding_ids.insert(key, BindingId(numeric));
        }

        let mut runtime_functions = HashMap::new();
        let mut functions = BTreeMap::new();
        for (index, (function_id, raw)) in raw_functions.iter().enumerate() {
            let numeric = u32::try_from(index + 1)
                .map_err(|_| InterpreterError::IdentitySpaceExceeded)?;
            let image = Rc::new(FunctionImage::build(
                raw.clone(),
                FunctionId(numeric),
                &binding_ids,
                &raw_functions,
            )?);
            runtime_functions.insert(numeric, function_id.clone());
            functions.insert(function_id.clone(), image);
        }

        for image in functions.values() {
            for instruction in &image.artifact.instructions {
                if let RegisterOperation::Constant {
                    constant,
                    kind: ConstantKind::FunctionId,
                    ..
                } = &instruction.operation
                {
                    let target = image.artifact.constants
                        .get(*constant as usize)
                        .and_then(JsonValue::as_str)
                        .ok_or(InterpreterError::InvalidConstant(*constant))?;
                    if !functions.contains_key(target) {
                        return Err(InterpreterError::MissingFunction(target.to_owned()));
                    }
                }
                if let RegisterOperation::Constant {
                    kind: ConstantKind::BigInt,
                    ..
                } = &instruction.operation
                {
                    return Err(InterpreterError::UnsupportedConstant(ConstantKind::BigInt));
                }
            }
        }

        Ok(Self { functions, runtime_functions })
    }

    /// Loads and verifies one serialized function artifact.
    pub fn load_one(bytes: &[u8]) -> Result<Self, InterpreterError> {
        Self::load([bytes])
    }

    /// Returns the verified function count.
    #[must_use]
    pub fn function_count(&self) -> usize { self.functions.len() }

    /// Returns manifest metadata for a function.
    #[must_use]
    pub fn function_metadata(&self, function_id: &str) -> Option<&JsonValue> {
        self.functions.get(function_id)
            .and_then(|image| image.artifact.manifest.get("metadata"))
    }

    /// Returns optional source/debug map for a function.
    #[must_use]
    pub fn source_map(&self, function_id: &str) -> Option<&JsonValue> {
        self.functions.get(function_id)
            .and_then(|image| image.artifact.source_map.as_ref())
    }

    /// Resolves a verified instruction offset to its source/debug metadata.
    #[must_use]
    pub fn source_location(&self, function_id: &str, byte_offset: u32) -> Option<InstructionLocation> {
        self.functions.get(function_id)?.location(byte_offset)
    }

    /// Executes an entry function and all nested VM calls.
    ///
    /// Entry functions with lexical captures cannot be called directly.
    pub fn execute<H: Host>(
        &self,
        heap: &mut Heap,
        entry_function: &str,
        arguments: &[Value],
        host: &mut H,
        options: ExecutionOptions,
    ) -> Result<ExecutionReport, InterpreterError> {
        validate_options(&options)?;
        let image = self.functions.get(entry_function).cloned()
            .ok_or_else(|| InterpreterError::MissingFunction(entry_function.to_owned()))?;
        if !image.captures.is_empty() {
            return Err(InterpreterError::InvalidMetadata(format!(
                "entry function {entry_function:?} requires lexical captures"
            )));
        }

        let mut state = ExecutionState::new(&options)?;
        let scope = heap.open_scope()?;
        let setup = (|| {
            let outer = heap.create_environment(None, &[])?;
            heap.scope_root_environment(scope, outer)?;
            let closure = heap.create_closure(&image.definition, outer, options.global_this)?;
            heap.scope_root(scope, closure)?;
            heap.enter_function_call(
                closure,
                &image.definition,
                Value::undefined(),
                arguments,
                options.global_this,
            )
        })();
        let activation = match setup {
            Ok(activation) => activation,
            Err(error) => {
                let _ = heap.close_scope(scope);
                return Err(error.into());
            }
        };
        if let Err(error) = heap.close_scope(scope) {
            let _ = heap.leave_function_call(activation);
            return Err(error.into());
        }

        let depth_limit = options.max_call_depth.min(image.max_call_depth);
        let root_frame = match VmFrame::new(heap, image, activation, arguments, 1, depth_limit) {
            Ok(frame) => frame,
            Err(error) => {
                let _ = heap.leave_function_call(activation);
                return Err(error);
            }
        };
        state.max_call_depth_reached = 1;
        let completion = self.run_loop(heap, host, &options, &mut state, vec![root_frame])?;
        Ok(state.report(completion))
    }

    /// Executes without granting host capabilities.
    pub fn execute_without_host(
        &self,
        heap: &mut Heap,
        entry_function: &str,
        arguments: &[Value],
        options: ExecutionOptions,
    ) -> Result<ExecutionReport, InterpreterError> {
        self.execute(heap, entry_function, arguments, &mut NoHost, options)
    }

    fn run_loop<H: Host>(
        &self,
        heap: &mut Heap,
        host: &mut H,
        options: &ExecutionOptions,
        state: &mut ExecutionState,
        mut frames: Vec<VmFrame>,
    ) -> Result<ExecutionCompletion, InterpreterError> {
        loop {
            let Some(mut frame) = frames.pop() else {
                return Err(InterpreterError::InvalidControlFlow(0));
            };
            if frame.entry_poll {
                if let Some(reason) = state.poll(true) {
                    frames.push(frame);
                    self.unwind_frames(heap, &mut frames)?;
                    return Ok(ExecutionCompletion::Interrupted(reason));
                }
                frame.entry_poll = false;
            }
            if state.fuel_remaining == 0 {
                frames.push(frame);
                self.unwind_frames(heap, &mut frames)?;
                return Ok(ExecutionCompletion::Interrupted(ExecutionInterruption::FuelExhausted));
            }

            let instruction = match frame.image.artifact.instructions.get(frame.pc).cloned() {
                Some(instruction) => instruction,
                None => {
                    let offset = frame.image.artifact.code.len() as u32;
                    frames.push(frame);
                    self.unwind_frames(heap, &mut frames)?;
                    return Err(InterpreterError::InvalidControlFlow(offset));
                }
            };
            state.last_location = frame.image.location(instruction.offset);
            let force_poll = operation_needs_interrupt(&instruction.operation)
                || state.instructions_executed == 0
                || state.instructions_executed % u64::from(options.interrupt_poll_interval) == 0;
            if let Some(reason) = state.poll(force_poll) {
                frames.push(frame);
                self.unwind_frames(heap, &mut frames)?;
                return Ok(ExecutionCompletion::Interrupted(reason));
            }

            state.fuel_remaining -= 1;
            state.instructions_executed = state.instructions_executed.saturating_add(1);
            let scope = match heap.open_scope() {
                Ok(scope) => scope,
                Err(error) => {
                    frames.push(frame);
                    self.unwind_frames(heap, &mut frames)?;
                    return Err(error.into());
                }
            };
            if let Err(error) = root_frame_values(heap, scope, &frame) {
                let _ = heap.close_scope(scope);
                frames.push(frame);
                self.unwind_frames(heap, &mut frames)?;
                return Err(error);
            }

            let step = self.dispatch(heap, host, options, state, &mut frame, scope, &instruction);
            match step {
                Ok(Step::Continue { next_pc, back_edge }) => {
                    if let Err(error) = heap.close_scope(scope) {
                        frames.push(frame);
                        self.unwind_frames(heap, &mut frames)?;
                        return Err(error.into());
                    }
                    frame.pc = next_pc;
                    if back_edge && let Some(reason) = state.poll(true) {
                        frames.push(frame);
                        self.unwind_frames(heap, &mut frames)?;
                        return Ok(ExecutionCompletion::Interrupted(reason));
                    }
                    frames.push(frame);
                }
                Ok(Step::PushFrame { child, pending_kind, destination }) => {
                    frame.pc += 1;
                    frame.pending_call = Some(PendingCall { scope, destination, kind: pending_kind });
                    state.max_call_depth_reached = state.max_call_depth_reached.max(child.depth);
                    frames.push(frame);
                    frames.push(child);
                }
                Ok(Step::Complete(completion)) => {
                    if let Err(error) = heap.close_scope(scope) {
                        frames.push(frame);
                        self.unwind_frames(heap, &mut frames)?;
                        return Err(error.into());
                    }
                    if let Err(error) = heap.leave_function_call(frame.activation) {
                        self.unwind_frames(heap, &mut frames)?;
                        return Err(error.into());
                    }
                    match completion {
                        ExecutionCompletion::Return(value) => {
                            let Some(mut caller) = frames.pop() else {
                                return Ok(ExecutionCompletion::Return(value));
                            };
                            let Some(pending) = caller.pending_call.take() else {
                                frames.push(caller);
                                self.unwind_frames(heap, &mut frames)?;
                                return Err(InterpreterError::InvalidControlFlow(instruction.offset));
                            };
                            if let Err(error) = heap.close_scope(pending.scope) {
                                frames.push(caller);
                                self.unwind_frames(heap, &mut frames)?;
                                return Err(error.into());
                            }
                            let result = match pending.kind {
                                PendingKind::Call => value,
                                PendingKind::Construct(instance) => {
                                    if matches!(value.value_type(), ValueType::Object | ValueType::Function) {
                                        value
                                    } else {
                                        instance
                                    }
                                }
                            };
                            if let Err(error) = write_register(
                                &mut caller, pending.destination, RegisterValue::Value(result),
                            ) {
                                frames.push(caller);
                                self.unwind_frames(heap, &mut frames)?;
                                return Err(error);
                            }
                            frames.push(caller);
                        }
                        ExecutionCompletion::Throw(value) => {
                            self.unwind_frames(heap, &mut frames)?;
                            return Ok(ExecutionCompletion::Throw(value));
                        }
                        ExecutionCompletion::Interrupted(reason) => {
                            self.unwind_frames(heap, &mut frames)?;
                            return Ok(ExecutionCompletion::Interrupted(reason));
                        }
                    }
                }
                Err(error) => {
                    let _ = heap.close_scope(scope);
                    frames.push(frame);
                    self.unwind_frames(heap, &mut frames)?;
                    return Err(error);
                }
            }
        }
    }

    fn dispatch<H: Host>(
        &self,
        heap: &mut Heap,
        host: &mut H,
        options: &ExecutionOptions,
        state: &mut ExecutionState,
        frame: &mut VmFrame,
        scope: ScopeToken,
        instruction: &DecodedRegisterInstruction,
    ) -> Result<Step, InterpreterError> {
        let next_pc = frame.pc + 1;
        match &instruction.operation {
            RegisterOperation::Constant { destination, constant, kind } => {
                let value = materialize_constant(heap, &frame.image.artifact, *constant, *kind)?;
                write_register(frame, *destination, value)?;
                Ok(Step::Continue { next_pc, back_edge: false })
            }
            RegisterOperation::Unary { destination, source, operation } => {
                let source_value = read_register(frame, *source)?.clone();
                let result = match operation {
                    UnaryOperation::Copy => source_value,
                    UnaryOperation::ToBoolean => RegisterValue::Value(Value::boolean(
                        heap.to_boolean(read_value_slot(&source_value)?)?,
                    )),
                    UnaryOperation::ToNumber => RegisterValue::Value(Value::number(
                        heap.to_number(read_value_slot(&source_value)?)?,
                    )),
                    UnaryOperation::ToString => RegisterValue::Value(
                        to_js_string(heap, read_value_slot(&source_value)?)?,
                    ),
                    UnaryOperation::TypeOf => {
                        let value = read_value_slot(&source_value)?;
                        let text = match value.value_type() {
                            ValueType::Undefined => "undefined",
                            ValueType::Null | ValueType::Object => "object",
                            ValueType::Boolean => "boolean",
                            ValueType::Number => "number",
                            ValueType::String => "string",
                            ValueType::Function => "function",
                        };
                        RegisterValue::Value(heap.allocate_string(text)?)
                    }
                    UnaryOperation::LoadBinding => match source_value {
                        RegisterValue::Uninitialized => {
                            return Err(InterpreterError::Runtime(HeapError::UninitializedBinding));
                        }
                        RegisterValue::Value(value) => RegisterValue::Value(value),
                        RegisterValue::Cell(_) | RegisterValue::Token { .. } => {
                            return Err(InterpreterError::InvalidRegister(*source));
                        }
                    },
                };
                write_register(frame, *destination, result)?;
                Ok(Step::Continue { next_pc, back_edge: false })
            }
            RegisterOperation::Binary { destination, left, right, operation } => {
                let left = read_value(frame, *left)?;
                let right = read_value(frame, *right)?;
                let result = match operation {
                    BinaryOperation::Add => RegisterValue::Value(js_add(heap, left, right)?),
                    BinaryOperation::StrictEqual => RegisterValue::Value(Value::boolean(
                        heap.strict_equal(left, right)?,
                    )),
                    BinaryOperation::LessThan => RegisterValue::Value(Value::boolean(
                        abstract_relational_compare(heap, left, right, false)?,
                    )),
                    BinaryOperation::GreaterThan => RegisterValue::Value(Value::boolean(
                        abstract_relational_compare(heap, left, right, true)?,
                    )),
                };
                write_register(frame, *destination, result)?;
                Ok(Step::Continue { next_pc, back_edge: false })
            }
            RegisterOperation::Call { destination, kind, sources } => self.dispatch_call(
                heap, host, options, state, frame, scope, *destination, *kind, sources,
                instruction.offset,
            ),
            RegisterOperation::Jump { target } => {
                let target_pc = frame.image.offset_to_index.get(target).copied()
                    .ok_or(InterpreterError::InvalidControlFlow(*target))?;
                Ok(Step::Continue { next_pc: target_pc, back_edge: *target <= instruction.offset })
            }
            RegisterOperation::Branch { condition, then_target, else_target } => {
                let condition = read_value(frame, *condition)?;
                let target = if heap.to_boolean(condition)? { *then_target } else { *else_target };
                let target_pc = frame.image.offset_to_index.get(&target).copied()
                    .ok_or(InterpreterError::InvalidControlFlow(target))?;
                Ok(Step::Continue { next_pc: target_pc, back_edge: target <= instruction.offset })
            }
            RegisterOperation::Return { source } => Ok(Step::Complete(
                ExecutionCompletion::Return(read_value(frame, *source)?),
            )),
            RegisterOperation::Throw { source } => Ok(Step::Complete(
                ExecutionCompletion::Throw(read_value(frame, *source)?),
            )),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_call<H: Host>(
        &self,
        heap: &mut Heap,
        host: &mut H,
        options: &ExecutionOptions,
        state: &mut ExecutionState,
        frame: &mut VmFrame,
        scope: ScopeToken,
        destination: u16,
        kind: CallKind,
        sources: &[u16],
        offset: u32,
    ) -> Result<Step, InterpreterError> {
        match kind {
            CallKind::JavaScript => {
                let callee = read_value(frame, sources[0])?;
                let this_arg = read_value(frame, sources[1])?;
                let arguments = sources[2..].iter()
                    .map(|register| read_value(frame, *register))
                    .collect::<Result<Vec<_>, _>>()?;
                self.prepare_child_call(
                    heap, options, state, frame, scope, destination, callee, this_arg,
                    &arguments, PendingKind::Call, offset,
                )
            }
            CallKind::Construct => {
                let constructor = read_value(frame, sources[0])?;
                let new_target = read_value(frame, sources[1])?;
                let _ = heap.function_id(constructor)?;
                let _ = heap.function_id(new_target)?;
                let instance = heap.allocate_object()?;
                heap.scope_root(scope, instance)?;
                let prototype_key = heap.intern_property_key("prototype")?;
                let prototype = heap.get_property(new_target, prototype_key)?;
                let prototype = if matches!(prototype.value_type(), ValueType::Object | ValueType::Function) {
                    Some(prototype)
                } else {
                    None
                };
                if !heap.set_prototype_of(instance, prototype)? {
                    return Err(InterpreterError::Runtime(HeapError::PrototypeCycle));
                }
                let arguments = sources[2..].iter()
                    .map(|register| read_value(frame, *register))
                    .collect::<Result<Vec<_>, _>>()?;
                self.prepare_child_call(
                    heap, options, state, frame, scope, destination, constructor, instance,
                    &arguments, PendingKind::Construct(instance), offset,
                )
            }
            CallKind::Host => {
                let capability = read_token(frame, sources[0], ConstantKind::HostCapability)?.to_owned();
                let arguments = sources[1..].iter()
                    .map(|register| read_value(frame, *register))
                    .collect::<Result<Vec<_>, _>>()?;
                match host.call(heap, &capability, &arguments) {
                    Ok(value) => {
                        heap.scope_root(scope, value)?;
                        write_register(frame, destination, RegisterValue::Value(value))?;
                        Ok(Step::Continue { next_pc: frame.pc + 1, back_edge: false })
                    }
                    Err(HostError::Throw(value)) => {
                        heap.scope_root(scope, value)?;
                        Ok(Step::Complete(ExecutionCompletion::Throw(value)))
                    }
                    Err(HostError::Denied) => Err(InterpreterError::HostCapabilityDenied(capability)),
                }
            }
            CallKind::GetProperty | CallKind::GetElement => {
                let object = read_value(frame, sources[0])?;
                let key = read_value(frame, sources[1])?;
                let key = heap.to_property_key(key)?;
                let value = heap.get_property(object, key)?;
                heap.scope_root(scope, value)?;
                write_register(frame, destination, RegisterValue::Value(value))?;
                Ok(Step::Continue { next_pc: frame.pc + 1, back_edge: false })
            }
            CallKind::SetProperty | CallKind::SetElement => {
                let object = read_value(frame, sources[0])?;
                let key = read_value(frame, sources[1])?;
                let value = read_value(frame, sources[2])?;
                let key = heap.to_property_key(key)?;
                heap.set_property_or_throw(object, key, value)?;
                write_register(frame, destination, RegisterValue::Value(value))?;
                Ok(Step::Continue { next_pc: frame.pc + 1, back_edge: false })
            }
            CallKind::MakeClosure => {
                let target_id = read_token(frame, sources[0], ConstantKind::FunctionId)?.to_owned();
                let target = self.functions.get(&target_id).cloned()
                    .ok_or_else(|| InterpreterError::MissingFunction(target_id.clone()))?;
                if sources.len() != target.captures.len() + 1 {
                    return Err(InterpreterError::InvalidMetadata(format!(
                        "closure target {target_id:?} expects {} captures, received {}",
                        target.captures.len(), sources.len().saturating_sub(1),
                    )));
                }
                for (register, capture) in sources[1..].iter().zip(target.captures.iter()) {
                    match capture.mode {
                        CaptureMode::Cell => match read_register(frame, *register)? {
                            RegisterValue::Cell(binding) if *binding == capture.source_binding => {}
                            _ => return Err(InterpreterError::InvalidRegister(*register)),
                        },
                        CaptureMode::Value => {
                            let visible_binding = frame.image.register_bindings.get(register)
                                == Some(&capture.source_binding)
                                || frame.image.captures.iter().enumerate().any(|(index, parent_capture)| {
                                    parent_capture.alias_binding == capture.source_binding
                                        && frame.image.artifact.entry_registers
                                            .get(frame.image.parameter_count + index) == Some(register)
                                });
                            if !visible_binding || !matches!(
                                read_register(frame, *register)?, RegisterValue::Value(_)
                            ) {
                                return Err(InterpreterError::InvalidRegister(*register));
                            }
                        }
                    }
                }
                let closure = heap.create_closure(
                    &target.definition,
                    frame.activation.environment(),
                    frame.activation.this_value(),
                )?;
                heap.scope_root(scope, closure)?;
                write_register(frame, destination, RegisterValue::Value(closure))?;
                Ok(Step::Continue { next_pc: frame.pc + 1, back_edge: false })
            }
            CallKind::LoadCapture => {
                let RegisterValue::Cell(binding) = read_register(frame, sources[0])? else {
                    return Err(InterpreterError::InvalidRegister(sources[0]));
                };
                let value = heap.load_binding(frame.activation.environment(), *binding)?;
                heap.scope_root(scope, value)?;
                write_register(frame, destination, RegisterValue::Value(value))?;
                Ok(Step::Continue { next_pc: frame.pc + 1, back_edge: false })
            }
            CallKind::StoreCapture => {
                let RegisterValue::Cell(binding) = read_register(frame, sources[0])? else {
                    return Err(InterpreterError::InvalidRegister(sources[0]));
                };
                let binding = *binding;
                let value = read_value(frame, sources[1])?;
                if frame.image.local_binding_ids.contains(&binding) {
                    if frame.initialized_bindings.contains(&binding) {
                        heap.store_binding(frame.activation.environment(), binding, value)?;
                    } else {
                        heap.initialize_binding(frame.activation.environment(), binding, value)?;
                        frame.initialized_bindings.insert(binding);
                    }
                } else {
                    heap.store_binding(frame.activation.environment(), binding, value)?;
                }
                write_register(frame, destination, RegisterValue::Value(value))?;
                Ok(Step::Continue { next_pc: frame.pc + 1, back_edge: false })
            }
            CallKind::MakeCell => {
                let binding = frame.image.register_bindings.get(&destination).copied()
                    .ok_or(InterpreterError::InvalidRegister(destination))?;
                if !frame.image.local_binding_ids.contains(&binding) {
                    return Err(InterpreterError::InvalidRegister(destination));
                }
                // Runtime has already allocated the traced cell in the activation environment.
                write_register(frame, destination, RegisterValue::Cell(binding))?;
                Ok(Step::Continue { next_pc: frame.pc + 1, back_edge: false })
            }
            CallKind::StoreBinding => {
                let binding_name = read_token(frame, sources[0], ConstantKind::BindingId)?.to_owned();
                let binding_register = frame.image.binding_registers.get(&binding_name).copied()
                    .ok_or_else(|| InterpreterError::InvalidMetadata(format!(
                        "StoreBinding refers to unknown binding {binding_name:?}"
                    )))?;
                let binding = frame.image.register_bindings.get(&binding_register).copied()
                    .ok_or(InterpreterError::InvalidRegister(binding_register))?;
                let value = read_value(frame, sources[1])?;
                if frame.initialized_bindings.contains(&binding) {
                    heap.store_binding(frame.activation.environment(), binding, value)?;
                } else {
                    heap.initialize_binding(frame.activation.environment(), binding, value)?;
                    frame.initialized_bindings.insert(binding);
                }
                write_register(frame, binding_register, RegisterValue::Value(value))?;
                write_register(frame, destination, RegisterValue::Value(value))?;
                Ok(Step::Continue { next_pc: frame.pc + 1, back_edge: false })
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_child_call(
        &self,
        heap: &mut Heap,
        options: &ExecutionOptions,
        state: &mut ExecutionState,
        caller: &VmFrame,
        _scope: ScopeToken,
        destination: u16,
        callee: Value,
        this_argument: Value,
        arguments: &[Value],
        pending_kind: PendingKind,
        offset: u32,
    ) -> Result<Step, InterpreterError> {
        if let Some(reason) = state.poll(true) {
            return Ok(Step::Complete(ExecutionCompletion::Interrupted(reason)));
        }
        let runtime_id = heap.function_id(callee)?.0;
        let function_id = self.runtime_functions.get(&runtime_id).ok_or_else(|| {
            InterpreterError::MissingFunction(format!(
                "Runtime function identity {runtime_id} at byte offset {offset}"
            ))
        })?;
        let image = self.functions.get(function_id).cloned()
            .ok_or_else(|| InterpreterError::MissingFunction(function_id.clone()))?;
        let call_limit = caller.call_depth_limit.min(image.max_call_depth).min(options.max_call_depth);
        if caller.depth >= call_limit {
            return Err(InterpreterError::CallDepthExceeded { limit: call_limit });
        }
        let activation = heap.enter_function_call(
            callee, &image.definition, this_argument, arguments, options.global_this,
        )?;
        let child = match VmFrame::new(heap, image, activation, arguments, caller.depth + 1, call_limit) {
            Ok(frame) => frame,
            Err(error) => {
                let _ = heap.leave_function_call(activation);
                return Err(error);
            }
        };
        Ok(Step::PushFrame { child, pending_kind, destination })
    }

    fn unwind_frames(&self, heap: &mut Heap, frames: &mut Vec<VmFrame>) -> Result<(), InterpreterError> {
        while let Some(mut frame) = frames.pop() {
            if let Some(pending) = frame.pending_call.take() {
                heap.close_scope(pending.scope)?;
            }
            heap.leave_function_call(frame.activation)?;
        }
        Ok(())
    }
}

#[derive(Clone)]
struct RawBinding { id: String, kind: String }

#[derive(Clone)]
struct RawCapture {
    id: String,
    source_environment: String,
    binding: String,
    mode: CaptureMode,
}

#[derive(Clone)]
struct RawFunction {
    artifact: VerifiedArtifact,
    id: String,
    name: String,
    environment_id: String,
    parent_environment: Option<String>,
    parameter_count: usize,
    register_count: usize,
    max_call_depth: u32,
    bindings: Vec<RawBinding>,
    captures: Vec<RawCapture>,
    binding_registers: HashMap<String, u16>,
}

impl RawFunction {
    fn parse(artifact: VerifiedArtifact) -> Result<Self, InterpreterError> {
        let metadata = artifact.manifest.get("metadata")
            .ok_or_else(|| InterpreterError::InvalidMetadata("metadata is missing".to_owned()))?;
        if metadata.get("execution_model").and_then(JsonValue::as_str) != Some("register") {
            return Err(InterpreterError::InvalidMetadata(
                "only the M1 register execution model is supported".to_owned(),
            ));
        }
        let id = required_string(metadata.pointer("/function/id"), "function.id")?.to_owned();
        let name = required_string(metadata.pointer("/function/name"), "function.name")?.to_owned();
        let environment_id = required_string(metadata.pointer("/environment/id"), "environment.id")?.to_owned();
        let parent_environment = metadata.pointer("/environment/parent")
            .and_then(JsonValue::as_str).map(str::to_owned);
        let parameter_count = required_usize(
            metadata.pointer("/frame_layout/parameter_count"), "frame_layout.parameter_count",
        )?;
        let register_count = required_usize(
            metadata.pointer("/frame_layout/register_count"), "frame_layout.register_count",
        )?;
        let max_call_depth = required_u32(
            artifact.manifest.pointer("/resource_limits/max_call_depth"),
            "resource_limits.max_call_depth",
        )?;
        if register_count == 0 || register_count > u16::MAX as usize + 1 {
            return Err(InterpreterError::InvalidMetadata(format!(
                "function {id:?} declares invalid register count {register_count}"
            )));
        }

        let binding_values = metadata.pointer("/environment/bindings")
            .and_then(JsonValue::as_array)
            .ok_or_else(|| InterpreterError::InvalidMetadata(format!(
                "function {id:?} has no environment binding array"
            )))?;
        let mut bindings = Vec::with_capacity(binding_values.len());
        let mut binding_names = HashSet::new();
        for binding in binding_values {
            let binding_id = required_string(binding.get("id"), "environment binding id")?;
            if !binding_names.insert(binding_id.to_owned()) {
                return Err(InterpreterError::InvalidMetadata(format!(
                    "function {id:?} repeats binding {binding_id:?}"
                )));
            }
            let kind = required_string(binding.get("kind"), "environment binding kind")?;
            if !matches!(kind, "var" | "let" | "const" | "parameter") {
                return Err(InterpreterError::UnsupportedOperation("catch/import lexical binding"));
            }
            let mutable = binding.get("mutable").and_then(JsonValue::as_bool)
                .ok_or_else(|| InterpreterError::InvalidMetadata(format!(
                    "binding {binding_id:?} has no mutability metadata"
                )))?;
            if (kind == "const") == mutable {
                return Err(InterpreterError::InvalidMetadata(format!(
                    "binding {binding_id:?} has inconsistent kind/mutability metadata"
                )));
            }
            bindings.push(RawBinding { id: binding_id.to_owned(), kind: kind.to_owned() });
        }

        let capture_values = metadata.pointer("/environment/captures")
            .and_then(JsonValue::as_array)
            .ok_or_else(|| InterpreterError::InvalidMetadata(format!(
                "function {id:?} has no capture array"
            )))?;
        let mut captures = Vec::with_capacity(capture_values.len());
        let mut capture_ids = HashSet::new();
        for capture in capture_values {
            let capture_id = required_string(capture.get("id"), "capture id")?;
            if !capture_ids.insert(capture_id.to_owned()) {
                return Err(InterpreterError::InvalidMetadata(format!(
                    "function {id:?} repeats capture alias {capture_id:?}"
                )));
            }
            let source_environment = required_string(
                capture.get("source_environment"), "capture source environment",
            )?;
            let binding = required_string(capture.get("binding"), "capture source binding")?;
            let mutable = capture.get("mutable").and_then(JsonValue::as_bool)
                .ok_or_else(|| InterpreterError::InvalidMetadata(format!(
                    "capture {capture_id:?} has no mutability metadata"
                )))?;
            let mode = match required_string(capture.get("mode"), "capture mode")? {
                "cell" if mutable => CaptureMode::Cell,
                "value" if !mutable => CaptureMode::Value,
                _ => return Err(InterpreterError::InvalidMetadata(format!(
                    "capture {capture_id:?} has inconsistent mode/mutability metadata"
                ))),
            };
            captures.push(RawCapture {
                id: capture_id.to_owned(),
                source_environment: source_environment.to_owned(),
                binding: binding.to_owned(),
                mode,
            });
        }

        let locations = metadata.pointer("/environment/locations")
            .and_then(JsonValue::as_array)
            .ok_or_else(|| InterpreterError::InvalidMetadata(format!(
                "function {id:?} has no binding location array"
            )))?;
        let mut binding_registers = HashMap::new();
        for location in locations {
            let binding = required_string(location.get("binding"), "binding location id")?;
            if required_string(location.get("kind"), "binding location kind")? != "register" {
                return Err(InterpreterError::UnsupportedOperation("non-register lexical storage"));
            }
            let register = u16::try_from(required_u32(location.get("index"), "binding location index")?)
                .map_err(|_| InterpreterError::InvalidMetadata(format!(
                    "binding {binding:?} register index exceeds encoded width"
                )))?;
            if register as usize >= register_count
                || !binding_names.contains(binding)
                || binding_registers.insert(binding.to_owned(), register).is_some()
            {
                return Err(InterpreterError::InvalidMetadata(format!(
                    "binding {binding:?} has an invalid or duplicate register location"
                )));
            }
        }
        if binding_registers.len() != bindings.len() {
            return Err(InterpreterError::InvalidMetadata(format!(
                "function {id:?} does not assign one register to every lexical binding"
            )));
        }
        if artifact.entry_registers.len() != parameter_count + captures.len() {
            return Err(InterpreterError::InvalidMetadata(format!(
                "function {id:?} has inconsistent parameter/capture entry registers"
            )));
        }

        Ok(Self {
            artifact, id, name, environment_id, parent_environment, parameter_count,
            register_count, max_call_depth, bindings, captures, binding_registers,
        })
    }
}

struct FunctionImage {
    artifact: VerifiedArtifact,
    id: String,
    name: String,
    parameter_count: usize,
    register_count: usize,
    max_call_depth: u32,
    captures: Vec<ImageCapture>,
    binding_registers: HashMap<String, u16>,
    register_bindings: HashMap<u16, BindingId>,
    local_binding_ids: HashSet<BindingId>,
    var_binding_ids: HashSet<BindingId>,
    offset_to_index: HashMap<u32, usize>,
    definition: FunctionDefinition,
}

struct ImageCapture {
    alias_binding: BindingId,
    source_binding: BindingId,
    mode: CaptureMode,
}

impl FunctionImage {
    fn build(
        raw: RawFunction,
        runtime_id: FunctionId,
        binding_ids: &HashMap<(String, String), BindingId>,
        all_functions: &BTreeMap<String, RawFunction>,
    ) -> Result<Self, InterpreterError> {
        let mut locals = Vec::with_capacity(raw.bindings.len());
        let mut register_bindings = HashMap::new();
        let mut local_binding_ids = HashSet::new();
        let mut var_binding_ids = HashSet::new();

        for binding in &raw.bindings {
            let key = (raw.environment_id.clone(), binding.id.clone());
            let id = binding_ids.get(&key).copied()
                .ok_or(InterpreterError::IdentitySpaceExceeded)?;
            local_binding_ids.insert(id);
            let definition = match binding.kind.as_str() {
                "var" => {
                    var_binding_ids.insert(id);
                    BindingDefinition::mutable(id, Value::undefined())
                }
                "let" | "parameter" => BindingDefinition::uninitialized(id, true),
                "const" => BindingDefinition::uninitialized(id, false),
                _ => return Err(InterpreterError::UnsupportedOperation("catch/import binding")),
            };
            locals.push(definition);
            if let Some(register) = raw.binding_registers.get(&binding.id) {
                register_bindings.insert(*register, id);
            }
        }

        let mut captures = Vec::with_capacity(raw.captures.len());
        let mut runtime_captures = Vec::with_capacity(raw.captures.len());
        for capture in &raw.captures {
            let parent_environment = raw.parent_environment.as_deref().ok_or_else(|| {
                InterpreterError::InvalidMetadata(format!(
                    "capturing function {:?} has no parent environment", raw.id
                ))
            })?;
            let source_binding = if capture.source_environment == parent_environment {
                let source_key = (capture.source_environment.clone(), capture.binding.clone());
                binding_ids.get(&source_key).copied()
                    .ok_or(InterpreterError::IdentitySpaceExceeded)?
            } else {
                let parent = all_functions.values()
                    .find(|function| function.environment_id == parent_environment)
                    .ok_or_else(|| InterpreterError::InvalidMetadata(format!(
                        "missing parent function for lexical environment {parent_environment:?}"
                    )))?;
                let forwarded = parent.captures.iter().find(|item| {
                    item.source_environment == capture.source_environment
                        && item.binding == capture.binding
                }).ok_or_else(|| InterpreterError::InvalidMetadata(format!(
                    "parent function {:?} does not import captured binding {:?}",
                    parent.id, capture.binding
                )))?;
                let alias_key = (closure_environment_key(&parent.id), forwarded.id.clone());
                binding_ids.get(&alias_key).copied()
                    .ok_or(InterpreterError::IdentitySpaceExceeded)?
            };
            let alias_key = (closure_environment_key(&raw.id), capture.id.clone());
            let alias_binding = binding_ids.get(&alias_key).copied()
                .ok_or(InterpreterError::IdentitySpaceExceeded)?;
            captures.push(ImageCapture { alias_binding, source_binding, mode: capture.mode });
            runtime_captures.push(CaptureDefinition {
                source: source_binding, alias: alias_binding, mode: capture.mode,
            });
        }

        let definition = FunctionDefinition {
            id: runtime_id,
            parameters: Vec::new(),
            locals,
            captures: runtime_captures,
            // The current OGMB v1 metadata does not yet encode arrow/strict flags.
            kind: FunctionKind::Ordinary,
            strict: true,
        };
        let offset_to_index = raw.artifact.instructions.iter().enumerate()
            .map(|(index, instruction)| (instruction.offset, index))
            .collect();

        Ok(Self {
            artifact: raw.artifact,
            id: raw.id,
            name: raw.name,
            parameter_count: raw.parameter_count,
            register_count: raw.register_count,
            max_call_depth: raw.max_call_depth,
            captures,
            binding_registers: raw.binding_registers,
            register_bindings,
            local_binding_ids,
            var_binding_ids,
            offset_to_index,
            definition,
        })
    }

    fn location(&self, byte_offset: u32) -> Option<InstructionLocation> {
        let metadata = self.artifact.manifest.get("metadata")?;
        let source_map = self.artifact.source_map.as_ref();
        let source = source_map.and_then(|map| map.get("locations"))
            .and_then(JsonValue::as_array)
            .and_then(|locations| locations.iter().find(|item| {
                item.get("offset").and_then(JsonValue::as_u64) == Some(u64::from(byte_offset))
            }))
            .and_then(|item| item.get("source")).cloned();
        let lexical_scopes = source_map.and_then(|map| map.get("lexical_scopes"))
            .and_then(JsonValue::as_array)
            .map(|items| items.iter().filter(|item| range_contains(item, byte_offset))
                .cloned().collect())
            .unwrap_or_default();
        let variable_locations = source_map.and_then(|map| map.get("variable_locations"))
            .and_then(JsonValue::as_array)
            .map(|items| items.iter().filter(|item| range_contains(item, byte_offset))
                .cloned().collect())
            .unwrap_or_default();
        Some(InstructionLocation {
            function_id: self.id.clone(),
            function_name: self.name.clone(),
            byte_offset,
            code_generation_id: metadata.pointer("/code_generation/id")
                .and_then(JsonValue::as_str).map(str::to_owned),
            code_generation_version: metadata.pointer("/code_generation/version")
                .and_then(JsonValue::as_str).map(str::to_owned),
            source, lexical_scopes, variable_locations,
        })
    }
}

fn range_contains(record: &JsonValue, offset: u32) -> bool {
    let start = record.get("start_offset").and_then(JsonValue::as_u64);
    let end = record.get("end_offset").and_then(JsonValue::as_u64);
    matches!((start, end), (Some(start), Some(end)) if start <= u64::from(offset) && u64::from(offset) < end)
}

fn closure_environment_key(function_id: &str) -> String {
    format!("$closure:{function_id}")
}

fn required_string<'a>(
    value: Option<&'a JsonValue>,
    path: &str,
) -> Result<&'a str, InterpreterError> {
    value.and_then(JsonValue::as_str)
        .ok_or_else(|| InterpreterError::InvalidMetadata(format!("{path} must be a string")))
}

fn required_u32(value: Option<&JsonValue>, path: &str) -> Result<u32, InterpreterError> {
    value.and_then(JsonValue::as_u64)
        .and_then(|number| u32::try_from(number).ok())
        .ok_or_else(|| InterpreterError::InvalidMetadata(format!("{path} must be uint32")))
}

fn required_usize(value: Option<&JsonValue>, path: &str) -> Result<usize, InterpreterError> {
    value.and_then(JsonValue::as_u64)
        .and_then(|number| usize::try_from(number).ok())
        .ok_or_else(|| InterpreterError::InvalidMetadata(format!("{path} must be unsigned")))
}

fn validate_options(options: &ExecutionOptions) -> Result<(), InterpreterError> {
    if options.max_call_depth == 0 {
        return Err(InterpreterError::InvalidOptions("max_call_depth must be positive"));
    }
    if options.interrupt_poll_interval == 0 {
        return Err(InterpreterError::InvalidOptions("interrupt_poll_interval must be positive"));
    }
    if options.time_budget.is_some_and(|budget| Instant::now().checked_add(budget).is_none()) {
        return Err(InterpreterError::InvalidOptions("time_budget overflows monotonic clock"));
    }
    Ok(())
}

struct ExecutionState {
    fuel_remaining: u64,
    instructions_executed: u64,
    max_call_depth_reached: u32,
    cancellation: Option<CancellationToken>,
    deadline: Option<Instant>,
    poll_interval: u64,
    last_location: Option<InstructionLocation>,
}

impl ExecutionState {
    fn new(options: &ExecutionOptions) -> Result<Self, InterpreterError> {
        let relative_deadline = match options.time_budget {
            Some(budget) => Some(Instant::now().checked_add(budget).ok_or(
                InterpreterError::InvalidOptions("time_budget overflows monotonic clock"),
            )?),
            None => None,
        };
        let deadline = match (options.deadline, relative_deadline) {
            (Some(explicit), Some(relative)) => Some(explicit.min(relative)),
            (Some(explicit), None) => Some(explicit),
            (None, Some(relative)) => Some(relative),
            (None, None) => None,
        };
        Ok(Self {
            fuel_remaining: options.instruction_budget,
            instructions_executed: 0,
            max_call_depth_reached: 0,
            cancellation: options.cancellation.clone(),
            deadline,
            poll_interval: u64::from(options.interrupt_poll_interval),
            last_location: None,
        })
    }

    fn poll(&self, force: bool) -> Option<ExecutionInterruption> {
        if !force && self.instructions_executed % self.poll_interval != 0 {
            return None;
        }
        if self.cancellation.as_ref().is_some_and(CancellationToken::is_cancelled) {
            return Some(ExecutionInterruption::Cancelled);
        }
        if self.deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Some(ExecutionInterruption::DeadlineExceeded);
        }
        None
    }

    fn report(&self, completion: ExecutionCompletion) -> ExecutionReport {
        ExecutionReport {
            completion,
            instructions_executed: self.instructions_executed,
            fuel_remaining: self.fuel_remaining,
            max_call_depth_reached: self.max_call_depth_reached,
            last_location: self.last_location.clone(),
        }
    }
}

#[derive(Clone)]
enum RegisterValue {
    Uninitialized,
    Value(Value),
    Token { kind: ConstantKind, value: String },
    Cell(BindingId),
}

struct VmFrame {
    image: Rc<FunctionImage>,
    activation: CallActivation,
    registers: Vec<RegisterValue>,
    initialized_bindings: HashSet<BindingId>,
    pc: usize,
    depth: u32,
    call_depth_limit: u32,
    entry_poll: bool,
    pending_call: Option<PendingCall>,
}

impl VmFrame {
    fn new(
        heap: &mut Heap,
        image: Rc<FunctionImage>,
        activation: CallActivation,
        arguments: &[Value],
        depth: u32,
        call_depth_limit: u32,
    ) -> Result<Self, InterpreterError> {
        let mut registers = vec![RegisterValue::Uninitialized; image.register_count];
        for index in 0..image.parameter_count {
            let register = image.artifact.entry_registers.get(index).copied()
                .ok_or_else(|| InterpreterError::InvalidMetadata(
                    "parameter register is missing from entry window".to_owned(),
                ))?;
            write_register_slot(
                &mut registers,
                register,
                RegisterValue::Value(arguments.get(index).copied().unwrap_or_else(Value::undefined)),
            )?;
        }
        for (index, capture) in image.captures.iter().enumerate() {
            let register = image.artifact.entry_registers.get(image.parameter_count + index)
                .copied().ok_or_else(|| InterpreterError::InvalidMetadata(
                    "capture register is missing from entry window".to_owned(),
                ))?;
            let value = match capture.mode {
                CaptureMode::Cell => RegisterValue::Cell(capture.alias_binding),
                CaptureMode::Value => RegisterValue::Value(
                    heap.load_binding(activation.environment(), capture.alias_binding)?,
                ),
            };
            write_register_slot(&mut registers, register, value)?;
        }
        Ok(Self {
            image: image.clone(),
            activation,
            registers,
            initialized_bindings: image.var_binding_ids.clone(),
            pc: 0,
            depth,
            call_depth_limit,
            entry_poll: true,
            pending_call: None,
        })
    }
}

struct PendingCall {
    scope: ScopeToken,
    destination: u16,
    kind: PendingKind,
}

enum PendingKind { Call, Construct(Value) }

enum Step {
    Continue { next_pc: usize, back_edge: bool },
    PushFrame { child: VmFrame, pending_kind: PendingKind, destination: u16 },
    Complete(ExecutionCompletion),
}

fn root_frame_values(
    heap: &mut Heap,
    scope: ScopeToken,
    frame: &VmFrame,
) -> Result<(), InterpreterError> {
    heap.scope_root_environment(scope, frame.activation.environment())?;
    heap.scope_root(scope, frame.activation.this_value())?;
    for value in &frame.registers {
        if let RegisterValue::Value(value) = value {
            heap.scope_root(scope, *value)?;
        }
    }
    Ok(())
}

fn read_register(frame: &VmFrame, register: u16) -> Result<&RegisterValue, InterpreterError> {
    frame.registers.get(register as usize)
        .ok_or(InterpreterError::InvalidRegister(register))
}

fn read_value(frame: &VmFrame, register: u16) -> Result<Value, InterpreterError> {
    read_value_slot(read_register(frame, register)?)
}

fn read_value_slot(value: &RegisterValue) -> Result<Value, InterpreterError> {
    match value {
        RegisterValue::Value(value) => Ok(*value),
        RegisterValue::Uninitialized => Err(InterpreterError::Runtime(HeapError::UninitializedBinding)),
        RegisterValue::Token { .. } | RegisterValue::Cell(_) => Err(InterpreterError::InvalidRegister(u16::MAX)),
    }
}

fn read_token(frame: &VmFrame, register: u16, expected: ConstantKind) -> Result<&str, InterpreterError> {
    match read_register(frame, register)? {
        RegisterValue::Token { kind, value } if *kind == expected => Ok(value),
        _ => Err(InterpreterError::InvalidRegister(register)),
    }
}

fn write_register(frame: &mut VmFrame, register: u16, value: RegisterValue) -> Result<(), InterpreterError> {
    write_register_slot(&mut frame.registers, register, value)
}

fn write_register_slot(
    registers: &mut [RegisterValue],
    register: u16,
    value: RegisterValue,
) -> Result<(), InterpreterError> {
    let slot = registers.get_mut(register as usize)
        .ok_or(InterpreterError::InvalidRegister(register))?;
    *slot = value;
    Ok(())
}

fn materialize_constant(
    heap: &mut Heap,
    artifact: &VerifiedArtifact,
    index: u32,
    kind: ConstantKind,
) -> Result<RegisterValue, InterpreterError> {
    let constant = artifact.constants.get(index as usize)
        .ok_or(InterpreterError::InvalidConstant(index))?;
    match kind {
        ConstantKind::Undefined => Ok(RegisterValue::Value(Value::undefined())),
        ConstantKind::Null => Ok(RegisterValue::Value(Value::null())),
        ConstantKind::Boolean => constant.as_bool()
            .map(|value| RegisterValue::Value(Value::boolean(value)))
            .ok_or_else(|| InterpreterError::InvalidMetadata("Boolean constant must be a JSON boolean".to_owned())),
        ConstantKind::Number => {
            let value = if let Some(number) = constant.as_f64() {
                number
            } else {
                match constant.as_str() {
                    Some("NaN") => f64::NAN,
                    Some("Infinity") => f64::INFINITY,
                    Some("-Infinity") => f64::NEG_INFINITY,
                    Some("-0") => -0.0,
                    _ => return Err(InterpreterError::InvalidMetadata(
                        "Number constant has an invalid binary64 payload".to_owned(),
                    )),
                }
            };
            Ok(RegisterValue::Value(Value::number(value)))
        }
        ConstantKind::String => {
            let text = constant.as_str().ok_or_else(|| InterpreterError::InvalidMetadata(
                "String constant must be a JSON string".to_owned(),
            ))?;
            Ok(RegisterValue::Value(heap.allocate_string(text)?))
        }
        ConstantKind::HostCapability | ConstantKind::FunctionId
        | ConstantKind::BindingId | ConstantKind::Intrinsic => {
            let value = constant.as_str().ok_or_else(|| InterpreterError::InvalidMetadata(
                "compiler token constant must be a JSON string".to_owned(),
            ))?;
            Ok(RegisterValue::Token { kind, value: value.to_owned() })
        }
        ConstantKind::Uninitialized => Ok(RegisterValue::Uninitialized),
        ConstantKind::BigInt => Err(InterpreterError::UnsupportedConstant(kind)),
    }
}

fn operation_needs_interrupt(operation: &RegisterOperation) -> bool {
    match operation {
        RegisterOperation::Constant { kind, .. } => matches!(kind, ConstantKind::String | ConstantKind::BigInt),
        RegisterOperation::Unary { operation, .. } => matches!(operation, UnaryOperation::ToNumber | UnaryOperation::ToString),
        RegisterOperation::Binary { operation, .. } => matches!(
            operation, BinaryOperation::Add | BinaryOperation::LessThan | BinaryOperation::GreaterThan,
        ),
        RegisterOperation::Call { .. } => true,
        RegisterOperation::Jump { .. } | RegisterOperation::Branch { .. }
        | RegisterOperation::Return { .. } | RegisterOperation::Throw { .. } => false,
    }
}

fn js_add(heap: &mut Heap, left: Value, right: Value) -> Result<Value, InterpreterError> {
    if left.value_type() == ValueType::String || right.value_type() == ValueType::String {
        let mut units = to_string_units(heap, left)?;
        units.extend(to_string_units(heap, right)?);
        return Ok(heap.allocate_string_utf16(&units)?);
    }
    Ok(Value::number(heap.to_number(left)? + heap.to_number(right)?))
}

fn abstract_relational_compare(
    heap: &mut Heap,
    left: Value,
    right: Value,
    greater_than: bool,
) -> Result<bool, InterpreterError> {
    let (first, second) = if greater_than { (right, left) } else { (left, right) };
    if first.value_type() == ValueType::String && second.value_type() == ValueType::String {
        return Ok(heap.string_utf16(first)?.to_vec() < heap.string_utf16(second)?.to_vec());
    }
    let first = heap.to_number(first)?;
    let second = heap.to_number(second)?;
    Ok(!first.is_nan() && !second.is_nan() && first < second)
}

fn to_js_string(heap: &mut Heap, value: Value) -> Result<Value, InterpreterError> {
    if value.value_type() == ValueType::String { return Ok(value); }
    let units = to_string_units(heap, value)?;
    Ok(heap.allocate_string_utf16(&units)?)
}

fn to_string_units(heap: &Heap, value: Value) -> Result<Vec<u16>, InterpreterError> {
    let string = match value.value_type() {
        ValueType::Undefined => "undefined".to_owned(),
        ValueType::Null => "null".to_owned(),
        ValueType::Boolean => if value.as_boolean() == Some(true) { "true" } else { "false" }.to_owned(),
        ValueType::Number => format_number(value.as_number().unwrap_or(f64::NAN)),
        ValueType::String => return Ok(heap.string_utf16(value)?.to_vec()),
        ValueType::Object | ValueType::Function => {
            return Err(InterpreterError::Runtime(HeapError::UnsupportedCoercion));
        }
    };
    Ok(string.encode_utf16().collect())
}

fn format_number(number: f64) -> String {
    if number.is_nan() { return "NaN".to_owned(); }
    if number == f64::INFINITY { return "Infinity".to_owned(); }
    if number == f64::NEG_INFINITY { return "-Infinity".to_owned(); }
    if number == 0.0 { return "0".to_owned(); }
    number.to_string()
}
