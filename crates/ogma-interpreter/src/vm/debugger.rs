//! Owner-thread production interpreter adapter for the backend-neutral Debugger API.
//!
//! The controller carries immutable DTOs over a bounded command queue. It never
//! receives Runtime values, VM frames, registers, heap cells or GC handles.

use std::collections::{BTreeMap, HashMap};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc::{self, Receiver, SyncSender},
};
use std::thread::{self, ThreadId};
use std::time::Duration;

use ogma_bytecode::ArtifactJsonValue as JsonValue;
use ogma_runtime::{CompletePropertyDescriptor, Heap, HeapError, HostHandle, Value, ValueType};

use super::{ExecutableModule, FunctionImage, InstructionLocation, RegisterValue, VmFrame};

const COMMAND_QUEUE_CAPACITY: usize = 64;
const MAX_INSPECTION_PROPERTIES: usize = 128;
const MAX_PREVIEW_CHARS: usize = 1024;
const MAX_OBJECT_HANDLES: usize = 4096;
static NEXT_DEBUGGER_SESSION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Eq, PartialEq)]
/// Error returned when a debugger command or inspection operation is unavailable.
pub struct DebuggerError {
    message: String,
}

impl DebuggerError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for DebuggerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for DebuggerError {}

impl From<HeapError> for DebuggerError {
    fn from(error: HeapError) -> Self {
        Self::new(format!("Runtime inspection failed: {error:?}"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Stable identity and version of the loaded executable code generation.
pub struct CodeGeneration {
    /// Describes this debugger snapshot field.
    pub id: String,
    /// Describes this debugger snapshot field.
    pub version: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Source position with one-based line and zero-based UTF-16 column.
pub struct SourcePosition {
    /// Describes this debugger snapshot field.
    pub line: u32,
    /// Describes this debugger snapshot field.
    pub column: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Source span and lexical identity resolved from verified bytecode metadata.
pub struct SourceLocation {
    /// Describes this debugger snapshot field.
    pub origin: String,
    /// Describes this debugger snapshot field.
    pub source_id: Option<String>,
    /// Describes this debugger snapshot field.
    pub source_sha256: Option<String>,
    /// Describes this debugger snapshot field.
    pub start: Option<SourcePosition>,
    /// Describes this debugger snapshot field.
    pub end: Option<SourcePosition>,
    /// Describes this debugger snapshot field.
    pub reason: Option<String>,
    /// Describes this debugger snapshot field.
    pub scope_id: Option<String>,
    /// Describes this debugger snapshot field.
    pub binding_ids: Vec<String>,
    /// Inline call-chain metadata; empty for the register interpreter.
    pub inline_chain: Vec<InlineFrame>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// One inlined frame in a native or optimized source call chain.
pub struct InlineFrame {
    /// Stable inlined function identity.
    pub function: String,
    /// Stable source file identity.
    pub file: String,
    /// Inclusive source start.
    pub start: SourcePosition,
    /// Exclusive source end.
    pub end: SourcePosition,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Source breakpoint and its current verification result.
pub struct Breakpoint {
    /// Describes this debugger snapshot field.
    pub id: String,
    /// Describes this debugger snapshot field.
    pub source_id: String,
    /// Describes this debugger snapshot field.
    pub source_sha256: Option<String>,
    /// Describes this debugger snapshot field.
    pub code_generation: Option<CodeGeneration>,
    /// Describes this debugger snapshot field.
    pub line: u32,
    /// Describes this debugger snapshot field.
    pub column: Option<u32>,
    /// Describes this debugger snapshot field.
    pub verified: bool,
    /// Describes this debugger snapshot field.
    pub message: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Immutable preview of a Runtime value with an optional opaque object reference.
pub struct DebugValue {
    /// Describes this debugger snapshot field.
    pub value_type: String,
    /// Describes this debugger snapshot field.
    pub preview: String,
    /// Describes this debugger snapshot field.
    pub object_id: Option<String>,
    /// Describes this debugger snapshot field.
    pub availability: String,
    /// Describes this debugger snapshot field.
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Read-only snapshot of a local or closure-captured binding.
pub struct VariableSnapshot {
    /// Describes this debugger snapshot field.
    pub binding_id: String,
    /// Describes this debugger snapshot field.
    pub name: String,
    /// Describes this debugger snapshot field.
    pub mutable: bool,
    /// Describes this debugger snapshot field.
    pub availability: String,
    /// Describes this debugger snapshot field.
    pub value: DebugValue,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Lexical scope snapshot at a paused source position.
pub struct ScopeSnapshot {
    /// Describes this debugger snapshot field.
    pub id: String,
    /// Describes this debugger snapshot field.
    pub name: String,
    /// Describes this debugger snapshot field.
    pub parent_id: Option<String>,
    /// Describes this debugger snapshot field.
    pub variables: Vec<VariableSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Immutable view of an active interpreter call frame.
pub struct StackFrameSnapshot {
    /// Describes this debugger snapshot field.
    pub id: String,
    /// Describes this debugger snapshot field.
    pub function_id: String,
    /// Describes this debugger snapshot field.
    pub function_name: String,
    /// Describes this debugger snapshot field.
    pub code_generation: CodeGeneration,
    /// Describes this debugger snapshot field.
    pub location: Option<SourceLocation>,
    /// Describes this debugger snapshot field.
    pub scope_ids: Vec<String>,
    /// Describes this debugger snapshot field.
    pub backend: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Notification that execution stopped for a breakpoint, pause or source step.
pub struct StoppedEvent {
    /// Describes this debugger snapshot field.
    pub reason: String,
    /// Describes this debugger snapshot field.
    pub frame_id: String,
    /// Describes this debugger snapshot field.
    pub location: Option<SourceLocation>,
    /// Describes this debugger snapshot field.
    pub description: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Notification for a thrown value in the supported interpreter subset.
pub struct ExceptionEvent {
    /// Describes this debugger snapshot field.
    pub value: DebugValue,
    /// Describes this debugger snapshot field.
    pub frame_id: String,
    /// Describes this debugger snapshot field.
    pub location: Option<SourceLocation>,
    /// Describes this debugger snapshot field.
    pub caught: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Backend-neutral event emitted by an execution debugger.
pub enum DebuggerEvent {
    /// Execution paused at a breakpoint, explicit pause or source step.
    Stopped(StoppedEvent),
    /// The current execution propagated a thrown value.
    Exception(ExceptionEvent),
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Own data-property descriptor snapshot; accessors are never evaluated.
pub struct InspectedProperty {
    /// Describes this debugger snapshot field.
    pub name: String,
    /// Describes this debugger snapshot field.
    pub value: DebugValue,
    /// Describes this debugger snapshot field.
    pub enumerable: bool,
    /// Describes this debugger snapshot field.
    pub writable: bool,
    /// Describes this debugger snapshot field.
    pub configurable: bool,
    /// Describes this debugger snapshot field.
    pub is_accessor: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Bounded, descriptor-only view of a managed Runtime object.
pub struct InspectedObject {
    /// Describes this debugger snapshot field.
    pub object_id: String,
    /// Describes this debugger snapshot field.
    pub is_proxy: bool,
    /// Describes this debugger snapshot field.
    pub properties: Vec<InspectedProperty>,
    /// Describes this debugger snapshot field.
    pub truncated: bool,
}

#[derive(Clone, Debug)]
struct SourceFile {
    language: String,
    sha256: Option<String>,
    canonical_id: String,
}

#[derive(Clone, Debug)]
struct SourceRecord {
    function_id: String,
    location: SourceLocation,
    code_generation: CodeGeneration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ModuleFunctionIdentity {
    code_generation: CodeGeneration,
    code_cache_id: String,
    code_cache_version: String,
    code_sha256: String,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct SourceKey {
    function_id: String,
    source_id: String,
    line: u32,
    column: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StepMode {
    Into,
    Over,
    Out,
}

#[derive(Clone, Debug)]
struct StepRequest {
    mode: StepMode,
    base_depth: usize,
    base_key: Option<SourceKey>,
}

enum Command {
    Pause,
    SetBreakpoint {
        source_id: String,
        line: u32,
        column: Option<u32>,
        sha256: Option<String>,
        code_generation: Option<CodeGeneration>,
        reply: SyncSender<Result<Breakpoint, DebuggerError>>,
    },
    RemoveBreakpoint {
        id: String,
        reply: SyncSender<Result<bool, DebuggerError>>,
    },
    ListBreakpoints {
        reply: SyncSender<Result<Vec<Breakpoint>, DebuggerError>>,
    },
    Resume {
        reply: SyncSender<Result<(), DebuggerError>>,
    },
    Step {
        mode: StepMode,
        reply: SyncSender<Result<(), DebuggerError>>,
    },
    GetStack {
        reply: SyncSender<Result<Vec<StackFrameSnapshot>, DebuggerError>>,
    },
    GetScopes {
        frame_id: String,
        reply: SyncSender<Result<Vec<ScopeSnapshot>, DebuggerError>>,
    },
    GetVariables {
        frame_id: String,
        scope_id: String,
        reply: SyncSender<Result<Vec<VariableSnapshot>, DebuggerError>>,
    },
    EvaluateIdentifier {
        frame_id: String,
        identifier: String,
        reply: SyncSender<Result<DebugValue, DebuggerError>>,
    },
    InspectObject {
        object_id: String,
        limit: usize,
        reply: SyncSender<Result<InspectedObject, DebuggerError>>,
    },
    EventMessage {
        reply: SyncSender<Result<Option<DebuggerEvent>, DebuggerError>>,
    },
    Detach {
        reply: SyncSender<Result<(), DebuggerError>>,
    },
    #[cfg(test)]
    CollectGarbageForTest {
        reply: SyncSender<Result<(), DebuggerError>>,
    },
}

/// Thread-safe command endpoint. Runtime and VM state never cross this boundary.
#[derive(Clone)]
pub struct DebuggerHandle {
    commands: SyncSender<Command>,
    events: Arc<Mutex<Receiver<DebuggerEvent>>>,
    active: Arc<AtomicBool>,
    owner_thread: ThreadId,
    session_id: String,
}

impl DebuggerHandle {
    /// Stable execution backend name.
    pub fn backend_name(&self) -> &'static str {
        "interpreter"
    }

    /// Capabilities supported by the production interpreter backend.
    pub fn backend_capabilities(&self) -> BTreeMap<String, bool> {
        [
            ("pause", true),
            ("breakpoints", true),
            ("stepInto", true),
            ("stepOver", true),
            ("stepOut", true),
            ("callStack", true),
            ("scopes", true),
            ("safeInspection", self.active.load(Ordering::Acquire)),
            ("exceptionEvents", true),
        ]
        .into_iter()
        .map(|(name, enabled)| (name.to_owned(), enabled))
        .collect()
    }

    /// Unique identity of this execution session.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    fn ensure_remote_thread(&self) -> Result<(), DebuggerError> {
        if thread::current().id() == self.owner_thread {
            return Err(DebuggerError::new(
                "DebuggerHandle must be used from a control thread, not the Runtime owner thread",
            ));
        }
        if !self.active.load(Ordering::Acquire) {
            return Err(DebuggerError::new(
                "debugger execution session is not active",
            ));
        }
        Ok(())
    }

    fn request<T>(
        &self,
        create: impl FnOnce(SyncSender<Result<T, DebuggerError>>) -> Command,
    ) -> Result<T, DebuggerError> {
        self.ensure_remote_thread()?;
        let (reply, receiver) = mpsc::sync_channel(1);
        self.commands.send(create(reply)).map_err(|_| {
            DebuggerError::new("debugger owner thread is no longer servicing commands")
        })?;
        receiver
            .recv()
            .map_err(|_| DebuggerError::new("debugger owner thread dropped a command response"))?
    }

    /// Requests a pause; it is applied at the next interpreter dispatch safe point.
    pub fn request_pause(&self) -> Result<(), DebuggerError> {
        self.ensure_remote_thread()?;
        self.commands.send(Command::Pause).map_err(|_| {
            DebuggerError::new("debugger owner thread is no longer servicing commands")
        })
    }

    /// Performs this debugger operation on the owning execution.
    pub fn set_breakpoint(
        &self,
        source_id: impl Into<String>,
        line: u32,
        column: Option<u32>,
        sha256: Option<String>,
        code_generation: Option<CodeGeneration>,
    ) -> Result<Breakpoint, DebuggerError> {
        let source_id = source_id.into();
        self.request(|reply| Command::SetBreakpoint {
            source_id,
            line,
            column,
            sha256,
            code_generation,
            reply,
        })
    }

    /// Performs this debugger operation on the owning execution.
    pub fn remove_breakpoint(&self, id: impl Into<String>) -> Result<bool, DebuggerError> {
        let id = id.into();
        self.request(|reply| Command::RemoveBreakpoint { id, reply })
    }

    /// Performs this debugger operation on the owning execution.
    pub fn list_breakpoints(&self) -> Result<Vec<Breakpoint>, DebuggerError> {
        self.request(|reply| Command::ListBreakpoints { reply })
    }

    /// Performs this debugger operation on the owning execution.
    pub fn resume(&self) -> Result<(), DebuggerError> {
        self.request(|reply| Command::Resume { reply })
    }

    /// Performs this debugger operation on the owning execution.
    pub fn step_into(&self) -> Result<(), DebuggerError> {
        self.request(|reply| Command::Step {
            mode: StepMode::Into,
            reply,
        })
    }

    /// Performs this debugger operation on the owning execution.
    pub fn step_over(&self) -> Result<(), DebuggerError> {
        self.request(|reply| Command::Step {
            mode: StepMode::Over,
            reply,
        })
    }

    /// Performs this debugger operation on the owning execution.
    pub fn step_out(&self) -> Result<(), DebuggerError> {
        self.request(|reply| Command::Step {
            mode: StepMode::Out,
            reply,
        })
    }

    /// Performs this debugger operation on the owning execution.
    pub fn get_stack(&self) -> Result<Vec<StackFrameSnapshot>, DebuggerError> {
        self.request(|reply| Command::GetStack { reply })
    }

    /// Performs this debugger operation on the owning execution.
    pub fn get_scopes(
        &self,
        frame_id: impl Into<String>,
    ) -> Result<Vec<ScopeSnapshot>, DebuggerError> {
        let frame_id = frame_id.into();
        self.request(|reply| Command::GetScopes { frame_id, reply })
    }

    /// Performs this debugger operation on the owning execution.
    pub fn get_variables(
        &self,
        frame_id: impl Into<String>,
        scope_id: impl Into<String>,
    ) -> Result<Vec<VariableSnapshot>, DebuggerError> {
        let frame_id = frame_id.into();
        let scope_id = scope_id.into();
        self.request(|reply| Command::GetVariables {
            frame_id,
            scope_id,
            reply,
        })
    }

    /// Evaluates a single identifier or binding ID. Expressions and user code are never executed.
    pub fn evaluate_identifier(
        &self,
        frame_id: impl Into<String>,
        identifier: impl Into<String>,
    ) -> Result<DebugValue, DebuggerError> {
        let frame_id = frame_id.into();
        let identifier = identifier.into();
        self.request(|reply| Command::EvaluateIdentifier {
            frame_id,
            identifier,
            reply,
        })
    }

    /// Performs this debugger operation on the owning execution.
    pub fn inspect_object(
        &self,
        object_id: impl Into<String>,
        limit: usize,
    ) -> Result<InspectedObject, DebuggerError> {
        let object_id = object_id.into();
        self.request(|reply| Command::InspectObject {
            object_id,
            limit,
            reply,
        })
    }

    /// Performs this debugger operation on the owning execution.
    pub fn event_message(&self) -> Result<Option<DebuggerEvent>, DebuggerError> {
        self.request(|reply| Command::EventMessage { reply })
    }

    /// Performs this debugger operation on the owning execution.
    pub fn detach(&self) -> Result<(), DebuggerError> {
        self.request(|reply| Command::Detach { reply })
    }

    #[cfg(test)]
    fn collect_garbage_for_test(&self) -> Result<(), DebuggerError> {
        self.request(|reply| Command::CollectGarbageForTest { reply })
    }

    /// Waits for the next stop/exception notification without touching Runtime state.
    pub fn recv_event_timeout(
        &self,
        timeout: Duration,
    ) -> Result<Option<DebuggerEvent>, DebuggerError> {
        let receiver = self
            .events
            .lock()
            .map_err(|_| DebuggerError::new("debugger event lock is poisoned"))?;
        match receiver.recv_timeout(timeout) {
            Ok(event) => Ok(Some(event)),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(DebuggerError::new("debugger event stream is closed"))
            }
        }
    }
}

/// Backend-neutral debugger command port implemented by execution backends.
///
/// No Runtime values, frames, registers, heap layout or mutable objects cross
/// this boundary; control clients receive immutable snapshots and opaque IDs.
pub trait DebuggerPort: Send + Sync {
    /// Returns the backend identifier.
    fn backend_name(&self) -> &'static str;
    /// Returns the capability map used by tooling for feature discovery.
    fn backend_capabilities(&self) -> BTreeMap<String, bool>;
    /// Returns the unique execution session identity.
    fn session_id(&self) -> &str;
    /// Adds a source breakpoint.
    fn set_breakpoint(
        &self,
        source_id: String,
        line: u32,
        column: Option<u32>,
        sha256: Option<String>,
        code_generation: Option<CodeGeneration>,
    ) -> Result<Breakpoint, DebuggerError>;
    /// Removes a source breakpoint.
    fn remove_breakpoint(&self, id: String) -> Result<bool, DebuggerError>;
    /// Lists source breakpoints.
    fn list_breakpoints(&self) -> Result<Vec<Breakpoint>, DebuggerError>;
    /// Requests a cooperative pause.
    fn request_pause(&self) -> Result<(), DebuggerError>;
    /// Resumes execution.
    fn resume(&self) -> Result<(), DebuggerError>;
    /// Steps into the next source-level location.
    fn step_into(&self) -> Result<(), DebuggerError>;
    /// Steps over the next source-level location.
    fn step_over(&self) -> Result<(), DebuggerError>;
    /// Steps out of the current function.
    fn step_out(&self) -> Result<(), DebuggerError>;
    /// Returns immutable active call-stack snapshots.
    fn get_stack(&self) -> Result<Vec<StackFrameSnapshot>, DebuggerError>;
    /// Returns lexical scopes in one paused frame.
    fn get_scopes(&self, frame_id: String) -> Result<Vec<ScopeSnapshot>, DebuggerError>;
    /// Returns variables from one paused lexical scope.
    fn get_variables(
        &self,
        frame_id: String,
        scope_id: String,
    ) -> Result<Vec<VariableSnapshot>, DebuggerError>;
    /// Resolves a single identifier or binding ID without evaluating user code.
    fn evaluate_identifier(
        &self,
        frame_id: String,
        identifier: String,
    ) -> Result<DebugValue, DebuggerError>;
    /// Inspects bounded own data-property descriptors.
    fn inspect_object(
        &self,
        object_id: String,
        limit: usize,
    ) -> Result<InspectedObject, DebuggerError>;
    /// Returns the latest stop or exception event.
    fn event_message(&self) -> Result<Option<DebuggerEvent>, DebuggerError>;
    /// Detaches this controller and releases retained inspection handles.
    fn detach(&self) -> Result<(), DebuggerError>;
}

impl DebuggerPort for DebuggerHandle {
    fn backend_name(&self) -> &'static str {
        DebuggerHandle::backend_name(self)
    }
    fn backend_capabilities(&self) -> BTreeMap<String, bool> {
        DebuggerHandle::backend_capabilities(self)
    }
    fn session_id(&self) -> &str {
        DebuggerHandle::session_id(self)
    }
    fn set_breakpoint(
        &self,
        source_id: String,
        line: u32,
        column: Option<u32>,
        sha256: Option<String>,
        code_generation: Option<CodeGeneration>,
    ) -> Result<Breakpoint, DebuggerError> {
        DebuggerHandle::set_breakpoint(self, source_id, line, column, sha256, code_generation)
    }
    fn remove_breakpoint(&self, id: String) -> Result<bool, DebuggerError> {
        DebuggerHandle::remove_breakpoint(self, id)
    }
    fn list_breakpoints(&self) -> Result<Vec<Breakpoint>, DebuggerError> {
        DebuggerHandle::list_breakpoints(self)
    }
    fn request_pause(&self) -> Result<(), DebuggerError> {
        DebuggerHandle::request_pause(self)
    }
    fn resume(&self) -> Result<(), DebuggerError> {
        DebuggerHandle::resume(self)
    }
    fn step_into(&self) -> Result<(), DebuggerError> {
        DebuggerHandle::step_into(self)
    }
    fn step_over(&self) -> Result<(), DebuggerError> {
        DebuggerHandle::step_over(self)
    }
    fn step_out(&self) -> Result<(), DebuggerError> {
        DebuggerHandle::step_out(self)
    }
    fn get_stack(&self) -> Result<Vec<StackFrameSnapshot>, DebuggerError> {
        DebuggerHandle::get_stack(self)
    }
    fn get_scopes(&self, frame_id: String) -> Result<Vec<ScopeSnapshot>, DebuggerError> {
        DebuggerHandle::get_scopes(self, frame_id)
    }
    fn get_variables(
        &self,
        frame_id: String,
        scope_id: String,
    ) -> Result<Vec<VariableSnapshot>, DebuggerError> {
        DebuggerHandle::get_variables(self, frame_id, scope_id)
    }
    fn evaluate_identifier(
        &self,
        frame_id: String,
        identifier: String,
    ) -> Result<DebugValue, DebuggerError> {
        DebuggerHandle::evaluate_identifier(self, frame_id, identifier)
    }
    fn inspect_object(
        &self,
        object_id: String,
        limit: usize,
    ) -> Result<InspectedObject, DebuggerError> {
        DebuggerHandle::inspect_object(self, object_id, limit)
    }
    fn event_message(&self) -> Result<Option<DebuggerEvent>, DebuggerError> {
        DebuggerHandle::event_message(self)
    }
    fn detach(&self) -> Result<(), DebuggerError> {
        DebuggerHandle::detach(self)
    }
}

/// Owner-thread adapter state. Create it on the same thread that owns the Runtime.
pub struct DebuggerSession {
    owner_thread: ThreadId,
    commands: Option<Receiver<Command>>,
    event_sender: mpsc::Sender<DebuggerEvent>,
    active: Arc<AtomicBool>,
    state: SessionState,
    breakpoints: BTreeMap<String, Breakpoint>,
    next_breakpoint_id: u64,
    source_files: HashMap<(String, String), SourceFile>,
    source_records: Vec<SourceRecord>,
    module_generations: HashMap<String, ModuleFunctionIdentity>,
    pause_requested: bool,
    step: Option<StepRequest>,

    last_instruction: HashMap<u64, (usize, Option<SourceKey>)>,
    event: Option<DebuggerEvent>,
    object_handles: HashMap<String, HostHandle>,
    next_object_id: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionState {
    Created,
    Running,
    Paused,
    Detached,
    Finished,
}

impl DebuggerSession {
    /// Creates a debugger session preloaded with the module's verified source map.
    pub fn new(module: &ExecutableModule) -> Result<(Self, DebuggerHandle), DebuggerError> {
        let (commands, receiver) = mpsc::sync_channel(COMMAND_QUEUE_CAPACITY);
        let (event_sender, event_receiver) = mpsc::channel();
        let owner_thread = thread::current().id();
        let mut session = Self {
            owner_thread,
            commands: Some(receiver),
            event_sender,
            active: Arc::new(AtomicBool::new(false)),
            state: SessionState::Created,
            breakpoints: BTreeMap::new(),
            next_breakpoint_id: 1,
            source_files: HashMap::new(),
            source_records: Vec::new(),
            module_generations: HashMap::new(),
            pause_requested: false,
            step: None,

            last_instruction: HashMap::new(),
            event: None,
            object_handles: HashMap::new(),
            next_object_id: 1,
        };
        session.index_module(module)?;
        let handle = DebuggerHandle {
            commands,
            events: Arc::new(Mutex::new(event_receiver)),
            active: session.active.clone(),
            owner_thread,
            session_id: format!(
                "debug-{}",
                NEXT_DEBUGGER_SESSION_ID.fetch_add(1, Ordering::Relaxed)
            ),
        };
        Ok((session, handle))
    }

    /// Verifies that a session is attached to the exact code generations it indexed.
    pub(super) fn validate_module(&self, module: &ExecutableModule) -> Result<(), DebuggerError> {
        self.ensure_owner()?;
        if module.functions.len() != self.module_generations.len() {
            return Err(DebuggerError::new(
                "debugger session belongs to a different executable module",
            ));
        }
        for image in module.functions.values() {
            let identity = module_function_identity(image).ok_or_else(|| {
                DebuggerError::new(format!(
                    "function {:?} has incomplete or invalid code-generation/cache identity",
                    image.id
                ))
            })?;
            if self.module_generations.get(&image.id) != Some(&identity) {
                return Err(DebuggerError::new(format!(
                    "debugger session artifact identity does not match function {:?}",
                    image.id
                )));
            }
        }
        Ok(())
    }

    fn ensure_owner(&self) -> Result<(), DebuggerError> {
        if thread::current().id() != self.owner_thread {
            return Err(DebuggerError::new(
                "debugger session access must run on the Runtime owner thread",
            ));
        }
        Ok(())
    }

    fn index_module(&mut self, module: &ExecutableModule) -> Result<(), DebuggerError> {
        // Core IR source-file IDs are local to a function. Index them with their
        // function identity first, then derive a session-wide SourceId only when
        // the same local ID is reused for different source contents.
        let mut source_groups: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for image in module.functions.values() {
            if !valid_api_id(&image.id) {
                return Err(DebuggerError::new(format!(
                    "function identity {:?} does not satisfy the Debugger API v1 id contract",
                    image.id
                )));
            }
            if let Some(files) = image
                .artifact
                .source_map
                .as_ref()
                .and_then(|map| map.get("files"))
                .and_then(JsonValue::as_array)
            {
                for file in files {
                    let Some(id) = file.get("id").and_then(JsonValue::as_str) else {
                        return Err(DebuggerError::new(
                            "source file metadata is missing its local id",
                        ));
                    };
                    let _path = file
                        .get("path")
                        .and_then(JsonValue::as_str)
                        .filter(|value| !value.is_empty() && value.chars().count() <= 4096)
                        .ok_or_else(|| {
                            DebuggerError::new(format!("source file {id:?} has an invalid path"))
                        })?;
                    let language = file
                        .get("language")
                        .and_then(JsonValue::as_str)
                        .filter(|value| {
                            matches!(*value, "javascript" | "typescript" | "json" | "synthetic")
                        })
                        .ok_or_else(|| {
                            DebuggerError::new(format!(
                                "source file {id:?} has an invalid language"
                            ))
                        })?;
                    if !valid_api_id(id) {
                        return Err(DebuggerError::new(format!(
                            "source file id {id:?} does not satisfy the Debugger API v1 id contract"
                        )));
                    }
                    let sha256 = file
                        .get("sha256")
                        .and_then(JsonValue::as_str)
                        .map(str::to_owned);
                    if sha256.as_deref().is_some_and(|digest| {
                        digest.len() != 64
                            || !digest
                                .bytes()
                                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    }) {
                        return Err(DebuggerError::new(format!(
                            "source file {id:?} has an invalid SHA-256 digest"
                        )));
                    }
                    let key = (image.id.clone(), id.to_owned());
                    if self
                        .source_files
                        .insert(
                            key.clone(),
                            SourceFile {
                                language: language.to_owned(),
                                sha256,
                                canonical_id: id.to_owned(),
                            },
                        )
                        .is_some()
                    {
                        return Err(DebuggerError::new(format!(
                            "function {:?} contains duplicate local source file id {id:?}",
                            image.id
                        )));
                    }
                    source_groups.entry(id.to_owned()).or_default().push(key);
                }
            }
        }

        for (local_id, entries) in source_groups {
            let Some(first_key) = entries.first() else {
                continue;
            };
            let first = self
                .source_files
                .get(first_key)
                .ok_or_else(|| DebuggerError::new("indexed source file disappeared"))?;
            let collides = entries.iter().skip(1).any(|key| {
                let Some(candidate) = self.source_files.get(key) else {
                    return true;
                };
                if candidate.language != first.language {
                    return true;
                }
                match (&candidate.sha256, &first.sha256) {
                    (Some(left), Some(right)) => left != right,
                    _ => true,
                }
            });
            if collides {
                for key in entries {
                    let canonical_id = qualified_source_id(&key.0, &local_id);
                    let file = self
                        .source_files
                        .get_mut(&key)
                        .ok_or_else(|| DebuggerError::new("indexed source file disappeared"))?;
                    file.canonical_id = canonical_id;
                }
            }
        }
        for image in module.functions.values() {
            let identity = module_function_identity(image).ok_or_else(|| {
                DebuggerError::new(format!(
                    "function {:?} has incomplete or invalid code-generation/cache identity",
                    image.id
                ))
            })?;
            let code_generation = identity.code_generation.clone();
            self.module_generations.insert(image.id.clone(), identity);
            for instruction in &image.artifact.instructions {
                if let Some(location) = image.location(instruction.offset) {
                    let source = self.parse_source_location(&image.id, location.source.as_ref());
                    if let Some(source) =
                        source.filter(|item| item.origin == "user" && item.source_id.is_some())
                    {
                        self.source_records.push(SourceRecord {
                            function_id: image.id.clone(),
                            location: source,
                            code_generation: code_generation.clone(),
                        });
                    }
                }
            }
        }
        Ok(())
    }

    fn parse_source_location(
        &self,
        function_id: &str,
        value: Option<&JsonValue>,
    ) -> Option<SourceLocation> {
        let value = value?;
        let origin = value.get("origin")?.as_str()?.to_owned();
        let local_source_id = value
            .get("file")
            .and_then(JsonValue::as_str)
            .map(str::to_owned);
        let source_file = local_source_id
            .as_ref()
            .and_then(|id| self.source_files.get(&(function_id.to_owned(), id.clone())));
        let source_id = source_file.map(|file| file.canonical_id.clone());
        let start = parse_position(value.get("start"));
        let end = parse_position(value.get("end"));
        let reason = value
            .get("reason")
            .and_then(JsonValue::as_str)
            .map(str::to_owned);
        match origin.as_str() {
            "user"
                if source_id.as_deref().is_some_and(valid_api_id)
                    && start.is_some()
                    && end.is_some() => {}
            "generated"
                if reason
                    .as_deref()
                    .is_some_and(|text| !text.is_empty() && text.chars().count() <= 512) => {}
            _ => return None,
        }
        let source_sha256 = source_file.and_then(|file| file.sha256.clone());
        let scope_id = value
            .get("scope")
            .and_then(JsonValue::as_str)
            .filter(|id| valid_api_id(id))
            .map(str::to_owned);
        let mut binding_ids = value
            .get("bindings")
            .and_then(JsonValue::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(JsonValue::as_str)
                    .filter(|id| valid_api_id(id))
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        binding_ids.sort();
        binding_ids.dedup();
        let inline_chain = value
            .get("inline_chain")
            .and_then(JsonValue::as_array)
            .map(|items| {
                items
                    .iter()
                    .take(64)
                    .filter_map(|item| {
                        let function = item.get("function").and_then(JsonValue::as_str)?;
                        let local_file = item.get("file").and_then(JsonValue::as_str)?;
                        if !valid_api_id(function) {
                            return None;
                        }
                        let file = self
                            .source_files
                            .get(&(function_id.to_owned(), local_file.to_owned()))?
                            .canonical_id
                            .clone();
                        Some(InlineFrame {
                            function: function.to_owned(),
                            file,
                            start: parse_position(item.get("start"))?,
                            end: parse_position(item.get("end"))?,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Some(SourceLocation {
            origin,
            source_id,
            source_sha256,
            start,
            end,
            reason,
            scope_id,
            binding_ids,
            inline_chain,
        })
    }

    /// Registers a breakpoint before or during execution on the owner thread.
    pub fn set_breakpoint(
        &mut self,
        source_id: impl Into<String>,
        line: u32,
        column: Option<u32>,
        sha256: Option<String>,
        generation: Option<CodeGeneration>,
    ) -> Result<Breakpoint, DebuggerError> {
        self.ensure_owner()?;
        if self.state != SessionState::Created {
            return Err(DebuggerError::new(
                "direct breakpoint changes are only available before execution; use DebuggerHandle while running",
            ));
        }
        self.create_breakpoint(source_id.into(), line, column, sha256, generation)
    }

    /// Performs this debugger operation on the owning execution.
    pub fn remove_breakpoint(&mut self, id: &str) -> Result<bool, DebuggerError> {
        self.ensure_owner()?;
        if self.state != SessionState::Created {
            return Err(DebuggerError::new(
                "direct breakpoint changes are only available before execution; use DebuggerHandle while running",
            ));
        }
        Ok(self.breakpoints.remove(id).is_some())
    }

    /// Performs this debugger operation on the owning execution.
    pub fn list_breakpoints(&self) -> Result<Vec<Breakpoint>, DebuggerError> {
        self.ensure_owner()?;
        Ok(self.breakpoints.values().cloned().collect())
    }

    /// Requests a cooperative pause from the owner thread before execution starts.
    pub fn request_pause(&mut self) -> Result<(), DebuggerError> {
        self.ensure_owner()?;
        if self.state != SessionState::Created {
            return Err(DebuggerError::new(
                "use DebuggerHandle to request a pause during execution",
            ));
        }
        self.pause_requested = true;
        Ok(())
    }

    fn create_breakpoint(
        &mut self,
        source_id: String,
        line: u32,
        column: Option<u32>,
        requested_sha: Option<String>,
        requested_generation: Option<CodeGeneration>,
    ) -> Result<Breakpoint, DebuggerError> {
        if !valid_api_id(&source_id) {
            return Err(DebuggerError::new(
                "source id does not satisfy the Debugger API v1 id contract",
            ));
        }
        if line == 0 {
            return Err(DebuggerError::new(
                "source line numbers are one-based and must be positive",
            ));
        }
        let source_file = self
            .source_files
            .values()
            .find(|file| file.canonical_id == source_id);
        let mut verified = source_file.is_some();
        let mut message = if verified {
            None
        } else {
            Some("unknown source file".to_owned())
        };
        if let Some(digest) = requested_sha.as_deref() {
            let valid = digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
            if !valid {
                verified = false;
                message = Some("source digest must be lowercase SHA-256 hex".to_owned());
            } else if source_file.and_then(|file| file.sha256.as_deref()) != Some(digest) {
                verified = false;
                message = Some("source digest does not match loaded source".to_owned());
            }
        }

        let matching = self.source_records.iter().find(|record| {
            let location = &record.location;
            location.origin == "user"
                && location.source_id.as_deref() == Some(source_id.as_str())
                && location.start.is_some_and(|start| {
                    start.line == line && column.is_none_or(|value| value == start.column)
                })
                && requested_generation
                    .as_ref()
                    .is_none_or(|wanted| &record.code_generation == wanted)
        });
        if matching.is_none() && verified {
            verified = false;
            message = Some(if requested_generation.is_some() {
                "no executable source location matches this line/column and code generation"
                    .to_owned()
            } else {
                "no executable source location matches this line/column".to_owned()
            });
        }

        let generation =
            requested_generation.or_else(|| matching.map(|item| item.code_generation.clone()));
        let id = format!("bp-{}", self.next_breakpoint_id);
        self.next_breakpoint_id = self
            .next_breakpoint_id
            .checked_add(1)
            .ok_or_else(|| DebuggerError::new("breakpoint identity space exhausted"))?;
        let breakpoint = Breakpoint {
            id: id.clone(),
            source_id,
            source_sha256: requested_sha
                .or_else(|| source_file.and_then(|file| file.sha256.clone())),
            code_generation: generation,
            line,
            column,
            verified,
            message,
        };
        self.breakpoints.insert(id, breakpoint.clone());
        Ok(breakpoint)
    }

    pub(super) fn activate(&mut self) -> Result<(), DebuggerError> {
        self.ensure_owner()?;
        if self.state != SessionState::Created {
            return Err(DebuggerError::new(
                "debugger session can only be executed once",
            ));
        }
        self.state = SessionState::Running;
        self.active.store(true, Ordering::Release);
        Ok(())
    }

    pub(super) fn finish(&mut self, heap: &mut Heap) -> Result<(), DebuggerError> {
        let cleanup = self.release_object_handles(heap);
        self.state = SessionState::Finished;
        self.active.store(false, Ordering::Release);
        self.reject_pending_commands();
        cleanup
    }

    fn reject_pending_commands(&mut self) {
        while let Some(command) = self
            .commands
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
        {
            let error = DebuggerError::new("debugger execution session has finished");
            match command {
                Command::Pause => {}
                Command::SetBreakpoint { reply, .. } => {
                    let _ = reply.send(Err(error));
                }
                Command::RemoveBreakpoint { reply, .. } => {
                    let _ = reply.send(Err(error));
                }
                Command::ListBreakpoints { reply } => {
                    let _ = reply.send(Err(error));
                }
                Command::Resume { reply } => {
                    let _ = reply.send(Err(error));
                }
                Command::Step { reply, .. } => {
                    let _ = reply.send(Err(error));
                }
                Command::GetStack { reply } => {
                    let _ = reply.send(Err(error));
                }
                Command::GetScopes { reply, .. } => {
                    let _ = reply.send(Err(error));
                }
                Command::GetVariables { reply, .. } => {
                    let _ = reply.send(Err(error));
                }
                Command::EvaluateIdentifier { reply, .. } => {
                    let _ = reply.send(Err(error));
                }
                Command::InspectObject { reply, .. } => {
                    let _ = reply.send(Err(error));
                }
                Command::EventMessage { reply } => {
                    let _ = reply.send(Err(error));
                }
                Command::Detach { reply } => {
                    let _ = reply.send(Err(error));
                }
                #[cfg(test)]
                Command::CollectGarbageForTest { reply } => {
                    let _ = reply.send(Err(error));
                }
            }
        }
        // Closing the receiver prevents a request that raced with shutdown from
        // being queued after the final drain and waiting forever for a reply.
        self.commands.take();
    }

    fn release_object_handles(&mut self, heap: &mut Heap) -> Result<(), DebuggerError> {
        let mut first_error = None;
        for (_, handle) in self.object_handles.drain() {
            if let Err(error) = heap.release_host_handle(handle) {
                first_error.get_or_insert(error);
            }
        }
        if let Some(error) = first_error {
            Err(error.into())
        } else {
            Ok(())
        }
    }

    pub(super) fn before_instruction(
        &mut self,
        heap: &mut Heap,
        callers: &[VmFrame],
        current: &VmFrame,
        current_location: Option<&InstructionLocation>,
    ) -> Result<(), DebuggerError> {
        self.ensure_owner()?;
        while let Some(command) = self
            .commands
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
        {
            self.process_command(heap, callers, current, current_location, command)?;
        }
        if self.state == SessionState::Detached {
            return Ok(());
        }

        let location = current_location
            .and_then(|item| self.parse_source_location(&current.image.id, item.source.as_ref()));
        let key = location
            .as_ref()
            .and_then(|item| source_key(&current.image.id, item));
        let previous = self
            .last_instruction
            .insert(current.id, (current.pc, key.clone()));
        let changed = previous
            .as_ref()
            .is_none_or(|(_, previous_key)| *previous_key != key);
        let loop_reentry = previous
            .as_ref()
            .is_some_and(|(previous_pc, _)| current.pc <= *previous_pc);
        let stop_reason = if self.pause_requested {
            self.pause_requested = false;
            Some(("pause", None))
        } else if (changed || loop_reentry)
            && self.matches_breakpoint(&current.image.id, location.as_ref())
        {
            Some(("breakpoint", None))
        } else if let Some(step) = self.step.as_ref() {
            let depth = callers.len() + 1;
            let source_changed = key.is_some() && key != step.base_key;
            let should_stop = match step.mode {
                StepMode::Into => source_changed,
                StepMode::Over => depth <= step.base_depth && source_changed,
                StepMode::Out => depth < step.base_depth && source_changed,
            };
            if should_stop {
                Some(("step", None))
            } else {
                None
            }
        } else {
            None
        };

        if let Some((reason, description)) = stop_reason {
            self.step = None;
            let event = StoppedEvent {
                reason: reason.to_owned(),
                frame_id: frame_id(current),
                location,
                description,
            };
            self.stop_with_event(DebuggerEvent::Stopped(event));
        }
        self.wait_while_paused(heap, callers, current, current_location)
    }

    pub(super) fn on_exception(
        &mut self,
        heap: &mut Heap,
        callers: &[VmFrame],
        current: &VmFrame,
        current_location: Option<&InstructionLocation>,
        value: Value,
    ) -> Result<(), DebuggerError> {
        self.ensure_owner()?;
        if self.state == SessionState::Detached {
            return Ok(());
        }
        let location = current_location
            .and_then(|item| self.parse_source_location(&current.image.id, item.source.as_ref()));
        let event = ExceptionEvent {
            value: self.debug_value(heap, value)?,
            frame_id: frame_id(current),
            location,
            caught: false,
        };
        self.step = None;
        self.stop_with_event(DebuggerEvent::Exception(event));
        self.wait_while_paused(heap, callers, current, current_location)
    }

    fn stop_with_event(&mut self, event: DebuggerEvent) {
        self.event = Some(event.clone());
        self.state = SessionState::Paused;
        let _ = self.event_sender.send(event);
    }

    fn wait_while_paused(
        &mut self,
        heap: &mut Heap,
        callers: &[VmFrame],
        current: &VmFrame,
        current_location: Option<&InstructionLocation>,
    ) -> Result<(), DebuggerError> {
        while self.state == SessionState::Paused {
            let command = self.commands.as_ref().map(Receiver::recv);
            match command {
                Some(Ok(command)) => {
                    self.process_command(heap, callers, current, current_location, command)?
                }
                Some(Err(_)) | None => {
                    self.state = SessionState::Detached;
                    self.active.store(false, Ordering::Release);
                    self.release_object_handles(heap)?;
                }
            }
        }
        Ok(())
    }

    fn process_command(
        &mut self,
        heap: &mut Heap,
        callers: &[VmFrame],
        current: &VmFrame,
        current_location: Option<&InstructionLocation>,
        command: Command,
    ) -> Result<(), DebuggerError> {
        match command {
            Command::Pause => {
                if self.state == SessionState::Running {
                    self.pause_requested = true;
                }
            }
            Command::SetBreakpoint {
                source_id,
                line,
                column,
                sha256,
                code_generation,
                reply,
            } => {
                let result =
                    self.create_breakpoint(source_id, line, column, sha256, code_generation);
                let _ = reply.send(result);
            }
            Command::RemoveBreakpoint { id, reply } => {
                let _ = reply.send(Ok(self.breakpoints.remove(&id).is_some()));
            }
            Command::ListBreakpoints { reply } => {
                let _ = reply.send(Ok(self.breakpoints.values().cloned().collect()));
            }
            Command::Resume { reply } => {
                let result = self.resume_from_pause(heap);
                let _ = reply.send(result);
            }
            Command::Step { mode, reply } => {
                let result = self.begin_step(heap, callers, current, current_location, mode);
                let _ = reply.send(result);
            }
            Command::GetStack { reply } => {
                let result = if self.state == SessionState::Paused {
                    self.stack_snapshot(callers, current, current_location)
                } else {
                    Err(DebuggerError::new(
                        "call-stack inspection requires a paused execution",
                    ))
                };
                let _ = reply.send(result);
            }
            Command::GetScopes {
                frame_id: requested,
                reply,
            } => {
                let result = self.scopes(heap, callers, current, current_location, &requested);
                let _ = reply.send(result);
            }
            Command::GetVariables {
                frame_id: requested,
                scope_id,
                reply,
            } => {
                let result = self
                    .scopes(heap, callers, current, current_location, &requested)
                    .and_then(|scopes| {
                        scopes
                            .into_iter()
                            .find(|scope| scope.id == scope_id)
                            .map(|scope| scope.variables)
                            .ok_or_else(|| DebuggerError::new("unknown lexical scope"))
                    });
                let _ = reply.send(result);
            }
            Command::EvaluateIdentifier {
                frame_id: requested,
                identifier,
                reply,
            } => {
                let result = self
                    .scopes(heap, callers, current, current_location, &requested)
                    .and_then(|scopes| {
                        scopes
                            .into_iter()
                            .flat_map(|scope| scope.variables)
                            .find(|variable| {
                                variable.binding_id == identifier || variable.name == identifier
                            })
                            .map(|variable| variable.value)
                            .ok_or_else(|| {
                                DebuggerError::new(
                                    "identifier is not available in the selected frame",
                                )
                            })
                    });
                let _ = reply.send(result);
            }
            Command::InspectObject {
                object_id,
                limit,
                reply,
            } => {
                let result = self.inspect(heap, &object_id, limit);
                let _ = reply.send(result);
            }
            Command::EventMessage { reply } => {
                let _ = reply.send(Ok(self.event.clone()));
            }
            Command::Detach { reply } => {
                self.state = SessionState::Detached;
                self.step = None;
                self.pause_requested = false;
                self.breakpoints.clear();
                self.event = None;
                self.active.store(false, Ordering::Release);
                let result = self.release_object_handles(heap);
                self.reject_pending_commands();
                let _ = reply.send(result);
            }
            #[cfg(test)]
            Command::CollectGarbageForTest { reply } => {
                let result = heap.collect().map(|_| ()).map_err(DebuggerError::from);
                let _ = reply.send(result);
            }
        }
        Ok(())
    }

    fn resume_from_pause(&mut self, heap: &mut Heap) -> Result<(), DebuggerError> {
        if self.state != SessionState::Paused {
            return Err(DebuggerError::new("execution is not paused"));
        }
        self.release_object_handles(heap)?;
        self.step = None;
        self.event = None;
        self.state = SessionState::Running;
        Ok(())
    }

    fn begin_step(
        &mut self,
        heap: &mut Heap,
        callers: &[VmFrame],
        current: &VmFrame,
        current_location: Option<&InstructionLocation>,
        mode: StepMode,
    ) -> Result<(), DebuggerError> {
        if self.state != SessionState::Paused {
            return Err(DebuggerError::new("execution is not paused"));
        }
        self.release_object_handles(heap)?;
        let location = current_location
            .and_then(|item| self.parse_source_location(&current.image.id, item.source.as_ref()));
        self.step = Some(StepRequest {
            mode,
            base_depth: callers.len() + 1,
            base_key: location
                .as_ref()
                .and_then(|item| source_key(&current.image.id, item)),
        });
        self.event = None;
        self.state = SessionState::Running;
        Ok(())
    }

    fn matches_breakpoint(&self, function_id: &str, location: Option<&SourceLocation>) -> bool {
        let Some(location) = location else {
            return false;
        };
        let Some(source_id) = location.source_id.as_deref() else {
            return false;
        };
        let Some(start) = location.start else {
            return false;
        };
        self.breakpoints.values().any(|breakpoint| {
            breakpoint.verified
                && breakpoint.source_id == source_id
                && breakpoint.line == start.line
                && breakpoint
                    .column
                    .is_none_or(|column| column == start.column)
                && breakpoint
                    .source_sha256
                    .as_ref()
                    .is_none_or(|sha| location.source_sha256.as_ref() == Some(sha))
                && breakpoint
                    .code_generation
                    .as_ref()
                    .is_none_or(|generation| {
                        self.source_records.iter().any(|record| {
                            record.function_id == function_id
                                && record.location == *location
                                && &record.code_generation == generation
                        })
                    })
        })
    }

    fn stack_snapshot(
        &self,
        callers: &[VmFrame],
        current: &VmFrame,
        current_location: Option<&InstructionLocation>,
    ) -> Result<Vec<StackFrameSnapshot>, DebuggerError> {
        let mut result = Vec::with_capacity(callers.len() + 1);
        result.push(self.frame_snapshot(current, current_location)?);
        for frame in callers.iter().rev() {
            let location = frame
                .last_offset
                .and_then(|offset| frame.image.location(offset));
            result.push(self.frame_snapshot(frame, location.as_ref())?);
        }
        Ok(result)
    }

    fn frame_snapshot(
        &self,
        frame: &VmFrame,
        location: Option<&InstructionLocation>,
    ) -> Result<StackFrameSnapshot, DebuggerError> {
        let generation = code_generation(&frame.image).ok_or_else(|| {
            DebuggerError::new(format!(
                "function {:?} has no valid Debugger API code-generation identity",
                frame.image.id
            ))
        })?;
        let mut scope_ids = location
            .into_iter()
            .flat_map(|item| item.lexical_scopes.iter())
            .filter_map(|scope| scope.get("id").and_then(JsonValue::as_str))
            .filter(|id| valid_api_id(id))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        scope_ids.sort();
        scope_ids.dedup();
        let source_location = location
            .and_then(|item| self.parse_source_location(&frame.image.id, item.source.as_ref()));
        Ok(StackFrameSnapshot {
            id: frame_id(frame),
            function_id: frame.image.id.clone(),
            function_name: {
                let name = if frame.image.name.is_empty() {
                    "(anonymous)"
                } else {
                    frame.image.name.as_str()
                };
                name.chars().take(512).collect()
            },
            code_generation: generation,
            location: source_location,
            scope_ids,
            backend: "interpreter".to_owned(),
        })
    }

    fn scopes(
        &mut self,
        heap: &mut Heap,
        callers: &[VmFrame],
        current: &VmFrame,
        current_location: Option<&InstructionLocation>,
        requested_frame: &str,
    ) -> Result<Vec<ScopeSnapshot>, DebuggerError> {
        if self.state != SessionState::Paused {
            return Err(DebuggerError::new(
                "scope inspection requires a paused execution",
            ));
        }
        let (frame, location) = find_frame(callers, current, current_location, requested_frame)
            .ok_or_else(|| DebuggerError::new("unknown stack frame"))?;
        let Some(location) = location else {
            return Ok(Vec::new());
        };
        let mut result = Vec::new();
        for scope in &location.lexical_scopes {
            let Some(id) = scope.get("id").and_then(JsonValue::as_str) else {
                continue;
            };
            if !valid_api_id(id) {
                continue;
            }
            let parent_id = scope
                .get("parent")
                .and_then(JsonValue::as_str)
                .filter(|parent| valid_api_id(parent))
                .map(str::to_owned);
            let mut binding_ids = scope
                .get("bindings")
                .and_then(JsonValue::as_array)
                .map(|bindings| {
                    bindings
                        .iter()
                        .filter_map(JsonValue::as_str)
                        .filter(|binding| valid_api_id(binding))
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            // Capture IDs live in this function's closure environment, not in
            // environment.bindings. The compiler records their canonical
            // variable_locations separately as kind="capture". Expose them in
            // the function root scope so the Inspector can resolve the alias
            // through the active frame's capture table.
            if parent_id.is_none()
                && let Some(captures) = frame
                    .image
                    .artifact
                    .manifest
                    .pointer("/metadata/environment/captures")
                    .and_then(JsonValue::as_array)
            {
                binding_ids.extend(captures.iter().filter_map(|capture| {
                    capture
                        .get("id")
                        .and_then(JsonValue::as_str)
                        .filter(|capture_id| valid_api_id(capture_id))
                        .map(str::to_owned)
                }));
            }
            binding_ids.sort();
            binding_ids.dedup();

            let mut variables = Vec::with_capacity(binding_ids.len());
            for binding in &binding_ids {
                variables.push(self.variable_snapshot(heap, frame, &location, binding)?);
            }
            result.push(ScopeSnapshot {
                id: id.to_owned(),
                name: "Locals".to_owned(),
                parent_id,
                variables,
            });
        }
        Ok(result)
    }

    fn variable_snapshot(
        &mut self,
        heap: &mut Heap,
        frame: &VmFrame,
        location: &InstructionLocation,
        binding_id: &str,
    ) -> Result<VariableSnapshot, DebuggerError> {
        let metadata = frame.image.artifact.manifest.get("metadata");
        let binding = metadata
            .and_then(|meta| meta.pointer("/environment/bindings"))
            .and_then(JsonValue::as_array)
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item.get("id").and_then(JsonValue::as_str) == Some(binding_id))
            });
        let capture = metadata
            .and_then(|meta| meta.pointer("/environment/captures"))
            .and_then(JsonValue::as_array)
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item.get("id").and_then(JsonValue::as_str) == Some(binding_id))
            });
        let descriptor = binding.or(capture);
        let raw_name = descriptor
            .and_then(|item| item.get("name"))
            .and_then(JsonValue::as_str)
            .filter(|name| !name.is_empty())
            .unwrap_or(binding_id);
        let name = raw_name.chars().take(512).collect::<String>();
        let mutable = descriptor
            .and_then(|item| item.get("mutable"))
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        let value = self.read_binding(heap, frame, location, binding_id)?;
        let (availability, debug_value) = match value {
            Some(value) => ("available".to_owned(), self.debug_value(heap, value)?),
            None => ("unavailable".to_owned(), unavailable_value()),
        };
        Ok(VariableSnapshot {
            binding_id: binding_id.to_owned(),
            name,
            mutable,
            availability,
            value: debug_value,
        })
    }

    fn read_binding(
        &self,
        heap: &Heap,
        frame: &VmFrame,
        location: &InstructionLocation,
        binding_id: &str,
    ) -> Result<Option<Value>, DebuggerError> {
        let Some(storage) = location
            .variable_locations
            .iter()
            .find(|item| item.get("binding").and_then(JsonValue::as_str) == Some(binding_id))
        else {
            return Ok(None);
        };
        let kind = storage
            .get("kind")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        let index = storage
            .get("index")
            .and_then(JsonValue::as_u64)
            .and_then(|value| usize::try_from(value).ok());
        let value = match kind {
            "register" => {
                let Some(index) = index.and_then(|value| u16::try_from(value).ok()) else {
                    return Ok(None);
                };
                match frame.registers.get(index as usize) {
                    Some(RegisterValue::Value(value)) => Some(*value),
                    Some(RegisterValue::Cell(binding)) => {
                        match heap.load_binding(frame.activation.environment(), *binding) {
                            Ok(value) => Some(value),
                            Err(HeapError::UninitializedBinding) => None,
                            Err(error) => return Err(error.into()),
                        }
                    }
                    Some(RegisterValue::Uninitialized | RegisterValue::Token { .. }) | None => None,
                }
            }
            "capture" => {
                let Some(capture) = index.and_then(|value| frame.image.captures.get(value)) else {
                    return Ok(None);
                };
                match heap.load_binding(frame.activation.environment(), capture.alias_binding) {
                    Ok(value) => Some(value),
                    Err(HeapError::UninitializedBinding) => None,
                    Err(error) => return Err(error.into()),
                }
            }
            "local" | "environment" => {
                let register = frame.image.binding_registers.get(binding_id).copied();
                let binding =
                    register.and_then(|slot| frame.image.register_bindings.get(&slot).copied());
                let Some(binding) = binding else {
                    return Ok(None);
                };
                match heap.load_binding(frame.activation.environment(), binding) {
                    Ok(value) => Some(value),
                    Err(HeapError::UninitializedBinding) => None,
                    Err(error) => return Err(error.into()),
                }
            }
            _ => None,
        };
        Ok(value)
    }

    fn debug_value(&mut self, heap: &mut Heap, value: Value) -> Result<DebugValue, DebuggerError> {
        let (value_type, preview, truncated) = match value.value_type() {
            ValueType::Undefined => ("undefined", "undefined".to_owned(), false),
            ValueType::Null => ("null", "null".to_owned(), false),
            ValueType::Boolean => (
                "boolean",
                if value.as_boolean() == Some(true) {
                    "true"
                } else {
                    "false"
                }
                .to_owned(),
                false,
            ),
            ValueType::Number => (
                "number",
                super::format_number(value.as_number().unwrap_or(f64::NAN)),
                false,
            ),
            ValueType::String => {
                let units = heap.string_utf16(value)?;
                let (preview, truncated) = bounded_utf16_preview(units, MAX_PREVIEW_CHARS);
                ("string", preview, truncated)
            }
            ValueType::Object => ("object", "Object".to_owned(), false),
            ValueType::Function => ("function", "Function".to_owned(), false),
        };
        let object_id = if matches!(value.value_type(), ValueType::Object | ValueType::Function) {
            if self.object_handles.len() >= MAX_OBJECT_HANDLES {
                return Ok(DebugValue {
                    value_type: "unavailable".to_owned(),
                    preview: "<inspection handle limit reached>".to_owned(),
                    object_id: None,
                    availability: "unavailable".to_owned(),
                    truncated: false,
                });
            }
            let next_object_id = self
                .next_object_id
                .checked_add(1)
                .ok_or_else(|| DebuggerError::new("object identity space exhausted"))?;
            let id = format!("object-{}", self.next_object_id);
            let handle = heap.retain_host_value(value)?;
            self.next_object_id = next_object_id;
            self.object_handles.insert(id.clone(), handle);
            Some(id)
        } else {
            None
        };
        Ok(DebugValue {
            value_type: value_type.to_owned(),
            preview,
            object_id,
            availability: "available".to_owned(),
            truncated,
        })
    }

    fn inspect(
        &mut self,
        heap: &mut Heap,
        object_id: &str,
        limit: usize,
    ) -> Result<InspectedObject, DebuggerError> {
        if self.state != SessionState::Paused {
            return Err(DebuggerError::new(
                "object inspection requires a paused execution",
            ));
        }
        if limit > MAX_INSPECTION_PROPERTIES {
            return Err(DebuggerError::new(
                "inspection limit must be between 0 and 128",
            ));
        }
        let handle = self
            .object_handles
            .get(object_id)
            .copied()
            .ok_or_else(|| DebuggerError::new("unknown or expired inspection reference"))?;
        let object = heap.resolve_host_handle(handle)?;
        if !matches!(object.value_type(), ValueType::Object | ValueType::Function) {
            return Err(DebuggerError::new(
                "inspection reference does not identify an object",
            ));
        }

        let scope = heap.open_scope()?;
        let result = (|| {
            heap.scope_root(scope, object)?;
            let keys = heap.own_property_keys(object)?;
            let mut properties = Vec::new();
            for key in keys.iter().take(limit) {
                let (name, _) = bounded_utf16_preview(heap.property_key_utf16(*key)?, 1024);
                if let Some(CompletePropertyDescriptor {
                    value,
                    writable,
                    enumerable,
                    configurable,
                }) = heap.get_own_property_descriptor(object, *key)?
                {
                    properties.push(InspectedProperty {
                        name,
                        value: self.debug_value(heap, value)?,
                        enumerable,
                        writable,
                        configurable,
                        is_accessor: false,
                    });
                }
            }
            Ok(InspectedObject {
                object_id: object_id.to_owned(),
                is_proxy: false,
                properties,
                truncated: keys.len() > limit,
            })
        })();
        let close = heap.close_scope(scope);
        match result {
            Err(error) => {
                let _ = close;
                Err(error)
            }
            Ok(value) => {
                close?;
                Ok(value)
            }
        }
    }
}

fn frame_id(frame: &VmFrame) -> String {
    format!("frame-{}", frame.id)
}

fn bounded_utf16_preview(units: &[u16], max_chars: usize) -> (String, bool) {
    let mut end = 0;
    let mut chars = 0;
    while end < units.len() && chars < max_chars {
        let first = units[end];
        end += 1;
        if (0xD800..=0xDBFF).contains(&first)
            && units
                .get(end)
                .is_some_and(|next| (0xDC00..=0xDFFF).contains(next))
        {
            end += 1;
        }
        chars += 1;
    }
    (String::from_utf16_lossy(&units[..end]), end < units.len())
}

fn qualified_source_id(function_id: &str, local_id: &str) -> String {
    let raw = format!(
        "fn_{}_{}_{}_{}",
        function_id.len(),
        function_id,
        local_id.len(),
        local_id
    );
    if raw.len() <= 128 {
        return raw;
    }
    let hash = raw
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325_u64, |state, byte| {
            (state ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    let prefix = raw.chars().take(111).collect::<String>();
    format!("{prefix}_{hash:016x}")
}

fn valid_api_id(value: &str) -> bool {
    if value.is_empty() || value.len() > 128 || !value.is_ascii() {
        return false;
    }
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | ':' | '-')
        })
}

fn parse_position(value: Option<&JsonValue>) -> Option<SourcePosition> {
    let value = value?;
    let line = u32::try_from(value.get("line")?.as_u64()?).ok()?;
    if line == 0 {
        return None;
    }
    Some(SourcePosition {
        line,
        column: u32::try_from(value.get("column")?.as_u64()?).ok()?,
    })
}

fn module_function_identity(image: &FunctionImage) -> Option<ModuleFunctionIdentity> {
    let metadata = image.artifact.manifest.get("metadata")?;
    let code_cache_id = metadata.pointer("/code_cache/id")?.as_str()?;
    let code_cache_version = metadata.pointer("/code_cache/version")?.as_str()?;
    let code_sha256 = image.artifact.manifest.get("code_sha256")?.as_str()?;
    if !valid_api_id(code_cache_id)
        || code_cache_version.is_empty()
        || code_cache_version.chars().count() > 128
        || code_sha256.len() != 64
        || !code_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    Some(ModuleFunctionIdentity {
        code_generation: code_generation(image)?,
        code_cache_id: code_cache_id.to_owned(),
        code_cache_version: code_cache_version.to_owned(),
        code_sha256: code_sha256.to_owned(),
    })
}

fn code_generation(image: &FunctionImage) -> Option<CodeGeneration> {
    let metadata = image.artifact.manifest.get("metadata")?;
    let id = metadata.pointer("/code_generation/id")?.as_str()?;
    let version = metadata.pointer("/code_generation/version")?.as_str()?;
    if !valid_api_id(id) || version.is_empty() || version.chars().count() > 128 {
        return None;
    }
    Some(CodeGeneration {
        id: id.to_owned(),
        version: version.to_owned(),
    })
}

fn source_key(function_id: &str, location: &SourceLocation) -> Option<SourceKey> {
    if location.origin != "user" {
        return None;
    }
    let start = location.start?;
    Some(SourceKey {
        function_id: function_id.to_owned(),
        source_id: location.source_id.clone()?,
        line: start.line,
        column: start.column,
    })
}

fn unavailable_value() -> DebugValue {
    DebugValue {
        value_type: "unavailable".to_owned(),
        preview: "<unavailable>".to_owned(),
        object_id: None,
        availability: "unavailable".to_owned(),
        truncated: false,
    }
}

fn find_frame<'a>(
    callers: &'a [VmFrame],
    current: &'a VmFrame,
    current_location: Option<&'a InstructionLocation>,
    requested_id: &str,
) -> Option<(&'a VmFrame, Option<InstructionLocation>)> {
    if frame_id(current) == requested_id {
        return Some((current, current_location.cloned()));
    }
    for frame in callers.iter().rev() {
        if frame_id(frame) == requested_id {
            return Some((
                frame,
                frame
                    .last_offset
                    .and_then(|offset| frame.image.location(offset)),
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use ogma_compiler::compile_to_bytecode;
    use ogma_frontend::{ParseRequestOptions, SourceFile, parse};
    use ogma_runtime::{HeapConfig, PropertyDescriptor, ValueType};
    use oxc_allocator::Allocator;

    #[test]
    fn frame_and_inspection_roots_survive_gc_while_execution_is_paused() {
        let source_text = "function run(subject) {\n  return subject;\n}\n";
        let allocator = Allocator::default();
        let source = SourceFile::new(
            "ogm49-paused-gc",
            "memory:///ogm49-paused-gc.js",
            None,
            source_text,
        )
        .expect("debugger test source is valid");
        let parsed = parse(&allocator, &source, ParseRequestOptions::default())
            .expect("debugger test source parses");
        let compiled = compile_to_bytecode(&parsed).expect("debugger test source compiles");
        let entry_id = compiled
            .functions
            .iter()
            .find(|function| function.name == "run")
            .expect("run entry exists")
            .id
            .clone();
        let artifacts = compiled
            .functions
            .into_iter()
            .map(|function| function.artifact.bytes)
            .collect::<Vec<_>>();
        let module = ExecutableModule::load(artifacts)
            .expect("compiler-produced artifacts pass bytecode verification");

        let functions = module.functions();
        let source_id = functions
            .iter()
            .find_map(|function| {
                (0..4096_u32)
                    .filter_map(|offset| module.source_location(&function.id, offset))
                    .find_map(|location| {
                        let source = location.source?;
                        let start = source.get("start")?;
                        if start.get("line")?.as_u64()? != 2 {
                            return None;
                        }
                        Some(source.get("file")?.as_str()?.to_owned())
                    })
            })
            .expect("return statement has a source mapping");
        let (mut session, handle) = module
            .create_debugger_session()
            .expect("debugger indexes verified source metadata");
        let breakpoint = session
            .set_breakpoint(source_id, 2, None, None, None)
            .expect("return statement breakpoint is accepted");
        assert!(breakpoint.verified, "{breakpoint:?}");

        let controller = thread::spawn(move || {
            let event = handle
                .recv_event_timeout(Duration::from_secs(5))
                .expect("event channel is connected")
                .expect("breakpoint stops the interpreter");
            assert!(
                matches!(event, DebuggerEvent::Stopped(StoppedEvent { reason, .. }) if reason == "breakpoint")
            );

            // The VM instruction root scope remains open during this owner-thread collection.
            handle
                .collect_garbage_for_test()
                .expect("live frame registers remain rooted during collection");

            let stack = handle
                .get_stack()
                .expect("VM frame survives paused collection");
            let scopes = handle
                .get_scopes(stack[0].id.clone())
                .expect("lexical environment survives paused collection");
            let subject = scopes
                .iter()
                .flat_map(|scope| &scope.variables)
                .find(|variable| variable.name == "subject")
                .expect("parameter is present in the current scope");
            assert_eq!(subject.value.value_type, "object");
            let object_id = subject
                .value
                .object_id
                .clone()
                .expect("object gets an opaque paused inspection handle");

            // A second collection validates the persistent host handle between
            // the variable and descriptor-inspection requests.
            handle
                .collect_garbage_for_test()
                .expect("inspection handle remains rooted during collection");
            let inspected = handle
                .inspect_object(object_id, 16)
                .expect("object remains inspectable after GC");
            assert!(
                inspected.properties.iter().any(|property| {
                    property.name == "answer" && property.value.preview == "42"
                })
            );

            handle
                .detach()
                .expect("detach releases all inspection handles");
        });

        let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");
        let object = heap.allocate_object().expect("managed object");
        let roots = heap.open_scope().expect("object initialization root scope");
        heap.scope_root(roots, object)
            .expect("object is rooted while initialized");
        let property = heap
            .intern_property_key("answer")
            .expect("object property key");
        assert!(
            heap.define_own_property(
                object,
                property,
                PropertyDescriptor::data(Value::number(42.0), true, true, true),
            )
            .expect("object property descriptor is accepted")
        );
        heap.close_scope(roots).expect("initialization root closes");

        let report = module
            .execute_debugged(
                &mut heap,
                &entry_id,
                &[object],
                &mut super::super::NoHost,
                Default::default(),
                &mut session,
            )
            .expect("debugged execution completes after detach");
        controller.join().expect("GC debugger controller completes");
        assert!(
            matches!(report.completion, super::super::ExecutionCompletion::Return(value) if value.value_type() == ValueType::Object)
        );

        let roots = heap.root_snapshot();
        assert_eq!(roots.interpreter_frames, 0);
        assert_eq!(roots.temporary_scopes, 0);
        assert_eq!(roots.persistent_host_roots, 0);
    }

    #[test]
    fn debugger_string_previews_are_bounded_without_splitting_surrogate_pairs() {
        let input = "A😀B".encode_utf16().collect::<Vec<_>>();
        let (preview, truncated) = bounded_utf16_preview(&input, 2);
        assert_eq!(preview, "A😀");
        assert!(truncated);

        let (complete, truncated) = bounded_utf16_preview(&input, 3);
        assert_eq!(complete, "A😀B");
        assert!(!truncated);

        let long = vec![u16::from(b'x'); 1_000_000];
        let (preview, truncated) = bounded_utf16_preview(&long, MAX_PREVIEW_CHARS);
        assert_eq!(preview.chars().count(), MAX_PREVIEW_CHARS);
        assert!(truncated);
    }

    #[test]
    fn debugger_api_identifiers_follow_the_v1_schema() {
        assert!(valid_api_id("source_file"));
        assert!(valid_api_id("_module:entry-1.0"));
        assert!(valid_api_id(&format!("a{}", "b".repeat(127))));
        assert!(!valid_api_id(""));
        assert!(!valid_api_id("1starts_with_digit"));
        assert!(!valid_api_id("contains space"));
        assert!(!valid_api_id("contains/slash"));
        assert!(!valid_api_id("a".repeat(129).as_str()));
        assert!(!valid_api_id("éclair"));
        let canonical_a = qualified_source_id("a_b", "c");
        let canonical_b = qualified_source_id("a", "b_c");
        assert_ne!(canonical_a, canonical_b);
        assert!(valid_api_id(&canonical_a));
        let long_canonical = qualified_source_id(&"f".repeat(128), &"s".repeat(128));
        assert!(long_canonical.len() <= 128);
        assert!(valid_api_id(&long_canonical));
    }

    #[test]
    fn debugger_session_rejects_a_different_code_generation() {
        let allocator = Allocator::default();
        let source = SourceFile::new(
            "ogm49-generation-identity",
            "memory:///ogm49-generation-identity.js",
            None,
            "function run() { return 1; }",
        )
        .expect("debugger test source is valid");
        let parsed = parse(&allocator, &source, ParseRequestOptions::default())
            .expect("debugger test source parses");
        let compiled = compile_to_bytecode(&parsed).expect("debugger test source compiles");
        let artifacts = compiled
            .functions
            .into_iter()
            .map(|function| function.artifact.bytes)
            .collect::<Vec<_>>();
        let module = ExecutableModule::load(artifacts).expect("compiler-produced artifacts verify");
        let (mut session, _handle) =
            DebuggerSession::new(&module).expect("debugger session indexes its module");
        session
            .validate_module(&module)
            .expect("session accepts the exact module it indexed");

        let function_id = session
            .module_generations
            .keys()
            .next()
            .expect("module includes a function")
            .clone();
        let mut changed = session
            .module_generations
            .get(&function_id)
            .expect("function has a code generation and cache identity")
            .clone();
        changed.code_generation.id = "different_generation".to_owned();
        session
            .module_generations
            .insert(function_id.clone(), changed.clone());
        assert!(session.validate_module(&module).is_err());

        let mut changed_cache = session
            .module_generations
            .get(&function_id)
            .expect("function has an indexed identity")
            .clone();
        changed_cache.code_cache_id = "different_cache".to_owned();
        session
            .module_generations
            .insert(function_id, changed_cache);
        assert!(session.validate_module(&module).is_err());
    }
}
