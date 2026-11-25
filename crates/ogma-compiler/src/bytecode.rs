//! Verified Core IR v1 to OGMB v1 register lowering.
//!
//! The lowering is function-scoped. It consumes the canonical typed Core IR,
//! preserves source provenance and rejects IR features outside the current M1
//! execution profile rather than silently erasing their semantics.

use std::collections::{BTreeMap, HashMap};

use ogma_bytecode::{
    BinaryOperation, CallKind, ConstantKind, Diagnostic, RegisterInstruction, RegisterOperation,
    UnaryOperation, VerifiedArtifact, build_ogmb_v1, encode_register_code, sha256_hex,
    verify_register_artifact,
};
use ogma_core_ir::{
    BindingKind, BlockParameter, CaptureMode, ExceptionFlow, Function, FunctionDocument, Source,
    Terminator, UnreachableReason, verify_document,
};
use serde_json::{Value, json};

/// Deterministic compile-time size and frame counters for a function artifact.
///
/// These are structural compiler counters, not interpreter throughput or
/// runtime allocation measurements.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodegenMetrics {
    /// Number of decoded register instructions.
    pub instruction_count: u32,
    /// Number of bytes in the encoded code section.
    pub code_bytes: usize,
    /// Number of scalar constants in the function pool.
    pub constant_count: usize,
    /// Number of virtual registers in the function frame.
    pub register_count: u32,
}

/// One independently verified portable artifact for a Core IR function.
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledFunction {
    /// Stable Core IR function identity.
    pub id: String,
    /// Human-readable function name.
    pub name: String,
    /// Deterministic code-generation counters for the artifact.
    pub metrics: CodegenMetrics,
    /// OGMB container and decoder facts accepted by the verifier boundary.
    pub artifact: VerifiedArtifact,
}

/// A module's independently verified portable function artifacts.
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledBytecodeModule {
    /// Compiled functions in canonical Core IR document order.
    pub functions: Vec<CompiledFunction>,
}

/// Lowers verified function documents into independently verified OGMB v1 files.
pub fn compile_functions(
    documents: &[FunctionDocument],
) -> Result<Vec<CompiledFunction>, Vec<Diagnostic>> {
    validate_module_references(documents)?;

    let owners: HashMap<&str, &str> = documents
        .iter()
        .map(|document| {
            (
                document.function.environment.id.as_str(),
                document.function.id.as_str(),
            )
        })
        .collect();
    let mut captured_cells = HashMap::<String, std::collections::BTreeSet<String>>::new();
    for child in documents {
        for capture in &child.function.captures {
            if capture.mode == CaptureMode::Cell {
                let Some(owner) = owners.get(capture.source_environment.as_str()) else {
                    return Err(vec![diagnostic(
                        "bytecode.capture_owner_missing",
                        format!("$.function.{}.captures.{}", child.function.id, capture.id),
                        format!(
                            "capture source environment {:?} is not present in this compilation unit",
                            capture.source_environment
                        ),
                    )]);
                };
                captured_cells
                    .entry((*owner).to_owned())
                    .or_default()
                    .insert(capture.binding.clone());
            }
        }
    }

    let mut output = Vec::with_capacity(documents.len());
    for document in documents {
        let cells = captured_cells
            .get(&document.function.id)
            .cloned()
            .unwrap_or_default();
        output.push(compile_function_with_cells(document, &cells)?);
    }
    Ok(output)
}

/// Lowers one verified Core IR function into an OGMB v1 artifact.
///
/// A standalone function cannot resolve nested closure targets or determine
/// which local bindings require shared cells. Use compile_functions for a
/// complete compilation unit.
pub fn compile_function(document: &FunctionDocument) -> Result<CompiledFunction, Vec<Diagnostic>> {
    if !document.function.captures.is_empty()
        || document
            .function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(|instruction| instruction.op == "closure.create")
    {
        return Err(vec![diagnostic(
            "bytecode.module_required",
            "$.function.instructions",
            "closure creation requires module-level target and capture validation; use compile_functions",
        )]);
    }
    compile_function_with_cells(document, &std::collections::BTreeSet::new())
}

fn validate_module_references(documents: &[FunctionDocument]) -> Result<(), Vec<Diagnostic>> {
    let mut functions = HashMap::<&str, &FunctionDocument>::new();
    let mut environments = HashMap::<&str, &FunctionDocument>::new();
    for document in documents {
        let errors = verify_document(document);
        if !errors.is_empty() {
            return Err(errors
                .into_iter()
                .map(|item| diagnostic(item.code, item.path, item.message))
                .collect());
        }
        let function = &document.function;
        if functions.insert(function.id.as_str(), document).is_some() {
            return Err(vec![diagnostic(
                "bytecode.function_identity_duplicate",
                "$.functions",
                format!("duplicate function identity {:?}", function.id),
            )]);
        }
        if environments
            .insert(function.environment.id.as_str(), document)
            .is_some()
        {
            return Err(vec![diagnostic(
                "bytecode.environment_identity_duplicate",
                "$.functions",
                format!(
                    "duplicate environment identity {:?}",
                    function.environment.id
                ),
            )]);
        }
    }

    for document in documents {
        let parent = &document.function;
        for capture in &parent.captures {
            let owner = environments
                .get(capture.source_environment.as_str())
                .ok_or_else(|| {
                    vec![diagnostic(
                        "bytecode.capture_owner_missing",
                        format!("$.function.{}.captures.{}", parent.id, capture.id),
                        format!(
                            "capture owner environment {:?} is absent from the compilation unit",
                            capture.source_environment
                        ),
                    )]
                })?;
            let source_binding = owner
                .function
                .environment
                .bindings
                .iter()
                .find(|binding| binding.id == capture.binding)
                .ok_or_else(|| {
                    vec![diagnostic(
                        "bytecode.capture_binding_missing",
                        format!("$.function.{}.captures.{}", parent.id, capture.id),
                        format!(
                            "source binding {:?} is absent from its owning environment",
                            capture.binding
                        ),
                    )]
                })?;
            if capture.name != source_binding.name
                || capture.mutable != source_binding.mutable
                || capture.semantic_type != source_binding.semantic_type
                || capture.representation != source_binding.representation
                || (capture.mutable && capture.mode != CaptureMode::Cell)
                || (!capture.mutable && capture.mode != CaptureMode::Value)
            {
                return Err(vec![diagnostic(
                    "bytecode.capture_metadata_mismatch",
                    format!("$.function.{}.captures.{}", parent.id, capture.id),
                    "capture descriptor does not preserve the source binding name, mutability, type, representation or storage mode",
                )]);
            }
        }

        for instruction in parent.blocks.iter().flat_map(|block| &block.instructions) {
            if instruction.op != "closure.create" {
                continue;
            }
            let target_id = instruction
                .attributes
                .get("function")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    vec![diagnostic(
                        "bytecode.closure_target",
                        format!("$.function.{}.instructions.{}", parent.id, instruction.id),
                        "closure.create must identify a target function",
                    )]
                })?;
            let target = functions.get(target_id).ok_or_else(|| {
                vec![diagnostic(
                    "bytecode.closure_target_missing",
                    format!("$.function.{}.instructions.{}", parent.id, instruction.id),
                    format!("closure target function {target_id:?} is not present in the compilation unit"),
                )]
            })?;
            let descriptors = instruction
                .attributes
                .get("captures")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    vec![diagnostic(
                        "bytecode.closure_captures",
                        format!("$.function.{}.instructions.{}", parent.id, instruction.id),
                        "closure capture descriptors must be an array",
                    )]
                })?;
            if descriptors.len() != target.function.captures.len() {
                return Err(vec![diagnostic(
                    "bytecode.closure_capture_arity",
                    format!("$.function.{}.instructions.{}", parent.id, instruction.id),
                    format!(
                        "closure target expects {} captures, received {}",
                        target.function.captures.len(),
                        descriptors.len()
                    ),
                )]);
            }

            for (index, (descriptor, target_capture)) in descriptors
                .iter()
                .zip(&target.function.captures)
                .enumerate()
            {
                let path = format!(
                    "$.function.{}.instructions.{}.attributes.captures[{index}]",
                    parent.id, instruction.id
                );
                let alias = descriptor
                    .get("binding")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let mode = descriptor
                    .get("mode")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let expected_mode = match target_capture.mode {
                    CaptureMode::Cell => "cell",
                    CaptureMode::Value => "value",
                };
                if mode != expected_mode {
                    return Err(vec![diagnostic(
                        "bytecode.closure_capture_mode",
                        format!("{path}.mode"),
                        format!(
                            "target capture requires mode {expected_mode:?}, descriptor declares {mode:?}"
                        ),
                    )]);
                }

                if let Some(local) = parent
                    .environment
                    .bindings
                    .iter()
                    .find(|binding| binding.id == alias)
                {
                    let source_mode = if local.kind == BindingKind::Const {
                        "value"
                    } else {
                        "cell"
                    };
                    if target_capture.source_environment != parent.environment.id
                        || target_capture.binding != local.id
                        || target_capture.name != local.name
                        || target_capture.mutable != local.mutable
                        || target_capture.semantic_type != local.semantic_type
                        || target_capture.representation != local.representation
                        || source_mode != mode
                    {
                        return Err(vec![diagnostic(
                            "bytecode.closure_capture_source",
                            path.clone(),
                            "closure capture does not match the owning local binding identity, environment or mutability mode",
                        )]);
                    }
                } else if let Some(imported) =
                    parent.captures.iter().find(|capture| capture.id == alias)
                {
                    let source_mode = match imported.mode {
                        CaptureMode::Cell => "cell",
                        CaptureMode::Value => "value",
                    };
                    if target_capture.source_environment != imported.source_environment
                        || target_capture.binding != imported.binding
                        || target_capture.name != imported.name
                        || target_capture.mutable != imported.mutable
                        || target_capture.semantic_type != imported.semantic_type
                        || target_capture.representation != imported.representation
                        || source_mode != mode
                    {
                        return Err(vec![diagnostic(
                            "bytecode.closure_capture_source",
                            path.clone(),
                            "forwarded closure capture does not preserve its original environment, binding and storage mode",
                        )]);
                    }
                } else {
                    return Err(vec![diagnostic(
                        "bytecode.closure_capture_source_missing",
                        path.clone(),
                        format!(
                            "capture source alias {alias:?} is not owned or imported by the enclosing function"
                        ),
                    )]);
                }
            }
        }
    }
    Ok(())
}

fn compile_function_with_cells(
    document: &FunctionDocument,
    captured_cells: &std::collections::BTreeSet<String>,
) -> Result<CompiledFunction, Vec<Diagnostic>> {
    let ir_errors = verify_document(document);
    if !ir_errors.is_empty() {
        return Err(ir_errors
            .into_iter()
            .map(|item| Diagnostic {
                code: item.code.to_owned(),
                path: item.path,
                message: item.message,
            })
            .collect());
    }

    let function = &document.function;
    let mut allocator = RegisterAllocator::default();
    let mut value_registers = HashMap::<String, u16>::new();
    let mut binding_registers = HashMap::<String, u16>::new();
    let mut capture_registers = HashMap::<String, u16>::new();
    let mut entry_registers = Vec::<u16>::new();

    for parameter in &function.parameters {
        let register = allocator.allocate()?;
        if value_registers
            .insert(parameter.id.clone(), register)
            .is_some()
        {
            return Err(vec![diagnostic(
                "register.identity_duplicate",
                "$.function.parameters",
                "parameter SSA identities must be unique",
            )]);
        }
        entry_registers.push(register);
    }

    for capture in &function.captures {
        let register = allocator.allocate()?;
        if capture_registers.contains_key(&capture.id)
            || (capture.binding != capture.id && capture_registers.contains_key(&capture.binding))
        {
            return Err(vec![diagnostic(
                "register.capture_duplicate",
                "$.function.captures",
                "capture aliases must map to one unique entry register",
            )]);
        }
        capture_registers.insert(capture.id.clone(), register);
        entry_registers.push(register);
    }

    for binding in &function.environment.bindings {
        if !binding_registers.contains_key(&binding.id)
            && !capture_registers.contains_key(&binding.id)
        {
            binding_registers.insert(binding.id.clone(), allocator.allocate()?);
        }
        if matches!(binding.kind, BindingKind::Catch | BindingKind::Import) {
            return Err(vec![diagnostic(
                "bytecode.binding_unsupported",
                format!("$.function.environment.bindings[{}]", binding.id),
                "catch and import bindings are outside the current synchronous M1 register profile",
            )]);
        }
    }

    for block in &function.blocks {
        if block.exception_parameter.is_some() {
            return Err(vec![diagnostic(
                "bytecode.exception_handler_unsupported",
                format!("$.function.blocks[{}].exception_parameter", block.id),
                "local catch/finally handler entry is not supported by the current M1 frontend profile",
            )]);
        }
        for parameter in &block.parameters {
            if let BlockParameter::Value(value) = parameter {
                value_registers.insert(value.id.clone(), allocator.allocate()?);
            }
        }
        for instruction in &block.instructions {
            if instruction.result.is_some() {
                let result = instruction.result.as_ref().expect("checked");
                value_registers.insert(result.id.clone(), allocator.allocate()?);
            }
            if instruction.effects.may_suspend {
                return Err(vec![diagnostic(
                    "bytecode.suspension_unsupported",
                    format!(
                        "$.function.blocks[{}].instructions[{}]",
                        block.id, instruction.id
                    ),
                    "async suspension is outside the current synchronous M1 bytecode profile",
                )]);
            }
            if matches!(instruction.exception, Some(ExceptionFlow::Edge { .. })) {
                return Err(vec![diagnostic(
                    "bytecode.exception_edge_unsupported",
                    format!(
                        "$.function.blocks[{}].instructions[{}].exception",
                        block.id, instruction.id
                    ),
                    "local exceptional CFG edges require catch/finally lowering and are not supported by this profile",
                )]);
            }
        }
    }

    let const_binding_ids: std::collections::BTreeSet<String> = function
        .environment
        .bindings
        .iter()
        .filter(|binding| binding.kind == BindingKind::Const)
        .map(|binding| binding.id.clone())
        .collect();
    let mut seen_const_definitions = std::collections::BTreeSet::<String>::new();
    let mut const_definition_instructions = std::collections::BTreeSet::<String>::new();
    for block in &function.blocks {
        for instruction in &block.instructions {
            let mut defines_const = false;
            for binding in source_bindings(&instruction.source) {
                if const_binding_ids.contains(&binding) && seen_const_definitions.insert(binding) {
                    defines_const = true;
                }
            }
            if defines_const {
                const_definition_instructions.insert(instruction.id.clone());
            }
        }
    }

    let mut constants = ConstantPool::default();
    let entry_index = function
        .blocks
        .iter()
        .position(|block| block.id == function.entry_block)
        .ok_or_else(|| {
            vec![diagnostic(
                "bytecode.entry_missing",
                "$.function.entry_block",
                "entry block is missing from the function",
            )]
        })?;

    let mut ordered_blocks = Vec::with_capacity(function.blocks.len());
    ordered_blocks.push(&function.blocks[entry_index]);
    for (index, block) in function.blocks.iter().enumerate() {
        if index != entry_index {
            ordered_blocks.push(block);
        }
    }

    let mut plans: Vec<PlannedBlock> = ordered_blocks
        .iter()
        .map(|block| PlannedBlock {
            id: block.id.clone(),
            instructions: Vec::new(),
        })
        .collect();
    let mut bridge_counter = 0usize;
    for index in 0..ordered_blocks.len() {
        let block = ordered_blocks[index];
        let mut instructions = Vec::new();
        if block.id == function.entry_block {
            emit_binding_initializers(
                function,
                &binding_registers,
                captured_cells,
                &mut allocator,
                &mut constants,
                &mut instructions,
            )?;
        }

        for instruction in &block.instructions {
            let lowered = lower_instruction(
                function,
                instruction,
                &value_registers,
                &binding_registers,
                &capture_registers,
                captured_cells,
                &const_definition_instructions,
                &mut allocator,
                &mut constants,
            )?;
            instructions.extend(lowered);
        }

        match &block.terminator {
            Terminator::Jump {
                target,
                arguments,
                source,
            } => {
                let edge = EdgeView { target, arguments };
                let target_id = materialize_edge(
                    function,
                    edge,
                    source,
                    &value_registers,
                    &mut allocator,
                    &mut plans,
                    &mut bridge_counter,
                )?;
                instructions.push(PlannedInstruction {
                    operation: PlannedOperation::Jump(target_id),
                    may_throw: false,
                    source: source.clone(),
                });
            }
            Terminator::Branch {
                condition,
                then_edge,
                else_edge,
                source,
            } => {
                let condition = value_register(
                    &value_registers,
                    condition,
                    "$.function.terminator.condition",
                )?;
                let then_same = then_edge.target == else_edge.target
                    && then_edge.arguments == else_edge.arguments;
                if then_same {
                    let target_id = materialize_edge(
                        function,
                        EdgeView {
                            target: &then_edge.target,
                            arguments: &then_edge.arguments,
                        },
                        source,
                        &value_registers,
                        &mut allocator,
                        &mut plans,
                        &mut bridge_counter,
                    )?;
                    instructions.push(PlannedInstruction {
                        operation: PlannedOperation::Jump(target_id),
                        may_throw: false,
                        source: source.clone(),
                    });
                } else {
                    let then_target = materialize_edge(
                        function,
                        EdgeView {
                            target: &then_edge.target,
                            arguments: &then_edge.arguments,
                        },
                        source,
                        &value_registers,
                        &mut allocator,
                        &mut plans,
                        &mut bridge_counter,
                    )?;
                    let else_target = materialize_edge(
                        function,
                        EdgeView {
                            target: &else_edge.target,
                            arguments: &else_edge.arguments,
                        },
                        source,
                        &value_registers,
                        &mut allocator,
                        &mut plans,
                        &mut bridge_counter,
                    )?;
                    instructions.push(PlannedInstruction {
                        operation: PlannedOperation::Branch {
                            condition,
                            then_target,
                            else_target,
                        },
                        may_throw: false,
                        source: source.clone(),
                    });
                }
            }
            Terminator::Return { value, source } => {
                let register = match value {
                    Some(value) => {
                        value_register(&value_registers, value, "$.function.terminator.return")?
                    }
                    None => {
                        let destination = allocator.allocate()?;
                        let constant = constants.insert(ConstantKind::Undefined, Value::Null)?;
                        instructions.push(PlannedInstruction {
                            operation: PlannedOperation::Ready(RegisterOperation::Constant {
                                destination,
                                constant,
                                kind: ConstantKind::Undefined,
                            }),
                            may_throw: false,
                            source: generated_source(
                                function,
                                "materialize implicit undefined return",
                                Vec::new(),
                            ),
                        });
                        destination
                    }
                };
                instructions.push(PlannedInstruction {
                    operation: PlannedOperation::Ready(RegisterOperation::Return {
                        source: register,
                    }),
                    may_throw: false,
                    source: source.clone(),
                });
            }
            Terminator::Unreachable { reason, source: _ } => match reason {
                UnreachableReason::AfterThrow
                    if matches!(
                        instructions.last().map(|item| &item.operation),
                        Some(PlannedOperation::Ready(RegisterOperation::Throw { .. }))
                    ) => {}
                UnreachableReason::AfterThrow => {
                    return Err(vec![diagnostic(
                        "bytecode.throw_terminator",
                        format!("$.function.blocks[{}].terminator", block.id),
                        "after_throw terminator must immediately follow a js.throw instruction",
                    )]);
                }
                UnreachableReason::ProvenUnreachable => {
                    return Err(vec![diagnostic(
                        "bytecode.unreachable_terminator",
                        format!("$.function.blocks[{}].terminator", block.id),
                        "proven-unreachable blocks have no equivalent executable M1 opcode and must be removed by Core IR lowering",
                    )]);
                }
            },
        }

        plans[index].instructions = instructions;
    }

    if plans.is_empty() || plans[0].id != function.entry_block {
        return Err(vec![diagnostic(
            "bytecode.entry_layout",
            "$.function.blocks",
            "entry block must be first in the emitted function layout",
        )]);
    }

    let mut block_offsets = BTreeMap::<String, u32>::new();
    let mut code_cursor = 0usize;
    for block in &plans {
        if code_cursor > u32::MAX as usize {
            return Err(vec![diagnostic(
                "resource.code_size",
                "$.function.blocks",
                "function code exceeds the 32-bit byte-offset range",
            )]);
        }
        block_offsets.insert(block.id.clone(), code_cursor as u32);
        for instruction in &block.instructions {
            code_cursor = code_cursor
                .checked_add(instruction.encoded_len())
                .ok_or_else(|| {
                    vec![diagnostic(
                        "resource.code_size",
                        "$.function.blocks",
                        "function instruction layout overflowed",
                    )]
                })?;
        }
    }

    let mut encoded_instructions = Vec::new();
    let mut source_locations = Vec::new();
    let mut offset = 0u32;
    for block in &plans {
        for instruction in &block.instructions {
            let operation = instruction.resolve(&block_offsets)?;
            source_locations.push(json!({
                "offset": offset,
                "source": normalized_source(&instruction.source, function)?
            }));
            let encoded = RegisterInstruction {
                operation,
                may_throw: instruction.may_throw,
            };
            let length = encoded.encoded_len();
            offset = offset.checked_add(length as u32).ok_or_else(|| {
                vec![diagnostic(
                    "resource.code_size",
                    "$.code",
                    "instruction offset overflowed",
                )]
            })?;
            encoded_instructions.push(encoded);
        }
    }
    if offset as usize != code_cursor {
        return Err(vec![diagnostic(
            "bytecode.layout_mismatch",
            "$.code",
            "resolved bytecode length differs from the planned CFG layout",
        )]);
    }

    let code = encode_register_code(&encoded_instructions)?;
    let register_count = allocator.count();
    if register_count == 0 || register_count > 65_536 {
        return Err(vec![diagnostic(
            "resource.register_count",
            "$.manifest.metadata.frame_layout.register_count",
            "register frame must contain between one and 65536 registers",
        )]);
    }

    let max_register_index = maximum_used_register(&encoded_instructions, &entry_registers)
        .ok_or_else(|| {
            vec![diagnostic(
                "register.frame_empty",
                "$.instructions",
                "encoded function does not reference a register",
            )]
        })?;
    let instruction_count = encoded_instructions.len() as u32;
    let exception_count = 0u32;
    let constant_count = constants.values.len() as u32;

    let environment_locations = function
        .environment
        .bindings
        .iter()
        .map(|binding| {
            let register = binding_registers.get(&binding.id).copied().ok_or_else(|| {
                vec![diagnostic(
                    "metadata.binding_location",
                    format!("$.function.environment.bindings.{}", binding.id),
                    "lexical binding has no canonical register location",
                )]
            })?;
            Ok(json!({"binding":binding.id,"kind":"register","index":register}))
        })
        .collect::<Result<Vec<_>, Vec<Diagnostic>>>()?;
    let parameter_binding_count = function.parameters.len();
    let local_count = 0u32;

    let captures_json = serde_json::to_value(&function.captures).map_err(|error| {
        vec![diagnostic(
            "metadata.capture_serialize",
            "$.manifest.metadata.environment.captures",
            error.to_string(),
        )]
    })?;
    let bindings_json = serde_json::to_value(&function.environment.bindings).map_err(|error| {
        vec![diagnostic(
            "metadata.binding_serialize",
            "$.manifest.metadata.environment.bindings",
            error.to_string(),
        )]
    })?;
    let source_files_json = function
        .source_files
        .iter()
        .map(|file| {
            let mut value = serde_json::to_value(file).map_err(|error| {
                vec![diagnostic(
                    "source_map.files_serialize",
                    "$.source_map.files",
                    error.to_string(),
                )]
            })?;
            if let Some(path) = value.get("path").and_then(Value::as_str).map(str::to_owned)
                && let Some(relative) = path.strip_prefix("memory:///")
            {
                value["path"] = Value::String(format!("memory/{relative}"));
            }
            Ok(value)
        })
        .collect::<Result<Vec<_>, Vec<Diagnostic>>>()?;

    let mut variable_locations = Vec::new();
    for binding in &function.environment.bindings {
        variable_locations.push(json!({
            "binding":binding.id,
            "kind":"register",
            "index":binding_registers[&binding.id],
            "start_offset":0,
            "end_offset":code.len()
        }));
    }
    for (index, capture) in function.captures.iter().enumerate() {
        variable_locations.push(json!({
            "binding":capture.id,
            "kind":"capture",
            "index":index,
            "start_offset":0,
            "end_offset":code.len()
        }));
    }

    let source_map = json!({
        "version":"1.0.0",
        "files":source_files_json,
        "locations":source_locations,
        "lexical_scopes":[{
            "id":function.environment.id,
            "parent":Value::Null,
            "start_offset":0,
            "end_offset":code.len(),
            "bindings":function.environment.bindings.iter().map(|binding|binding.id.clone()).collect::<Vec<_>>()
        }],
        "variable_locations":variable_locations
    });

    let ir_bytes = serde_json::to_vec(document).map_err(|error| {
        vec![diagnostic(
            "metadata.cache_fingerprint",
            "$.function",
            format!("could not serialize verified Core IR for cache identity: {error}"),
        )]
    })?;
    let cache_identity = format!("cache_{}", &sha256_hex(&ir_bytes)[..32]);
    let metadata = json!({
        "execution_model":"register",
        "function":{"id":function.id,"name":function.name},
        "code_generation":{"id":"ogma_m1_register_codegen","version":"1.0.0"},
        "code_cache":{"id":cache_identity,"version":"1.0.0"},
        "frame_layout":{
            "parameter_count":parameter_binding_count,
            "local_count":local_count,
            "register_count":register_count
        },
        "environment":{
            "id":function.environment.id,
            "parent":function.environment.parent,
            "bindings":bindings_json,
            "captures":captures_json,
            "locations":environment_locations
        },
        "suspension_points":[],
        "inline_cache_slots":[],
        "verifier_summary":{
            "schema":"ogma.bytecode-verification-summary",
            "version":"1.0.0",
            "code_sha256":"0".repeat(64),
            "instruction_count":instruction_count,
            "max_stack_depth":0,
            "max_register_index":max_register_index,
            "exception_region_count":exception_count,
            "constant_count":constant_count
        }
    });
    let manifest = json!({
        "format":"ogma.ogmb",
        "version":"1.0",
        "encoding_id":ogma_bytecode::REGISTER_ENCODING_ID,
        "code_sha256":"0".repeat(64),
        "resource_limits":{
            "max_code_bytes":67_108_864u32,
            "max_instructions":1_000_000u32,
            "max_constants":262_144u32,
            "max_stack":65_536u32,
            "max_registers":65_536u32,
            "max_locals":65_536u32,
            "max_call_depth":4_096u32,
            "max_exception_regions":65_536u32
        },
        "metadata":metadata
    });

    let bytes = build_ogmb_v1(manifest, &code, &constants.values, &[], Some(&source_map))?;
    let artifact = verify_register_artifact(&bytes, &entry_registers)?;
    let metrics = CodegenMetrics {
        instruction_count,
        code_bytes: artifact.code.len(),
        constant_count: artifact.constants.len(),
        register_count,
    };
    Ok(CompiledFunction {
        id: function.id.clone(),
        name: function.name.clone(),
        metrics,
        artifact,
    })
}

#[derive(Default)]
struct RegisterAllocator {
    next: u32,
}

impl RegisterAllocator {
    fn allocate(&mut self) -> Result<u16, Vec<Diagnostic>> {
        if self.next > u16::MAX as u32 {
            return Err(vec![diagnostic(
                "resource.register_count",
                "$.registers",
                "register allocation exceeded the 16-bit operand range",
            )]);
        }
        let result = self.next as u16;
        self.next += 1;
        Ok(result)
    }

    fn count(&self) -> u32 {
        self.next
    }
}

#[derive(Default)]
struct ConstantPool {
    values: Vec<Value>,
    indices: BTreeMap<(u8, String), u32>,
}

impl ConstantPool {
    fn insert(&mut self, kind: ConstantKind, value: Value) -> Result<u32, Vec<Diagnostic>> {
        if !(value.is_null() || value.is_boolean() || value.is_number() || value.is_string()) {
            return Err(vec![diagnostic(
                "constant_pool.value",
                "$.sections.constants",
                "constant pool values must be JSON scalars",
            )]);
        }
        let canonical = serde_json::to_string(&value).map_err(|error| {
            vec![diagnostic(
                "constant_pool.serialize",
                "$.sections.constants",
                error.to_string(),
            )]
        })?;
        let key = (kind as u8, canonical);
        if let Some(index) = self.indices.get(&key) {
            return Ok(*index);
        }
        let index = u32::try_from(self.values.len()).map_err(|_| {
            vec![diagnostic(
                "resource.constant_count",
                "$.sections.constants",
                "constant pool exceeds uint32 indexing",
            )]
        })?;
        self.values.push(value);
        self.indices.insert(key, index);
        Ok(index)
    }
}

struct PlannedBlock {
    id: String,
    instructions: Vec<PlannedInstruction>,
}

struct PlannedInstruction {
    operation: PlannedOperation,
    may_throw: bool,
    source: Source,
}

enum PlannedOperation {
    Ready(RegisterOperation),
    Jump(String),
    Branch {
        condition: u16,
        then_target: String,
        else_target: String,
    },
}

impl PlannedInstruction {
    fn encoded_len(&self) -> usize {
        match &self.operation {
            PlannedOperation::Ready(operation) => RegisterInstruction {
                operation: operation.clone(),
                may_throw: self.may_throw,
            }
            .encoded_len(),
            PlannedOperation::Jump(_) => 6,
            PlannedOperation::Branch { .. } => 12,
        }
    }

    fn resolve(
        &self,
        offsets: &BTreeMap<String, u32>,
    ) -> Result<RegisterOperation, Vec<Diagnostic>> {
        match &self.operation {
            PlannedOperation::Ready(operation) => Ok(operation.clone()),
            PlannedOperation::Jump(target) => offsets
                .get(target)
                .copied()
                .map(|target| RegisterOperation::Jump { target })
                .ok_or_else(|| {
                    vec![diagnostic(
                        "bytecode.target_missing",
                        "$.instructions",
                        format!("jump target block {target:?} is missing"),
                    )]
                }),
            PlannedOperation::Branch {
                condition,
                then_target,
                else_target,
            } => {
                let then_target = offsets.get(then_target).copied().ok_or_else(|| {
                    vec![diagnostic(
                        "bytecode.target_missing",
                        "$.instructions",
                        "branch true target is missing",
                    )]
                })?;
                let else_target = offsets.get(else_target).copied().ok_or_else(|| {
                    vec![diagnostic(
                        "bytecode.target_missing",
                        "$.instructions",
                        "branch false target is missing",
                    )]
                })?;
                Ok(RegisterOperation::Branch {
                    condition: *condition,
                    then_target,
                    else_target,
                })
            }
        }
    }
}

struct EdgeView<'a> {
    target: &'a str,
    arguments: &'a [String],
}

fn materialize_edge(
    function: &Function,
    edge: EdgeView<'_>,
    source: &Source,
    values: &HashMap<String, u16>,
    allocator: &mut RegisterAllocator,
    plans: &mut Vec<PlannedBlock>,
    bridge_counter: &mut usize,
) -> Result<String, Vec<Diagnostic>> {
    let target = function
        .blocks
        .iter()
        .find(|block| block.id == edge.target)
        .ok_or_else(|| {
            vec![diagnostic(
                "bytecode.target_missing",
                "$.function.blocks",
                format!("edge target {:?} does not exist", edge.target),
            )]
        })?;
    let value_parameters: Vec<_> = target
        .parameters
        .iter()
        .filter_map(|parameter| {
            if let BlockParameter::Value(value) = parameter {
                Some(value)
            } else {
                None
            }
        })
        .collect();
    if edge.arguments.len() != target.parameters.len() {
        return Err(vec![diagnostic(
            "bytecode.edge_arity",
            "$.function.blocks",
            format!(
                "edge to {:?} supplies {} arguments for {} block parameters",
                edge.target,
                edge.arguments.len(),
                target.parameters.len()
            ),
        )]);
    }
    if value_parameters.is_empty() {
        return Ok(edge.target.to_owned());
    }

    let mut source_registers = Vec::with_capacity(value_parameters.len());
    let mut target_registers = Vec::with_capacity(value_parameters.len());
    for (parameter, argument) in target.parameters.iter().zip(edge.arguments) {
        if let BlockParameter::Value(value) = parameter {
            source_registers.push(value_register(
                values,
                argument,
                "$.function.edge.arguments",
            )?);
            let destination = values.get(&value.id).copied().ok_or_else(|| {
                vec![diagnostic(
                    "bytecode.block_parameter",
                    "$.function.blocks",
                    "target block parameter has no register",
                )]
            })?;
            target_registers.push(destination);
        }
    }

    let bridge_id = loop {
        let candidate = format!("ogma_bridge_{}", *bridge_counter);
        *bridge_counter += 1;
        if !plans.iter().any(|plan| plan.id == candidate) {
            break candidate;
        }
    };
    let generated = generated_source(
        function,
        "forward SSA block arguments through a parallel-copy bridge",
        Vec::new(),
    );
    let mut bridge = Vec::new();
    let mut temporaries = Vec::with_capacity(source_registers.len());
    for source_register in source_registers {
        let temporary = allocator.allocate()?;
        temporaries.push(temporary);
        bridge.push(PlannedInstruction {
            operation: PlannedOperation::Ready(RegisterOperation::Unary {
                destination: temporary,
                source: source_register,
                operation: UnaryOperation::Copy,
            }),
            may_throw: false,
            source: generated.clone(),
        });
    }
    for (temporary, destination) in temporaries.into_iter().zip(target_registers) {
        bridge.push(PlannedInstruction {
            operation: PlannedOperation::Ready(RegisterOperation::Unary {
                destination,
                source: temporary,
                operation: UnaryOperation::Copy,
            }),
            may_throw: false,
            source: generated.clone(),
        });
    }
    bridge.push(PlannedInstruction {
        operation: PlannedOperation::Jump(edge.target.to_owned()),
        may_throw: false,
        source: source.clone(),
    });
    plans.push(PlannedBlock {
        id: bridge_id.clone(),
        instructions: bridge,
    });
    Ok(bridge_id)
}

fn emit_binding_initializers(
    function: &Function,
    binding_registers: &HashMap<String, u16>,
    captured_cells: &std::collections::BTreeSet<String>,
    allocator: &mut RegisterAllocator,
    constants: &mut ConstantPool,
    output: &mut Vec<PlannedInstruction>,
) -> Result<(), Vec<Diagnostic>> {
    for binding in &function.environment.bindings {
        if matches!(binding.kind, BindingKind::Catch | BindingKind::Import) {
            return Err(vec![diagnostic(
                "bytecode.binding_unsupported",
                "$.function.environment.bindings",
                "catch and import binding initialization is outside the M1 register profile",
            )]);
        }

        let is_cell = captured_cells.contains(&binding.id);
        // Every binding slot is initialized structurally before the first
        // env.store reads its l-value register. The canonical Core IR stores
        // below then perform parameter/var/declaration initialization.
        let kind = match binding.kind {
            BindingKind::Var => ConstantKind::Undefined,
            BindingKind::Let | BindingKind::Const | BindingKind::Parameter => {
                ConstantKind::Uninitialized
            }
            BindingKind::Catch | BindingKind::Import => unreachable!("rejected above"),
        };
        let destination = binding_registers.get(&binding.id).copied().ok_or_else(|| {
            vec![diagnostic(
                "metadata.binding_location",
                "$.function.environment.bindings",
                format!("binding {:?} has no allocated register", binding.id),
            )]
        })?;
        let constant = constants.insert(kind, Value::Null)?;
        let source = generated_source(
            function,
            "initialize lexical binding state",
            vec![binding.id.clone()],
        );

        if is_cell {
            let initial_value = allocator.allocate()?;
            output.push(PlannedInstruction {
                operation: PlannedOperation::Ready(RegisterOperation::Constant {
                    destination: initial_value,
                    constant,
                    kind,
                }),
                may_throw: false,
                source: source.clone(),
            });
            output.push(PlannedInstruction {
                operation: PlannedOperation::Ready(RegisterOperation::Call {
                    destination,
                    kind: CallKind::MakeCell,
                    sources: vec![initial_value],
                }),
                may_throw: true,
                source,
            });
        } else {
            output.push(PlannedInstruction {
                operation: PlannedOperation::Ready(RegisterOperation::Constant {
                    destination,
                    constant,
                    kind,
                }),
                may_throw: false,
                source,
            });
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn lower_instruction(
    function: &Function,
    instruction: &ogma_core_ir::Instruction,
    values: &HashMap<String, u16>,
    bindings: &HashMap<String, u16>,
    captures: &HashMap<String, u16>,
    captured_cells: &std::collections::BTreeSet<String>,
    const_definition_instructions: &std::collections::BTreeSet<String>,
    allocator: &mut RegisterAllocator,
    constants: &mut ConstantPool,
) -> Result<Vec<PlannedInstruction>, Vec<Diagnostic>> {
    let destination = instruction
        .result
        .as_ref()
        .map(|result| {
            values.get(&result.id).copied().ok_or_else(|| {
                vec![diagnostic(
                    "bytecode.result_register",
                    format!("$.instructions.{}.result", instruction.id),
                    "result value has no allocated register",
                )]
            })
        })
        .transpose()?;
    let source = instruction.source.clone();
    let may_throw = instruction.effects.may_throw;
    let mut output = Vec::new();

    let reg = |id: &str| {
        value_register(
            values,
            id,
            &format!("$.function.instructions.{}.operands", instruction.id),
        )
    };
    macro_rules! emit {
        ($operation:expr, $may_throw:expr, $source:expr) => {
            output.push(PlannedInstruction {
                operation: PlannedOperation::Ready($operation),
                may_throw: $may_throw,
                source: $source.clone(),
            });
        };
    }

    match instruction.op.as_str() {
        "js.constant" => {
            let destination = required_destination(destination, &instruction.id)?;
            let literal = instruction
                .attributes
                .get("literal")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    vec![diagnostic(
                        "bytecode.constant_literal",
                        "$.function.instructions",
                        "constant literal tag is missing",
                    )]
                })?;
            let value = instruction
                .attributes
                .get("value")
                .cloned()
                .ok_or_else(|| {
                    vec![diagnostic(
                        "bytecode.constant_value",
                        "$.function.instructions",
                        "constant value is missing",
                    )]
                })?;
            let kind = match literal {
                "undefined" => ConstantKind::Undefined,
                "null" => ConstantKind::Null,
                "boolean" => ConstantKind::Boolean,
                "number" => ConstantKind::Number,
                "bigint" => ConstantKind::BigInt,
                "string" => ConstantKind::String,
                _ => {
                    return Err(vec![diagnostic(
                        "bytecode.constant_literal",
                        "$.function.instructions",
                        format!("unsupported JavaScript literal {literal:?}"),
                    )]);
                }
            };
            let constant = constants.insert(kind, value)?;
            emit!(
                RegisterOperation::Constant {
                    destination,
                    constant,
                    kind
                },
                false,
                source.clone()
            );
        }
        "js.to_boolean" | "js.to_number" | "js.to_string" | "js.typeof" => {
            let destination = required_destination(destination, &instruction.id)?;
            let operand = one_operand(instruction, &reg)?;
            let operation = match instruction.op.as_str() {
                "js.to_boolean" => UnaryOperation::ToBoolean,
                "js.to_number" => UnaryOperation::ToNumber,
                "js.to_string" => UnaryOperation::ToString,
                _ => UnaryOperation::TypeOf,
            };
            emit!(
                RegisterOperation::Unary {
                    destination,
                    source: operand,
                    operation
                },
                may_throw,
                source
            );
        }
        "js.add" | "js.strict_equal" | "js.less_than" | "js.greater_than" => {
            let destination = required_destination(destination, &instruction.id)?;
            let mut operands = unique_registers(
                instruction
                    .operands
                    .iter()
                    .map(|operand| reg(operand))
                    .collect::<Result<Vec<_>, _>>()?,
                allocator,
                &source,
                &mut output,
            )?;
            if operands.len() != 2 {
                return Err(vec![diagnostic(
                    "bytecode.operation_arity",
                    "$.function.instructions",
                    "binary operation requires exactly two operands",
                )]);
            }
            let operation = match instruction.op.as_str() {
                "js.add" => BinaryOperation::Add,
                "js.strict_equal" => BinaryOperation::StrictEqual,
                "js.less_than" => BinaryOperation::LessThan,
                _ => BinaryOperation::GreaterThan,
            };
            emit!(
                RegisterOperation::Binary {
                    destination,
                    left: operands.remove(0),
                    right: operands.remove(0),
                    operation,
                },
                may_throw,
                source
            );
        }
        "js.get_property" | "js.set_property" => {
            let key = instruction
                .attributes
                .get("key")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    vec![diagnostic(
                        "bytecode.property_key",
                        "$.function.instructions",
                        "named property operation requires a string key",
                    )]
                })?;
            let key_register = constant_register(
                key,
                ConstantKind::String,
                constants,
                allocator,
                &source,
                &mut output,
            )?;
            let mut sources = instruction
                .operands
                .iter()
                .map(|operand| reg(operand))
                .collect::<Result<Vec<_>, _>>()?;
            sources.insert(1, key_register);
            sources = unique_registers(sources, allocator, &source, &mut output)?;
            let destination = required_destination(destination, &instruction.id)?;
            let kind = if instruction.op == "js.get_property" {
                CallKind::GetProperty
            } else {
                CallKind::SetProperty
            };
            emit!(
                RegisterOperation::Call {
                    destination,
                    kind,
                    sources
                },
                may_throw,
                source
            );
        }
        "js.get_element" | "js.set_element" => {
            let sources = unique_registers(
                instruction
                    .operands
                    .iter()
                    .map(|operand| reg(operand))
                    .collect::<Result<Vec<_>, _>>()?,
                allocator,
                &source,
                &mut output,
            )?;
            let destination = required_destination(destination, &instruction.id)?;
            let kind = if instruction.op == "js.get_element" {
                CallKind::GetElement
            } else {
                CallKind::SetElement
            };
            emit!(
                RegisterOperation::Call {
                    destination,
                    kind,
                    sources
                },
                may_throw,
                source
            );
        }
        "js.call" | "js.construct" => {
            let sources = unique_registers(
                instruction
                    .operands
                    .iter()
                    .map(|operand| reg(operand))
                    .collect::<Result<Vec<_>, _>>()?,
                allocator,
                &source,
                &mut output,
            )?;
            let destination = required_destination(destination, &instruction.id)?;
            let kind = if instruction.op == "js.call" {
                CallKind::JavaScript
            } else {
                CallKind::Construct
            };
            emit!(
                RegisterOperation::Call {
                    destination,
                    kind,
                    sources
                },
                may_throw,
                source
            );
        }
        "host.call" => {
            let symbol = instruction
                .attributes
                .get("symbol")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    vec![diagnostic(
                        "bytecode.host_symbol",
                        "$.function.instructions",
                        "host.call requires a capability symbol",
                    )]
                })?;
            let token = constant_register(
                symbol,
                ConstantKind::HostCapability,
                constants,
                allocator,
                &source,
                &mut output,
            )?;
            let mut sources = vec![token];
            sources.extend(
                instruction
                    .operands
                    .iter()
                    .map(|operand| reg(operand))
                    .collect::<Result<Vec<_>, _>>()?,
            );
            sources = unique_registers(sources, allocator, &source, &mut output)?;
            let destination = required_destination(destination, &instruction.id)?;
            emit!(
                RegisterOperation::Call {
                    destination,
                    kind: CallKind::Host,
                    sources
                },
                may_throw,
                source
            );
        }
        "closure.create" => {
            let function_id = instruction
                .attributes
                .get("function")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    vec![diagnostic(
                        "bytecode.closure_function",
                        "$.function.instructions",
                        "closure.create requires a target function identity",
                    )]
                })?;
            let function_register = constant_register(
                function_id,
                ConstantKind::FunctionId,
                constants,
                allocator,
                &source,
                &mut output,
            )?;
            let descriptors = instruction
                .attributes
                .get("captures")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    vec![diagnostic(
                        "bytecode.closure_captures",
                        "$.function.instructions",
                        "closure captures must be an array",
                    )]
                })?;
            let mut sources = vec![function_register];
            for descriptor in descriptors {
                let binding = descriptor
                    .get("binding")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        vec![diagnostic(
                            "bytecode.closure_capture_binding",
                            "$.function.instructions",
                            "capture descriptor has no binding identity",
                        )]
                    })?;
                let register = bindings
                    .get(binding)
                    .or_else(|| captures.get(binding))
                    .copied()
                    .ok_or_else(|| {
                        vec![diagnostic(
                            "bytecode.closure_capture_register",
                            "$.function.instructions",
                            format!("capture source {binding:?} has no frame register"),
                        )]
                    })?;
                let mode = descriptor
                    .get("mode")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        vec![diagnostic(
                            "bytecode.closure_capture_mode",
                            "$.function.instructions",
                            "capture descriptor requires a cell/value mode",
                        )]
                    })?;
                let source_is_cell = captured_cells.contains(binding)
                    || function
                        .captures
                        .iter()
                        .any(|capture| capture.id == binding && capture.mode == CaptureMode::Cell);
                if (mode == "cell") != source_is_cell {
                    return Err(vec![diagnostic(
                        "bytecode.closure_capture_mode",
                        "$.function.instructions",
                        format!(
                            "capture {binding:?} expects mode {mode:?}, but its source register has a different storage mode"
                        ),
                    )]);
                }
                sources.push(register);
            }
            sources = unique_registers(sources, allocator, &source, &mut output)?;
            let destination = required_destination(destination, &instruction.id)?;
            emit!(
                RegisterOperation::Call {
                    destination,
                    kind: CallKind::MakeClosure,
                    sources
                },
                may_throw,
                source
            );
        }
        "env.load" => {
            let alias = instruction
                .attributes
                .get("binding")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    vec![diagnostic(
                        "bytecode.binding_id",
                        "$.function.instructions",
                        "env.load requires a binding identity",
                    )]
                })?;
            let destination = required_destination(destination, &instruction.id)?;
            if let Some(register) = bindings.get(alias).copied() {
                if captured_cells.contains(alias) {
                    emit!(
                        RegisterOperation::Call {
                            destination,
                            kind: CallKind::LoadCapture,
                            sources: vec![register],
                        },
                        may_throw,
                        source
                    );
                } else {
                    emit!(
                        RegisterOperation::Unary {
                            destination,
                            source: register,
                            operation: UnaryOperation::LoadBinding,
                        },
                        may_throw,
                        source
                    );
                }
            } else if let Some(register) = captures.get(alias).copied() {
                let capture = function
                    .captures
                    .iter()
                    .find(|capture| capture.id == alias || capture.binding == alias)
                    .ok_or_else(|| {
                        vec![diagnostic(
                            "bytecode.capture_metadata",
                            "$.function.captures",
                            "capture register has no matching descriptor",
                        )]
                    })?;
                if capture.mode == CaptureMode::Cell {
                    emit!(
                        RegisterOperation::Call {
                            destination,
                            kind: CallKind::LoadCapture,
                            sources: vec![register],
                        },
                        may_throw,
                        source
                    );
                } else {
                    emit!(
                        RegisterOperation::Unary {
                            destination,
                            source: register,
                            operation: UnaryOperation::Copy,
                        },
                        may_throw,
                        source
                    );
                }
            } else {
                return Err(vec![diagnostic(
                    "bytecode.binding_id",
                    "$.function.instructions",
                    format!("unknown binding {alias:?}"),
                )]);
            }
        }
        "env.store" => {
            let alias = instruction
                .attributes
                .get("binding")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    vec![diagnostic(
                        "bytecode.binding_id",
                        "$.function.instructions",
                        "env.store requires a binding identity",
                    )]
                })?;
            let value = one_operand(instruction, &reg)?;
            let destination = required_destination(destination, &instruction.id)?;
            if let Some(register) = bindings.get(alias).copied() {
                if captured_cells.contains(alias) {
                    let sources =
                        unique_registers(vec![register, value], allocator, &source, &mut output)?;
                    emit!(
                        RegisterOperation::Call {
                            destination,
                            kind: CallKind::StoreCapture,
                            sources,
                        },
                        may_throw,
                        source
                    );
                } else {
                    let binding_token = constant_register(
                        alias,
                        ConstantKind::BindingId,
                        constants,
                        allocator,
                        &source,
                        &mut output,
                    )?;
                    let sources = unique_registers(
                        vec![binding_token, value],
                        allocator,
                        &source,
                        &mut output,
                    )?;
                    emit!(
                        RegisterOperation::Call {
                            destination,
                            kind: CallKind::StoreBinding,
                            sources,
                        },
                        may_throw,
                        source
                    );
                }
            } else if let Some(register) = captures.get(alias).copied() {
                let capture = function
                    .captures
                    .iter()
                    .find(|capture| capture.id == alias || capture.binding == alias)
                    .ok_or_else(|| {
                        vec![diagnostic(
                            "bytecode.capture_metadata",
                            "$.function.captures",
                            "capture register has no matching descriptor",
                        )]
                    })?;
                if capture.mode != CaptureMode::Cell {
                    return Err(vec![diagnostic(
                        "bytecode.capture_immutable",
                        "$.function.instructions",
                        "cannot store through a by-value capture",
                    )]);
                }
                let sources =
                    unique_registers(vec![register, value], allocator, &source, &mut output)?;
                emit!(
                    RegisterOperation::Call {
                        destination,
                        kind: CallKind::StoreCapture,
                        sources,
                    },
                    may_throw,
                    source
                );
            } else {
                return Err(vec![diagnostic(
                    "bytecode.binding_id",
                    "$.function.instructions",
                    format!("unknown binding {alias:?}"),
                )]);
            }
        }
        "js.throw" => {
            let value = one_operand(instruction, &reg)?;
            emit!(RegisterOperation::Throw { source: value }, true, source);
        }
        "async.await" => {
            return Err(vec![diagnostic(
                "bytecode.suspension_unsupported",
                "$.function.instructions",
                "async.await is not admitted by the synchronous M1 register profile",
            )]);
        }
        operation => {
            return Err(vec![diagnostic(
                "bytecode.operation_unsupported",
                format!("$.function.instructions.{}", instruction.id),
                format!("Core IR operation {operation:?} has no M1 register lowering"),
            )]);
        }
    }
    if let Some(result_register) = destination
        && const_definition_instructions.contains(&instruction.id)
    {
        for binding in source_bindings(&source) {
            if const_binding_is_local(function, &binding) {
                if let Some(binding_register) = bindings.get(&binding).copied() {
                    if binding_register != result_register {
                        output.push(PlannedInstruction {
                            operation: PlannedOperation::Ready(RegisterOperation::Unary {
                                destination: binding_register,
                                source: result_register,
                                operation: UnaryOperation::Copy,
                            }),
                            may_throw: false,
                            source: source.clone(),
                        });
                    }
                }
            }
        }
    }
    Ok(output)
}

fn constant_register(
    value: &str,
    kind: ConstantKind,
    constants: &mut ConstantPool,
    allocator: &mut RegisterAllocator,
    source: &Source,
    output: &mut Vec<PlannedInstruction>,
) -> Result<u16, Vec<Diagnostic>> {
    let destination = allocator.allocate()?;
    let constant = constants.insert(kind, Value::String(value.to_owned()))?;
    output.push(PlannedInstruction {
        operation: PlannedOperation::Ready(RegisterOperation::Constant {
            destination,
            constant,
            kind,
        }),
        may_throw: false,
        source: source.clone(),
    });
    Ok(destination)
}

fn unique_registers(
    sources: Vec<u16>,
    allocator: &mut RegisterAllocator,
    source: &Source,
    output: &mut Vec<PlannedInstruction>,
) -> Result<Vec<u16>, Vec<Diagnostic>> {
    let mut seen = std::collections::BTreeSet::new();
    let mut unique = Vec::with_capacity(sources.len());
    for register in sources {
        if seen.insert(register) {
            unique.push(register);
        } else {
            let temporary = allocator.allocate()?;
            output.push(PlannedInstruction {
                operation: PlannedOperation::Ready(RegisterOperation::Unary {
                    destination: temporary,
                    source: register,
                    operation: UnaryOperation::Copy,
                }),
                may_throw: false,
                source: source.clone(),
            });
            seen.insert(temporary);
            unique.push(temporary);
        }
    }
    Ok(unique)
}

fn one_operand<F>(
    instruction: &ogma_core_ir::Instruction,
    register: &F,
) -> Result<u16, Vec<Diagnostic>>
where
    F: Fn(&str) -> Result<u16, Vec<Diagnostic>>,
{
    if instruction.operands.len() != 1 {
        return Err(vec![diagnostic(
            "bytecode.operation_arity",
            format!("$.function.instructions.{}", instruction.id),
            "operation requires exactly one operand",
        )]);
    }
    register(&instruction.operands[0])
}

fn required_destination(
    destination: Option<u16>,
    instruction_id: &str,
) -> Result<u16, Vec<Diagnostic>> {
    destination.ok_or_else(|| {
        vec![diagnostic(
            "bytecode.result_missing",
            format!("$.function.instructions.{instruction_id}.result"),
            "operation requires a destination register",
        )]
    })
}

fn value_register(
    values: &HashMap<String, u16>,
    id: &str,
    path: &str,
) -> Result<u16, Vec<Diagnostic>> {
    values.get(id).copied().ok_or_else(|| {
        vec![diagnostic(
            "bytecode.value_register",
            path,
            format!("SSA value {id:?} has no allocated register"),
        )]
    })
}

fn source_bindings(source: &Source) -> Vec<String> {
    match source {
        Source::User { bindings, .. } | Source::Generated { bindings, .. } => bindings.clone(),
    }
}

fn const_binding_is_local(function: &Function, id: &str) -> bool {
    function
        .environment
        .bindings
        .iter()
        .any(|binding| binding.id == id && binding.kind == BindingKind::Const)
}

fn generated_source(function: &Function, reason: &str, bindings: Vec<String>) -> Source {
    Source::Generated {
        reason: reason.to_owned(),
        scope: Some(function.environment.id.clone()),
        bindings,
        suspension: None,
        inline_chain: Vec::new(),
    }
}

fn normalized_source(source: &Source, function: &Function) -> Result<Value, Vec<Diagnostic>> {
    let mut value = serde_json::to_value(source).map_err(|error| {
        vec![diagnostic(
            "source_map.serialize",
            "$.source_map.locations",
            error.to_string(),
        )]
    })?;
    let aliases: HashMap<&str, &str> = function
        .captures
        .iter()
        .map(|capture| (capture.binding.as_str(), capture.id.as_str()))
        .collect();
    if let Some(bindings) = value.get_mut("bindings").and_then(Value::as_array_mut) {
        for binding in bindings.iter_mut() {
            if let Some(id) = binding.as_str().and_then(|id| aliases.get(id).copied()) {
                *binding = Value::String(id.to_owned());
            }
        }
        bindings.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
        bindings.dedup();
    }
    Ok(value)
}

fn maximum_used_register(
    instructions: &[RegisterInstruction],
    entry_registers: &[u16],
) -> Option<u32> {
    let mut maximum = entry_registers.iter().copied().max();
    for instruction in instructions {
        let registers: Vec<u16> = match &instruction.operation {
            RegisterOperation::Constant { destination, .. } => vec![*destination],
            RegisterOperation::Unary {
                destination,
                source,
                ..
            } => vec![*destination, *source],
            RegisterOperation::Binary {
                destination,
                left,
                right,
                ..
            } => vec![*destination, *left, *right],
            RegisterOperation::Call {
                destination,
                sources,
                ..
            } => {
                let mut result = vec![*destination];
                result.extend(sources.iter().copied());
                result
            }
            RegisterOperation::Jump { .. } => Vec::new(),
            RegisterOperation::Branch { condition, .. } => vec![*condition],
            RegisterOperation::Return { source } | RegisterOperation::Throw { source } => {
                vec![*source]
            }
        };
        if let Some(register) = registers.into_iter().max() {
            maximum = Some(maximum.map_or(register, |existing| existing.max(register)));
        }
    }
    maximum.map(u32::from)
}

fn diagnostic(code: &str, path: impl Into<String>, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        code: code.to_owned(),
        path: path.into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ogma_core_ir::{
        BasicBlock, Capture, CaptureMode, Function, FunctionDocument, Representation,
        ReturnSignature, SemanticType, SourceFile, SourceLanguage, Terminator,
    };

    fn function_document() -> FunctionDocument {
        FunctionDocument {
            contract: "ogma.core-ir".to_owned(),
            version: "1.0.0".to_owned(),
            function: Function {
                id: "ir_test".to_owned(),
                name: "constant_return".to_owned(),
                parameters: Vec::new(),
                return_signature: Some(ReturnSignature {
                    semantic_type: SemanticType::Number,
                    representation: Representation::Boxed,
                }),
                source_files: vec![SourceFile {
                    id: "source_test".to_owned(),
                    path: "fixtures/constant.js".to_owned(),
                    language: SourceLanguage::Javascript,
                    sha256: None,
                }],
                environment: ogma_core_ir::Environment {
                    id: "env_test".to_owned(),
                    parent: None,
                    bindings: Vec::new(),
                },
                captures: Vec::new(),
                entry_block: "entry".to_owned(),
                effect_entry: None,
                blocks: vec![BasicBlock {
                    id: "entry".to_owned(),
                    parameters: Vec::new(),
                    exception_parameter: None,
                    instructions: vec![ogma_core_ir::Instruction {
                        id: "i0".to_owned(),
                        op: "js.constant".to_owned(),
                        operands: Vec::new(),
                        result: Some(ogma_core_ir::ValueDefinition {
                            id: "v0".to_owned(),
                            kind: "value".to_owned(),
                            semantic_type: SemanticType::Number,
                            representation: Representation::Boxed,
                        }),
                        attributes: BTreeMap::from([
                            ("literal".to_owned(), json!("number")),
                            ("value".to_owned(), json!(42)),
                        ]),
                        effects: Default::default(),
                        source: Source::User {
                            file: "source_test".to_owned(),
                            start: ogma_core_ir::SourcePosition { line: 1, column: 0 },
                            end: ogma_core_ir::SourcePosition {
                                line: 1,
                                column: 10,
                            },
                            scope: Some("env_test".to_owned()),
                            bindings: Vec::new(),
                            suspension: None,
                            inline_chain: Vec::new(),
                        },
                        effect_in: None,
                        effect_out: None,
                        exception: None,
                    }],
                    terminator: Terminator::Return {
                        value: Some("v0".to_owned()),
                        source: Source::User {
                            file: "source_test".to_owned(),
                            start: ogma_core_ir::SourcePosition { line: 1, column: 0 },
                            end: ogma_core_ir::SourcePosition {
                                line: 1,
                                column: 10,
                            },
                            scope: Some("env_test".to_owned()),
                            bindings: Vec::new(),
                            suspension: None,
                            inline_chain: Vec::new(),
                        },
                    },
                }],
            },
        }
    }

    #[test]
    fn normalized_source_rewrites_capture_aliases_without_moving_bindings() {
        let mut function = function_document().function;
        function.captures.push(Capture {
            id: "capture_0".to_owned(),
            source_environment: "env_parent".to_owned(),
            binding: "binding_outer".to_owned(),
            name: "outer".to_owned(),
            mutable: true,
            semantic_type: SemanticType::Number,
            representation: Representation::Boxed,
            mode: CaptureMode::Cell,
        });
        let source = Source::Generated {
            reason: "capture alias regression".to_owned(),
            scope: Some("env_test".to_owned()),
            bindings: vec![
                "binding_outer".to_owned(),
                "capture_0".to_owned(),
                "binding_outer".to_owned(),
            ],
            suspension: None,
            inline_chain: Vec::new(),
        };

        let normalized = normalized_source(&source, &function).expect("normalize source");
        assert_eq!(normalized["bindings"], json!(["capture_0"]));
    }

    #[test]
    fn lowers_a_verified_core_ir_function_to_deterministic_ogmb() {
        let document = function_document();
        let first = compile_function(&document).expect("first artifact");
        let second = compile_function(&document).expect("second artifact");
        assert_eq!(first.artifact.bytes, second.artifact.bytes);
        assert_eq!(first.metrics, second.metrics);
        assert_eq!(
            first.metrics.instruction_count as usize,
            first.artifact.verification_input["instructions"]
                .as_array()
                .unwrap()
                .len()
        );
        assert_eq!(first.metrics.code_bytes, first.artifact.code.len());
        assert_eq!(first.metrics.constant_count, first.artifact.constants.len());
        assert_eq!(
            first.metrics.register_count,
            first.artifact.manifest["metadata"]["frame_layout"]["register_count"]
                .as_u64()
                .unwrap() as u32
        );
        assert_eq!(
            first.artifact.verification_input["execution_model"],
            "register"
        );
        assert_eq!(
            first.artifact.source_map.as_ref().expect("source map")["locations"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn rejects_invalid_ssa_reference_before_emitting_bytecode() {
        let mut document = function_document();
        let source = match &document.function.blocks[0].terminator {
            Terminator::Return { source, .. } => source.clone(),
            _ => unreachable!("fixture has a return terminator"),
        };
        document.function.blocks[0].terminator = Terminator::Return {
            value: Some("v_missing".to_owned()),
            source,
        };
        assert!(!verify_document(&document).is_empty());
        assert!(compile_function(&document).is_err());
    }

    #[test]
    fn rejects_unsupported_core_ir_operation_before_emitting_bytecode() {
        let mut document = function_document();
        document.function.blocks[0].instructions[0].op = "js.unsupported_m1".to_owned();
        assert!(!verify_document(&document).is_empty());
        assert!(compile_function(&document).is_err());
    }

    #[test]
    fn rejects_invalid_control_flow_target_before_emitting_bytecode() {
        let mut document = function_document();
        let source = match &document.function.blocks[0].terminator {
            Terminator::Return { source, .. } => source.clone(),
            _ => unreachable!("fixture has a return terminator"),
        };
        document.function.blocks[0].terminator = Terminator::Jump {
            target: "bb_missing".to_owned(),
            arguments: Vec::new(),
            source,
        };
        assert!(!verify_document(&document).is_empty());
        assert!(compile_function(&document).is_err());
    }
}
