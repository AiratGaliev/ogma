//! End-to-end tests for verified OGMB loading and M1 interpreter execution.

use ogma_bytecode::{
    BinaryOperation, CallKind, ConstantKind, RegisterInstruction, RegisterOperation, build_ogmb_v1,
    encode_register_code,
};
use ogma_compiler::{CompileFailure, compile_to_bytecode};
use ogma_frontend::{ParseRequestOptions, SourceFile, parse};
use ogma_interpreter::{
    CancellationToken, ExecutableModule, ExecutionCompletion, ExecutionInterruption,
    ExecutionOptions, Host, HostError, InterpreterError, NoHost,
};
#[cfg(debug_assertions)]
use ogma_interpreter::{DebuggerEvent, DebuggerPort};
#[cfg(debug_assertions)]
use ogma_runtime::PropertyDescriptor;
use ogma_runtime::{Heap, HeapConfig, Value, ValueType};
use oxc_allocator::Allocator;
use serde_json::{Value as JsonValue, json};

#[allow(clippy::too_many_arguments)]
fn artifact(
    function_id: &str,
    environment_id: &str,
    parent_environment: Option<&str>,
    parameter_count: usize,
    register_count: usize,
    max_register_index: u32,
    instructions: Vec<RegisterInstruction>,
    constants: Vec<JsonValue>,
    bindings: Vec<JsonValue>,
    captures: Vec<JsonValue>,
    locations: Vec<JsonValue>,
) -> Vec<u8> {
    let actual_max_register_index = instructions
        .iter()
        .flat_map(|instruction| {
            let mut registers = Vec::new();
            match &instruction.operation {
                RegisterOperation::Constant { destination, .. }
                | RegisterOperation::Unary { destination, .. } => {
                    registers.push(*destination);
                    if let RegisterOperation::Unary { source, .. } = &instruction.operation {
                        registers.push(*source);
                    }
                }
                RegisterOperation::Binary {
                    destination,
                    left,
                    right,
                    ..
                } => registers.extend([*destination, *left, *right]),
                RegisterOperation::Call {
                    destination,
                    sources,
                    ..
                } => {
                    registers.push(*destination);
                    registers.extend(sources.iter().copied());
                }
                RegisterOperation::Branch { condition, .. } => registers.push(*condition),
                RegisterOperation::Return { source } | RegisterOperation::Throw { source } => {
                    registers.push(*source);
                }
                RegisterOperation::Jump { .. } => {}
            }
            registers
        })
        .max()
        .map_or(0, u32::from);
    assert_eq!(
        max_register_index, actual_max_register_index,
        "test manifest must declare the maximum register actually used by its instructions"
    );
    let code = encode_register_code(&instructions).expect("valid test bytecode");
    let manifest = json!({
        "format": "ogma.ogmb",
        "version": "1.0",
        "encoding_id": "ogma.register.m1.v1",
        "code_sha256": "0".repeat(64),
        "resource_limits": {
            "max_code_bytes": 65536,
            "max_instructions": 1000,
            "max_constants": 1000,
            "max_stack": 4096,
            "max_registers": 65536,
            "max_locals": 65536,
            "max_call_depth": 64,
            "max_exception_regions": 1000
        },
        "metadata": {
            "execution_model": "register",
            "function": {"id": function_id, "name": function_id},
            "code_generation": {"id": "ogma_register_codegen", "version": "1.0.0"},
            "code_cache": {"id": format!("cache_{function_id}"), "version": "1.0.0"},
            "frame_layout": {
                "parameter_count": parameter_count,
                "local_count": 0,
                "register_count": register_count
            },
            "environment": {
                "id": environment_id,
                "parent": parent_environment,
                "bindings": bindings,
                "captures": captures,
                "locations": locations
            },
            "suspension_points": [],
            "inline_cache_slots": [],
            "verifier_summary": {
                "schema": "ogma.bytecode-verification-summary",
                "version": "1.0.0",
                "code_sha256": "0".repeat(64),
                "instruction_count": instructions.len(),
                "max_stack_depth": 0,
                "max_register_index": actual_max_register_index,
                "exception_region_count": 0,
                "constant_count": constants.len()
            }
        }
    });
    build_ogmb_v1(manifest, &code, &constants, &[], None).expect("valid OGMB test artifact")
}

fn number_constant(destination: u16, index: u32) -> RegisterInstruction {
    RegisterInstruction {
        operation: RegisterOperation::Constant {
            destination,
            constant: index,
            kind: ConstantKind::Number,
        },
        may_throw: false,
    }
}

fn return_register(source: u16) -> RegisterInstruction {
    RegisterInstruction {
        operation: RegisterOperation::Return { source },
        may_throw: false,
    }
}

fn empty_bindings() -> (Vec<JsonValue>, Vec<JsonValue>) {
    (Vec::new(), Vec::new())
}

fn module_from_core_ir_fixture(source: &str) -> ExecutableModule {
    let document: ogma_core_ir::FunctionDocument =
        serde_json::from_str(source).expect("canonical Core IR v1 fixture");
    let functions = ogma_compiler::bytecode::compile_functions(&[document])
        .expect("fixture compiles through the Rust Core IR-to-OGMB backend");
    let artifacts = functions
        .into_iter()
        .map(|function| function.artifact.bytes)
        .collect::<Vec<_>>();
    ExecutableModule::load(artifacts)
        .expect("interpreter re-verifies every serialized fixture artifact")
}

fn execute_number(
    module: &ExecutableModule,
    entry: &str,
    arguments: &[Value],
    heap: &mut Heap,
) -> f64 {
    let report = module
        .execute_without_host(heap, entry, arguments, ExecutionOptions::default())
        .expect("verified Core IR fixture executes");
    match report.completion {
        ExecutionCompletion::Return(value) => value.as_number().expect("fixture returns a Number"),
        completion => panic!("expected normal fixture completion, got {completion:?}"),
    }
}

#[test]
fn compiler_rejects_arrow_functions_until_ogmb_preserves_function_kind() {
    let allocator = Allocator::default();
    let source = SourceFile::new(
        "ogm46-arrow-function-profile",
        "memory:///ogm46-arrow-function-profile.js",
        None,
        "function run(seed) { const read = () => seed; return read(); }",
    )
    .expect("source is valid");
    let parsed = parse(&allocator, &source, ParseRequestOptions::default())
        .expect("arrow function source parses");
    let failure =
        compile_to_bytecode(&parsed).expect_err("OGMB v1 must not erase the arrow function kind");
    let CompileFailure::Semantic(diagnostics) = failure else {
        panic!("unsupported arrow metadata must be reported as a semantic diagnostic");
    };
    let diagnostic = diagnostics
        .iter()
        .find(|item| item.code == "semantic.arrow_function_metadata_unsupported")
        .expect("source-linked arrow metadata diagnostic");
    assert!(diagnostic.span.is_some());
    assert!(diagnostic.message.contains("function-kind"));
}

#[test]
fn executes_canonical_core_ir_branch_and_loop_fixtures_via_serialized_ogmb() {
    // These are the same checked-in Core IR inputs used by the OGM-31
    // cross-backend proof. This path intentionally compiles to OGMB first;
    // the interpreter never receives or evaluates Core IR directly.
    let branch = module_from_core_ir_fixture(include_str!(
        "../../../tests/core-ir/fixtures/positive-ssa-branch.json"
    ));
    let loop_module = module_from_core_ir_fixture(include_str!(
        "../../../tests/core-ir/fixtures/positive-loop-carried-values.json"
    ));
    let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");

    assert_eq!(
        execute_number(
            &branch,
            "fn0",
            &[Value::number(7.0), Value::number(7.0)],
            &mut heap
        ),
        7.0
    );
    assert_eq!(
        execute_number(
            &branch,
            "fn0",
            &[Value::number(7.0), Value::number(8.0)],
            &mut heap
        ),
        8.0
    );
    assert_eq!(
        execute_number(
            &loop_module,
            "fn_loop_carried",
            &[Value::number(0.0), Value::number(5.0), Value::number(1.0)],
            &mut heap
        ),
        5.0
    );
    assert_eq!(
        execute_number(
            &loop_module,
            "fn_loop_carried",
            &[Value::number(0.5), Value::number(3.5), Value::number(0.75)],
            &mut heap
        ),
        3.5
    );
}

#[derive(Default)]
struct CanonicalEchoHost {
    calls: Vec<(String, Vec<f64>)>,
}

impl Host for CanonicalEchoHost {
    fn call(
        &mut self,
        _heap: &mut Heap,
        capability: &str,
        arguments: &[Value],
    ) -> Result<Value, HostError> {
        if capability != "benchmark.host.echo" || arguments.len() != 1 {
            return Err(HostError::Denied);
        }
        let Some(argument) = arguments[0].as_number() else {
            return Err(HostError::Denied);
        };
        self.calls.push((capability.to_owned(), vec![argument]));
        Ok(Value::number(argument + 11.0))
    }
}

struct NestedExecutionHost {
    nested: ExecutableModule,
    observed_depth: Option<u32>,
}

impl Host for NestedExecutionHost {
    fn call(
        &mut self,
        _heap: &mut Heap,
        _capability: &str,
        _arguments: &[Value],
    ) -> Result<Value, HostError> {
        Err(HostError::Denied)
    }

    fn call_with_context(
        &mut self,
        context: &mut ogma_interpreter::HostCallContext<'_>,
        capability: &str,
        _arguments: &[Value],
    ) -> Result<ogma_interpreter::HostCallCompletion, HostError> {
        if capability != "benchmark.host.echo" {
            return Err(HostError::Denied);
        }
        self.observed_depth = Some(context.call_depth());
        let mut no_host = ogma_interpreter::NoHost;
        let nested = context
            .execute_nested(
                &self.nested,
                "fn0",
                &[Value::number(7.0), Value::number(7.0)],
                &mut no_host,
            )
            .map_err(|_| HostError::Panicked)?;
        match nested {
            ExecutionCompletion::Return(value) => {
                Ok(ogma_interpreter::HostCallCompletion::Normal(value))
            }
            ExecutionCompletion::Throw(value) => {
                Ok(ogma_interpreter::HostCallCompletion::Throw(value))
            }
            ExecutionCompletion::Interrupted(reason) => {
                Ok(ogma_interpreter::HostCallCompletion::Interrupted(reason))
            }
        }
    }
}

#[test]
fn host_callback_can_execute_a_nested_module_on_the_same_heap_and_budget() {
    let outer = module_from_core_ir_fixture(include_str!(
        "../../../tests/core-ir/fixtures/positive-host-call.json"
    ));
    let nested = module_from_core_ir_fixture(include_str!(
        "../../../tests/core-ir/fixtures/positive-ssa-branch.json"
    ));
    let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");
    let mut host = NestedExecutionHost {
        nested,
        observed_depth: None,
    };
    let report = outer
        .execute(
            &mut heap,
            "fn_host_call",
            &[Value::number(31.0)],
            &mut host,
            ExecutionOptions::default(),
        )
        .expect("outer function executes with a nested host call");

    assert_eq!(host.observed_depth, Some(1));
    assert_eq!(report.max_call_depth_reached, 2);
    assert!(report.max_stack_bytes_reached > 0);
    // Both the outer host-call fixture and nested branch fixture share fuel.
    assert_eq!(report.instructions_executed, 12);
    match report.completion {
        ExecutionCompletion::Return(value) => assert_eq!(value.as_number(), Some(7.0)),
        completion => panic!("expected nested Return completion, got {completion:?}"),
    }
}

#[test]
fn nested_host_execution_consumes_the_parent_instruction_fuel() {
    let outer = module_from_core_ir_fixture(include_str!(
        "../../../tests/core-ir/fixtures/positive-host-call.json"
    ));
    let nested = module_from_core_ir_fixture(include_str!(
        "../../../tests/core-ir/fixtures/positive-ssa-branch.json"
    ));
    let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");
    let mut host = NestedExecutionHost {
        nested,
        observed_depth: None,
    };
    let report = outer
        .execute(
            &mut heap,
            "fn_host_call",
            &[Value::number(31.0)],
            &mut host,
            ExecutionOptions {
                instruction_budget: 2,
                ..ExecutionOptions::default()
            },
        )
        .expect("nested execution reports cooperative fuel exhaustion");

    assert!(matches!(
        report.completion,
        ExecutionCompletion::Interrupted(ExecutionInterruption::FuelExhausted)
    ));
    assert_eq!(report.instructions_executed, 2);
}

#[test]
fn vm_stack_quota_is_enforced_at_exact_and_over_limit_boundaries() {
    let module = module_from_core_ir_fixture(include_str!(
        "../../../tests/core-ir/fixtures/positive-ssa-branch.json"
    ));
    let mut baseline_heap = Heap::new(HeapConfig::default()).expect("baseline heap");
    let mut baseline_host = NoHost;
    let baseline = module
        .execute(
            &mut baseline_heap,
            "fn0",
            &[Value::number(7.0), Value::number(7.0)],
            &mut baseline_host,
            ExecutionOptions {
                max_stack_bytes: usize::MAX,
                ..ExecutionOptions::default()
            },
        )
        .expect("baseline execution with a permissive stack quota");
    let required = baseline.max_stack_bytes_reached;
    assert!(required > 0);

    let mut exact_heap = Heap::new(HeapConfig::default()).expect("exact heap");
    let mut exact_host = NoHost;
    let exact = module
        .execute(
            &mut exact_heap,
            "fn0",
            &[Value::number(7.0), Value::number(7.0)],
            &mut exact_host,
            ExecutionOptions {
                max_stack_bytes: required,
                ..ExecutionOptions::default()
            },
        )
        .expect("an exact stack quota is sufficient");
    assert_eq!(exact.max_stack_bytes_reached, required);

    let mut limited_heap = Heap::new(HeapConfig::default()).expect("limited heap");
    let mut limited_host = NoHost;
    let error = module
        .execute(
            &mut limited_heap,
            "fn0",
            &[Value::number(7.0), Value::number(7.0)],
            &mut limited_host,
            ExecutionOptions {
                max_stack_bytes: required - 1,
                ..ExecutionOptions::default()
            },
        )
        .expect_err("one byte below the required frame allocation is rejected");
    assert!(matches!(
        error,
        InterpreterError::StackQuotaExceeded {
            limit_bytes,
            requested_bytes
        } if limit_bytes == required - 1 && requested_bytes == required
    ));
    let roots = limited_heap.root_snapshot();
    assert_eq!(roots.interpreter_frames, 0);
    assert_eq!(roots.temporary_scopes, 0);
}

#[test]
fn executes_canonical_core_ir_host_call_fixture_with_observable_host_trace() {
    let module = module_from_core_ir_fixture(include_str!(
        "../../../tests/core-ir/fixtures/positive-host-call.json"
    ));
    let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");
    let mut host = CanonicalEchoHost::default();
    let report = module
        .execute(
            &mut heap,
            "fn_host_call",
            &[Value::number(31.0)],
            &mut host,
            ExecutionOptions::default(),
        )
        .expect("host-backed Core IR fixture executes");

    match report.completion {
        ExecutionCompletion::Return(value) => assert_eq!(value.as_number(), Some(42.0)),
        completion => panic!("expected host-backed return, got {completion:?}"),
    }
    assert_eq!(
        host.calls,
        vec![("benchmark.host.echo".to_owned(), vec![31.0])]
    );
}

#[test]
fn executes_serialized_decoded_register_bytecode() {
    let (bindings, captures) = empty_bindings();
    let bytes = artifact(
        "main",
        "main_env",
        None,
        0,
        3,
        2,
        vec![
            number_constant(0, 0),
            number_constant(1, 1),
            RegisterInstruction {
                operation: RegisterOperation::Binary {
                    destination: 2,
                    left: 0,
                    right: 1,
                    operation: BinaryOperation::Add,
                },
                may_throw: true,
            },
            return_register(2),
        ],
        vec![json!(40), json!(2)],
        bindings,
        captures,
        Vec::new(),
    );

    let module = ExecutableModule::load_one(&bytes).expect("verified OGMB module");
    let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");
    let report = module
        .execute_without_host(&mut heap, "main", &[], ExecutionOptions::default())
        .expect("execution report");

    match report.completion {
        ExecutionCompletion::Return(value) => assert_eq!(value.as_number(), Some(42.0)),
        completion => panic!("expected normal return, got {completion:?}"),
    }
    assert_eq!(report.instructions_executed, 4);
    assert_eq!(module.function_count(), 1);
}

#[test]
fn executes_nested_function_calls_using_verified_function_table() {
    let parameter_binding = json!({
        "id": "arg",
        "name": "arg",
        "kind": "parameter",
        "mutable": true,
        "semantic_type": "number",
        "representation": "boxed"
    });
    let callee = artifact(
        "add_one",
        "add_one_env",
        None,
        1,
        7,
        6,
        vec![
            number_constant(1, 0),
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 2,
                    constant: 1,
                    kind: ConstantKind::BindingId,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Call {
                    destination: 4,
                    kind: CallKind::StoreBinding,
                    sources: vec![2, 0],
                },
                may_throw: true,
            },
            RegisterInstruction {
                operation: RegisterOperation::Unary {
                    destination: 5,
                    source: 0,
                    operation: ogma_bytecode::UnaryOperation::LoadBinding,
                },
                may_throw: true,
            },
            RegisterInstruction {
                operation: RegisterOperation::Binary {
                    destination: 6,
                    left: 5,
                    right: 1,
                    operation: BinaryOperation::Add,
                },
                may_throw: true,
            },
            return_register(6),
        ],
        vec![json!(1), json!("arg")],
        vec![parameter_binding],
        Vec::new(),
        vec![json!({"binding":"arg","kind":"register","index":0})],
    );
    let caller = artifact(
        "main",
        "main_env",
        None,
        0,
        4,
        3,
        vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::FunctionId,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Call {
                    destination: 0,
                    kind: CallKind::MakeClosure,
                    sources: vec![0],
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 1,
                    constant: 1,
                    kind: ConstantKind::Undefined,
                },
                may_throw: false,
            },
            number_constant(2, 2),
            RegisterInstruction {
                operation: RegisterOperation::Call {
                    destination: 3,
                    kind: CallKind::JavaScript,
                    sources: vec![0, 1, 2],
                },
                may_throw: true,
            },
            return_register(3),
        ],
        vec![json!("add_one"), JsonValue::Null, json!(41)],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );

    let module = ExecutableModule::load([caller, callee]).expect("verified function module");
    let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");
    let report = module
        .execute_without_host(&mut heap, "main", &[], ExecutionOptions::default())
        .expect("execution report");

    match report.completion {
        ExecutionCompletion::Return(value) => assert_eq!(value.as_number(), Some(42.0)),
        completion => panic!("expected nested call return, got {completion:?}"),
    }
    assert_eq!(report.max_call_depth_reached, 2);

    let depth_error = module.execute_without_host(
        &mut heap,
        "main",
        &[],
        ExecutionOptions {
            max_call_depth: 1,
            ..ExecutionOptions::default()
        },
    );
    assert!(matches!(
        depth_error,
        Err(InterpreterError::CallDepthExceeded { limit: 1 })
    ));
}

#[test]
fn closures_share_runtime_managed_mutable_binding_cells() {
    let parent_binding = json!({
        "id": "counter",
        "name": "counter",
        "kind": "let",
        "mutable": true,
        "semantic_type": "number",
        "representation": "boxed"
    });
    let binding_location = json!({
        "binding": "counter",
        "kind": "register",
        "index": 1
    });
    let child_capture = json!({
        "id": "counter_capture",
        "source_environment": "parent_env",
        "binding": "counter",
        "name": "counter",
        "mutable": true,
        "semantic_type": "number",
        "representation": "boxed",
        "mode": "cell"
    });

    let child = artifact(
        "read_counter",
        "child_env",
        Some("parent_env"),
        0,
        2,
        1,
        vec![
            RegisterInstruction {
                operation: RegisterOperation::Call {
                    destination: 1,
                    kind: CallKind::LoadCapture,
                    sources: vec![0],
                },
                may_throw: true,
            },
            return_register(1),
        ],
        Vec::new(),
        Vec::new(),
        vec![child_capture],
        Vec::new(),
    );

    let parent = artifact(
        "main",
        "parent_env",
        None,
        0,
        8,
        7,
        vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::Uninitialized,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Call {
                    destination: 1,
                    kind: CallKind::MakeCell,
                    sources: vec![0],
                },
                may_throw: true,
            },
            number_constant(2, 1),
            RegisterInstruction {
                operation: RegisterOperation::Call {
                    destination: 3,
                    kind: CallKind::StoreCapture,
                    sources: vec![1, 2],
                },
                may_throw: true,
            },
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 4,
                    constant: 2,
                    kind: ConstantKind::FunctionId,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Call {
                    destination: 5,
                    kind: CallKind::MakeClosure,
                    sources: vec![4, 1],
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 6,
                    constant: 3,
                    kind: ConstantKind::Undefined,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Call {
                    destination: 7,
                    kind: CallKind::JavaScript,
                    sources: vec![5, 6],
                },
                may_throw: true,
            },
            return_register(7),
        ],
        vec![
            JsonValue::Null,
            json!(42),
            json!("read_counter"),
            JsonValue::Null,
        ],
        vec![parent_binding],
        Vec::new(),
        vec![binding_location],
    );

    let module = ExecutableModule::load([parent, child]).expect("verified closure module");
    let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");
    let report = module
        .execute_without_host(&mut heap, "main", &[], ExecutionOptions::default())
        .expect("execution report");

    match report.completion {
        ExecutionCompletion::Return(value) => assert_eq!(value.as_number(), Some(42.0)),
        completion => panic!("expected captured value, got {completion:?}"),
    }
    assert_eq!(report.max_call_depth_reached, 2);
}

#[test]
fn property_operations_use_the_shared_runtime_object_model() {
    let parameter = json!({
        "id": "object_arg",
        "name": "object_arg",
        "kind": "parameter",
        "mutable": true,
        "semantic_type": "object",
        "representation": "boxed"
    });
    let bytes = artifact(
        "read_property",
        "read_property_env",
        None,
        1,
        6,
        5,
        vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 2,
                    constant: 0,
                    kind: ConstantKind::String,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 3,
                    constant: 1,
                    kind: ConstantKind::BindingId,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Call {
                    destination: 4,
                    kind: CallKind::StoreBinding,
                    sources: vec![3, 0],
                },
                may_throw: true,
            },
            RegisterInstruction {
                operation: RegisterOperation::Call {
                    destination: 5,
                    kind: CallKind::GetProperty,
                    sources: vec![0, 2],
                },
                may_throw: true,
            },
            return_register(5),
        ],
        vec![json!("answer"), json!("object_arg")],
        vec![parameter],
        Vec::new(),
        vec![json!({"binding":"object_arg","kind":"register","index":0})],
    );

    let module = ExecutableModule::load_one(&bytes).expect("verified property module");
    let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");
    let object = heap.allocate_object().expect("object");
    let key = heap.intern_property_key("answer").expect("property key");
    heap.set_property(object, key, Value::number(42.0))
        .expect("property write");

    let report = module
        .execute_without_host(
            &mut heap,
            "read_property",
            &[object],
            ExecutionOptions::default(),
        )
        .expect("execution report");
    match report.completion {
        ExecutionCompletion::Return(value) => assert_eq!(value.as_number(), Some(42.0)),
        completion => panic!("expected property value, got {completion:?}"),
    }
}

#[test]
fn closures_capture_immutable_values_by_value() {
    let child = artifact(
        "read_snapshot",
        "child_env",
        Some("parent_env"),
        0,
        2,
        1,
        vec![
            RegisterInstruction {
                operation: RegisterOperation::Unary {
                    destination: 1,
                    source: 0,
                    operation: ogma_bytecode::UnaryOperation::Copy,
                },
                may_throw: false,
            },
            return_register(1),
        ],
        Vec::new(),
        Vec::new(),
        vec![json!({
            "id": "snapshot_capture",
            "source_environment": "parent_env",
            "binding": "snapshot",
            "name": "snapshot",
            "mutable": false,
            "semantic_type": "number",
            "representation": "boxed",
            "mode": "value"
        })],
        Vec::new(),
    );
    let parent = artifact(
        "main",
        "parent_env",
        None,
        0,
        7,
        6,
        vec![
            number_constant(0, 0),
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 1,
                    constant: 1,
                    kind: ConstantKind::BindingId,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Call {
                    destination: 2,
                    kind: CallKind::StoreBinding,
                    sources: vec![1, 0],
                },
                may_throw: true,
            },
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 3,
                    constant: 2,
                    kind: ConstantKind::FunctionId,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Call {
                    destination: 4,
                    kind: CallKind::MakeClosure,
                    sources: vec![3, 0],
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 5,
                    constant: 3,
                    kind: ConstantKind::Undefined,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Call {
                    destination: 6,
                    kind: CallKind::JavaScript,
                    sources: vec![4, 5],
                },
                may_throw: true,
            },
            return_register(6),
        ],
        vec![
            json!(42),
            json!("snapshot"),
            json!("read_snapshot"),
            JsonValue::Null,
        ],
        vec![json!({
            "id": "snapshot",
            "name": "snapshot",
            "kind": "const",
            "mutable": false,
            "semantic_type": "number",
            "representation": "boxed"
        })],
        Vec::new(),
        vec![json!({"binding":"snapshot","kind":"register","index":0})],
    );

    let module = ExecutableModule::load([parent, child]).expect("verified value-capture module");
    let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");
    let report = module
        .execute_without_host(&mut heap, "main", &[], ExecutionOptions::default())
        .expect("execution report");

    match report.completion {
        ExecutionCompletion::Return(value) => assert_eq!(value.as_number(), Some(42.0)),
        completion => panic!("expected captured immutable value, got {completion:?}"),
    }
}

#[test]
fn throws_are_propagated_as_distinct_completions() {
    let (bindings, captures) = empty_bindings();
    let bytes = artifact(
        "throws",
        "throws_env",
        None,
        0,
        1,
        0,
        vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::String,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Throw { source: 0 },
                may_throw: true,
            },
        ],
        vec![json!("boom")],
        bindings,
        captures,
        Vec::new(),
    );
    let module = ExecutableModule::load_one(&bytes).expect("verified throw module");
    let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");
    let report = module
        .execute_without_host(&mut heap, "throws", &[], ExecutionOptions::default())
        .expect("execution report");
    match report.completion {
        ExecutionCompletion::Throw(value) => {
            assert_eq!(value.value_type(), ValueType::String);
            assert_eq!(
                heap.string_utf16(value).expect("thrown string"),
                "boom".encode_utf16().collect::<Vec<_>>()
            );
        }
        completion => panic!("expected thrown completion, got {completion:?}"),
    }
}

#[test]
fn fuel_exhaustion_stops_a_loop_at_the_back_edge() {
    let (bindings, captures) = empty_bindings();
    let bytes = artifact(
        "loop",
        "loop_env",
        None,
        0,
        1,
        0,
        vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::Boolean,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Jump { target: 9 },
                may_throw: false,
            },
        ],
        vec![json!(true)],
        bindings,
        captures,
        Vec::new(),
    );
    let module = ExecutableModule::load_one(&bytes).expect("verified loop module");
    let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");
    let report = module
        .execute_without_host(
            &mut heap,
            "loop",
            &[],
            ExecutionOptions {
                instruction_budget: 5,
                ..ExecutionOptions::default()
            },
        )
        .expect("interrupted report");

    assert!(matches!(
        report.completion,
        ExecutionCompletion::Interrupted(ExecutionInterruption::FuelExhausted)
    ));
    assert_eq!(report.instructions_executed, 5);
    assert_eq!(report.fuel_remaining, 0);
    let roots = heap.root_snapshot();
    assert_eq!(roots.interpreter_frames, 0);
    assert_eq!(roots.temporary_scopes, 0);
}

#[test]
fn cancellation_is_observed_at_function_entry_without_dispatch() {
    let (bindings, captures) = empty_bindings();
    let bytes = artifact(
        "cancelled",
        "cancelled_env",
        None,
        0,
        1,
        0,
        vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::Number,
                },
                may_throw: false,
            },
            return_register(0),
        ],
        vec![json!(1)],
        bindings,
        captures,
        Vec::new(),
    );
    let module = ExecutableModule::load_one(&bytes).expect("verified cancellation module");
    let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");
    let token = CancellationToken::new();
    token.cancel();
    let report = module
        .execute_without_host(
            &mut heap,
            "cancelled",
            &[],
            ExecutionOptions {
                cancellation: Some(token),
                ..ExecutionOptions::default()
            },
        )
        .expect("interrupted report");

    assert!(matches!(
        report.completion,
        ExecutionCompletion::Interrupted(ExecutionInterruption::Cancelled)
    ));
    assert_eq!(report.instructions_executed, 0);
    let roots = heap.root_snapshot();
    assert_eq!(roots.interpreter_frames, 0);
    assert_eq!(roots.temporary_scopes, 0);
}

#[test]
fn deadline_exhaustion_is_distinct_from_fuel_and_cancellation() {
    let (bindings, captures) = empty_bindings();
    let bytes = artifact(
        "deadline",
        "deadline_env",
        None,
        0,
        1,
        0,
        vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::Number,
                },
                may_throw: false,
            },
            return_register(0),
        ],
        vec![json!(1)],
        bindings,
        captures,
        Vec::new(),
    );
    let module = ExecutableModule::load_one(&bytes).expect("verified deadline module");
    let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");
    let report = module
        .execute_without_host(
            &mut heap,
            "deadline",
            &[],
            ExecutionOptions {
                time_budget: Some(std::time::Duration::ZERO),
                ..ExecutionOptions::default()
            },
        )
        .expect("interrupted report");
    assert!(matches!(
        report.completion,
        ExecutionCompletion::Interrupted(ExecutionInterruption::DeadlineExceeded)
    ));
    assert_eq!(report.instructions_executed, 0);
    let roots = heap.root_snapshot();
    assert_eq!(roots.interpreter_frames, 0);
    assert_eq!(roots.temporary_scopes, 0);
}

#[test]
fn malformed_serialized_artifact_is_rejected_before_dispatch() {
    let (bindings, captures) = empty_bindings();
    let mut bytes = artifact(
        "invalid",
        "invalid_env",
        None,
        0,
        1,
        0,
        vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::Number,
                },
                may_throw: false,
            },
            return_register(0),
        ],
        vec![json!(1)],
        bindings,
        captures,
        Vec::new(),
    );
    bytes[0] ^= 0x01;
    let error = match ExecutableModule::load_one(&bytes) {
        Err(error) => error,
        Ok(_) => panic!("corrupt container must fail"),
    };
    assert!(matches!(error, InterpreterError::Bytecode(_)));
}

struct RecordingHost {
    calls: Vec<String>,
}

impl Host for RecordingHost {
    fn call(
        &mut self,
        _heap: &mut Heap,
        capability: &str,
        arguments: &[Value],
    ) -> Result<Value, HostError> {
        self.calls.push(format!("{capability}:{}", arguments.len()));
        Ok(Value::number(7.0))
    }
}

struct CancellingHost {
    token: CancellationToken,
}

impl Host for CancellingHost {
    fn call(
        &mut self,
        _heap: &mut Heap,
        _capability: &str,
        _arguments: &[Value],
    ) -> Result<Value, HostError> {
        self.token.cancel();
        Ok(Value::number(7.0))
    }
}

#[test]
fn cancellation_inside_nested_host_call_unwinds_every_vm_frame() {
    let (bindings, captures) = empty_bindings();
    let child = artifact(
        "nested_host",
        "nested_host_env",
        None,
        0,
        3,
        2,
        vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::HostCapability,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Call {
                    destination: 2,
                    kind: CallKind::Host,
                    sources: vec![0],
                },
                may_throw: true,
            },
            return_register(2),
        ],
        vec![json!("cancel.request")],
        bindings.clone(),
        captures.clone(),
        Vec::new(),
    );
    let parent = artifact(
        "main",
        "main_env",
        None,
        0,
        4,
        2,
        vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::FunctionId,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Call {
                    destination: 0,
                    kind: CallKind::MakeClosure,
                    sources: vec![0],
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 1,
                    constant: 1,
                    kind: ConstantKind::Undefined,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Call {
                    destination: 2,
                    kind: CallKind::JavaScript,
                    sources: vec![0, 1],
                },
                may_throw: true,
            },
            return_register(2),
        ],
        vec![json!("nested_host"), JsonValue::Null],
        bindings,
        captures,
        Vec::new(),
    );

    let module = ExecutableModule::load([parent, child]).expect("verified nested host module");
    let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");
    let token = CancellationToken::new();
    let mut host = CancellingHost {
        token: token.clone(),
    };
    let report = module
        .execute(
            &mut heap,
            "main",
            &[],
            &mut host,
            ExecutionOptions {
                cancellation: Some(token.clone()),
                interrupt_poll_interval: 64,
                ..ExecutionOptions::default()
            },
        )
        .expect("interrupted execution report");

    assert!(token.is_cancelled());
    assert!(report.instructions_executed > 0);
    assert_eq!(report.max_call_depth_reached, 2);
    assert!(matches!(
        report.completion,
        ExecutionCompletion::Interrupted(ExecutionInterruption::Cancelled)
    ));
    let roots = heap.root_snapshot();
    assert_eq!(roots.interpreter_frames, 0);
    assert_eq!(roots.temporary_scopes, 0);
}

#[test]
fn call_depth_limit_rejects_nested_frames_and_unwinds_runtime_roots() {
    let (bindings, captures) = empty_bindings();
    let child = artifact(
        "child",
        "child_env",
        Some("main_env"),
        0,
        1,
        0,
        vec![number_constant(0, 0), return_register(0)],
        vec![json!(9)],
        bindings.clone(),
        captures.clone(),
        Vec::new(),
    );
    let parent = artifact(
        "main",
        "main_env",
        None,
        0,
        3,
        2,
        vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::FunctionId,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Call {
                    destination: 0,
                    kind: CallKind::MakeClosure,
                    sources: vec![0],
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 1,
                    constant: 1,
                    kind: ConstantKind::Undefined,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Call {
                    destination: 2,
                    kind: CallKind::JavaScript,
                    sources: vec![0, 1],
                },
                may_throw: true,
            },
            return_register(2),
        ],
        vec![json!("child"), JsonValue::Null],
        bindings,
        captures,
        Vec::new(),
    );
    let module = ExecutableModule::load([parent, child]).expect("verified nested-call module");
    let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");
    let error = module
        .execute_without_host(
            &mut heap,
            "main",
            &[],
            ExecutionOptions {
                max_call_depth: 1,
                ..ExecutionOptions::default()
            },
        )
        .expect_err("second VM frame must exceed the configured depth limit");

    assert!(matches!(
        error,
        InterpreterError::CallDepthExceeded { limit: 1 }
    ));
    let roots = heap.root_snapshot();
    assert_eq!(roots.interpreter_frames, 0);
    assert_eq!(roots.temporary_scopes, 0);
}

#[test]
fn entry_arguments_survive_vm_setup_and_return_values_can_be_promoted() {
    let (_, captures) = empty_bindings();
    let bindings = vec![json!({
        "id": "identity_argument",
        "name": "argument",
        "kind": "parameter",
        "mutable": true,
        "semantic_type": "object",
        "representation": "object_ref"
    })];
    let locations = vec![json!({
        "binding": "identity_argument",
        "kind": "register",
        "index": 0
    })];
    let bytes = artifact(
        "identity",
        "identity_env",
        None,
        1,
        1,
        0,
        vec![return_register(0)],
        Vec::new(),
        bindings,
        captures,
        locations,
    );
    let module = ExecutableModule::load_one(&bytes).expect("verified identity module");
    let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");
    let argument = heap.allocate_object().expect("managed argument object");
    let report = module
        .execute_without_host(
            &mut heap,
            "identity",
            &[argument],
            ExecutionOptions::default(),
        )
        .expect("identity call completes");

    let returned = match report.completion {
        ExecutionCompletion::Return(value) => value,
        completion => panic!("expected identity return, got {completion:?}"),
    };
    assert!(
        heap.strict_equal(argument, returned)
            .expect("same heap object")
    );
    let persistent = heap
        .retain_host_value(returned)
        .expect("promote transient completion to a host root");
    assert_eq!(heap.root_snapshot().interpreter_frames, 0);
    assert_eq!(heap.root_snapshot().temporary_scopes, 0);
    assert_eq!(heap.root_snapshot().persistent_host_roots, 1);

    heap.collect().expect("collection after result promotion");
    let resolved = heap
        .resolve_host_handle(persistent)
        .expect("persistent completion remains alive");
    assert!(
        heap.strict_equal(argument, resolved)
            .expect("same retained object")
    );
}

#[test]
fn compiler_output_materializes_const_ssa_values_before_by_value_capture() {
    let allocator = Allocator::default();
    let source = SourceFile::new(
        "ogm46-const-capture",
        "memory:///ogm46-const-capture.js",
        None,
        "function run(seed) { const immutable = seed + 1; const read = function() { return immutable; }; return read(); }",
    )
    .expect("source is valid");
    let parsed = parse(&allocator, &source, ParseRequestOptions::default())
        .expect("source parses through the pinned Oxc frontend");
    let compiled =
        compile_to_bytecode(&parsed).expect("const value capture lowers through Core IR and OGMB");
    let entry = compiled
        .functions
        .iter()
        .find(|function| function.name == "run")
        .expect("run function artifact");
    let entry_id = entry.id.clone();
    let artifacts = compiled
        .functions
        .into_iter()
        .map(|function| function.artifact.bytes)
        .collect::<Vec<_>>();
    let module = ExecutableModule::load(artifacts)
        .expect("interpreter verifies compiler-produced const-capture artifacts");
    let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");
    let report = module
        .execute_without_host(
            &mut heap,
            &entry_id,
            &[Value::number(41.0)],
            ExecutionOptions::default(),
        )
        .expect("const-capturing closure executes");

    match report.completion {
        ExecutionCompletion::Return(value) => assert_eq!(value.as_number(), Some(42.0)),
        completion => panic!("expected by-value closure return, got {completion:?}"),
    }
    let roots = heap.root_snapshot();
    assert_eq!(roots.interpreter_frames, 0);
    assert_eq!(roots.temporary_scopes, 0);
}

#[test]
fn host_calls_are_capability_named_and_traced() {
    let (bindings, captures) = empty_bindings();
    let bytes = artifact(
        "host_call",
        "host_call_env",
        None,
        0,
        3,
        2,
        vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::HostCapability,
                },
                may_throw: false,
            },
            number_constant(1, 1),
            RegisterInstruction {
                operation: RegisterOperation::Call {
                    destination: 2,
                    kind: CallKind::Host,
                    sources: vec![0, 1],
                },
                may_throw: true,
            },
            return_register(2),
        ],
        vec![json!("telemetry.record"), json!(5)],
        bindings,
        captures,
        Vec::new(),
    );
    let module = ExecutableModule::load_one(&bytes).expect("verified host-call module");
    let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");
    let mut host = RecordingHost { calls: Vec::new() };
    let report = module
        .execute(
            &mut heap,
            "host_call",
            &[],
            &mut host,
            ExecutionOptions::default(),
        )
        .expect("execution report");

    assert_eq!(host.calls, vec!["telemetry.record:1"]);
    match report.completion {
        ExecutionCompletion::Return(value) => assert_eq!(value.as_number(), Some(7.0)),
        completion => panic!("expected host result, got {completion:?}"),
    }
}

#[test]
fn executes_real_compiler_output_with_lexical_closure_capture() {
    let allocator = Allocator::default();
    let source = SourceFile::new(
        "ogm46-compiler-integration",
        "memory:///ogm46-compiler-integration.js",
        None,
        "function run(seed) { let count = seed; if (seed > 0) { count = count + 1; } function create() { return function() { count = count + 1; return count; }; } const increment = create(); return increment() + increment(); }",
    )
    .expect("source is valid");
    let parsed = parse(&allocator, &source, ParseRequestOptions::default())
        .expect("source parses through the pinned Oxc frontend");
    let compiled = compile_to_bytecode(&parsed)
        .expect("frontend, semantic analysis, Core IR and OGMB compilation succeed");
    assert!(compiled.functions.len() >= 3);

    let entry = compiled
        .functions
        .iter()
        .find(|function| function.name == "run")
        .expect("run function artifact");
    let entry_id = entry.id.clone();
    let artifacts = compiled
        .functions
        .iter()
        .map(|function| function.artifact.bytes.clone())
        .collect::<Vec<_>>();
    let module = ExecutableModule::load(artifacts)
        .expect("interpreter re-verifies compiler-produced serialized OGMB artifacts");
    let entry_location = module
        .source_location(&entry_id, 0)
        .expect("verified byte offset resolves through the compiler source map");
    assert_eq!(entry_location.function_id, entry_id);
    assert_eq!(
        entry_location.code_generation_id.as_deref(),
        Some("ogma_m1_register_codegen")
    );
    assert_eq!(
        entry_location.code_generation_version.as_deref(),
        Some("1.0.0")
    );
    assert!(entry_location.source.is_some());
    assert!(!entry_location.lexical_scopes.is_empty());
    assert!(!entry_location.variable_locations.is_empty());
    assert!(
        module.source_location(&entry_id, u32::MAX).is_none(),
        "offsets outside the verified instruction stream are not source locations"
    );

    let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");
    let report = module
        .execute_without_host(
            &mut heap,
            &entry_id,
            &[Value::number(40.0)],
            ExecutionOptions::default(),
        )
        .expect("compiler-produced artifact executes");

    let last_location = report
        .last_location
        .as_ref()
        .expect("execution report retains the last dispatched source location");
    assert!(last_location.source.is_some());
    assert!(!last_location.lexical_scopes.is_empty());

    match report.completion {
        ExecutionCompletion::Return(value) => assert_eq!(value.as_number(), Some(85.0)),
        completion => panic!("expected closure-backed compiler result, got {completion:?}"),
    }
    assert!(report.instructions_executed > 0);
    assert_eq!(report.max_call_depth_reached, 2);
}

#[cfg(debug_assertions)]
fn compile_debugger_module(source_id: &str, source_text: &str) -> (ExecutableModule, String) {
    let allocator = Allocator::default();
    let source = SourceFile::new(
        source_id,
        format!("memory:///{source_id}.js"),
        None,
        source_text,
    )
    .expect("debugger test source is valid");
    let parsed = parse(&allocator, &source, ParseRequestOptions::default())
        .expect("debugger test source parses");
    let compiled = compile_to_bytecode(&parsed)
        .expect("debugger test source compiles into verified OGMB artifacts");
    let entry_id = compiled
        .functions
        .iter()
        .find(|function| function.name == "run")
        .expect("run entry artifact exists")
        .id
        .clone();
    let artifacts = compiled
        .functions
        .into_iter()
        .map(|function| function.artifact.bytes)
        .collect::<Vec<_>>();
    (
        ExecutableModule::load(artifacts)
            .expect("interpreter verifies compiler-produced artifacts"),
        entry_id,
    )
}

#[cfg(debug_assertions)]
fn source_breakpoint(module: &ExecutableModule, function_id: &str, line: u64) -> (String, u32) {
    source_breakpoint_in_functions(module, std::iter::once(function_id), line)
}

#[cfg(debug_assertions)]
fn source_breakpoint_in_functions<'a>(
    module: &ExecutableModule,
    function_ids: impl IntoIterator<Item = &'a str>,
    line: u64,
) -> (String, u32) {
    function_ids
        .into_iter()
        .find_map(|function_id| {
            (0..4096_u32)
                .filter_map(|offset| module.source_location(function_id, offset))
                .find_map(|location| {
                    let source = location.source?;
                    let start = source.get("start")?;
                    let source_line = start.get("line")?.as_u64()?;
                    if source_line != line {
                        return None;
                    }
                    let source_id = source.get("file")?.as_str()?.to_owned();
                    Some((source_id, u32::try_from(source_line).ok()?))
                })
        })
        .expect("requested source line has a verified executable location")
}

#[cfg(debug_assertions)]
fn source_breakpoint_any(module: &ExecutableModule, line: u64) -> (String, u32) {
    let functions = module.functions();
    source_breakpoint_in_functions(
        module,
        functions.iter().map(|function| function.id.as_str()),
        line,
    )
}

#[cfg(debug_assertions)]
fn stopped_event(handle: &ogma_interpreter::DebuggerHandle) -> ogma_interpreter::StoppedEvent {
    match handle
        .recv_event_timeout(std::time::Duration::from_secs(5))
        .expect("debugger event channel remains connected")
        .expect("interpreter reaches the requested source stop")
    {
        DebuggerEvent::Stopped(event) => event,
        DebuggerEvent::Exception(event) => {
            panic!("execution stopped for an unexpected exception: {event:?}")
        }
    }
}

#[cfg(debug_assertions)]
#[test]
fn production_debugger_supports_source_breakpoints_and_source_level_stepping() {
    let source_text = "function run(seed) {\n  function inner(value) {\n    const incremented = value + 1;\n    return incremented;\n  }\n  const initial = seed + 1;\n  const answer = inner(initial);\n  return answer;\n}\n";
    let (module, run_id) = compile_debugger_module("ogm49-step-debugger", source_text);
    let (source_id, line) = source_breakpoint(&module, &run_id, 7);
    let (mut session, handle) = module
        .create_debugger_session()
        .expect("verified bytecode source metadata creates a debugger session");
    let initial_breakpoint = session
        .set_breakpoint(source_id.clone(), line, None, None, None)
        .expect("source breakpoint is accepted");
    assert!(initial_breakpoint.verified, "{initial_breakpoint:?}");
    assert!(
        session
            .remove_breakpoint(&initial_breakpoint.id)
            .expect("breakpoint can be removed")
    );
    assert!(
        session
            .list_breakpoints()
            .expect("breakpoints can be listed")
            .is_empty()
    );
    let breakpoint = session
        .set_breakpoint(source_id.clone(), line, None, None, None)
        .expect("source breakpoint can be re-registered");
    assert!(breakpoint.verified, "{breakpoint:?}");

    let control_source_id = source_id.clone();
    let expected_run_id = run_id.clone();
    let controller = std::thread::spawn(move || {
        let first = stopped_event(&handle);
        assert_eq!(first.reason, "breakpoint");
        let dynamic_breakpoint = handle
            .set_breakpoint(control_source_id, 8, None, None, None)
            .expect("breakpoints can be added from the control thread while paused");
        assert!(dynamic_breakpoint.verified, "{dynamic_breakpoint:?}");
        assert!(
            handle
                .remove_breakpoint(dynamic_breakpoint.id)
                .expect("breakpoints can be removed from the control thread while paused")
        );
        let stack = handle.get_stack().expect("paused VM stack is readable");
        assert_eq!(stack.len(), 1);
        assert_eq!(stack[0].function_id, expected_run_id);
        assert!(!stack[0].scope_ids.is_empty());

        handle.step_into().expect("step-into command is accepted");
        let into = stopped_event(&handle);
        assert_eq!(into.reason, "step");
        let stack = handle.get_stack().expect("step-into exposes nested frame");
        assert_eq!(stack.len(), 2);
        assert_eq!(stack[0].function_name, "inner");

        handle.step_over().expect("step-over command is accepted");
        let over = stopped_event(&handle);
        assert_eq!(over.reason, "step");
        let stack = handle
            .get_stack()
            .expect("step-over preserves nested call frame");
        assert_eq!(stack[0].function_name, "inner");
        let scopes = handle
            .get_scopes(stack[0].id.clone())
            .expect("active lexical scopes are readable");
        let scope_id = scopes
            .first()
            .expect("inner function has a lexical scope")
            .id
            .clone();
        let variables = handle
            .get_variables(stack[0].id.clone(), scope_id)
            .expect("variables can be requested by lexical scope id");
        assert!(
            variables.iter().any(|variable| {
                variable.availability == "available" && variable.value.preview == "42"
            }),
            "the initialized local is exposed with its real value: {variables:?}"
        );
        assert_eq!(
            handle
                .evaluate_identifier(stack[0].id.clone(), "incremented")
                .expect("identifier-only inspection resolves local bindings")
                .preview,
            "42"
        );

        handle.step_out().expect("step-out command is accepted");
        let out = stopped_event(&handle);
        assert_eq!(out.reason, "step");
        let stack = handle.get_stack().expect("caller frame is restored");
        assert_eq!(stack.len(), 1);
        assert_eq!(stack[0].function_id, expected_run_id);
        handle
            .resume()
            .expect("execution resumes after the final step");
    });

    let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");
    let report = module
        .execute_debugged(
            &mut heap,
            &run_id,
            &[Value::number(40.0)],
            &mut NoHost,
            ExecutionOptions::default(),
            &mut session,
        )
        .expect("debugged interpreter execution completes");
    controller
        .join()
        .expect("debugger controller completes all operations");

    match report.completion {
        ExecutionCompletion::Return(value) => assert_eq!(value.as_number(), Some(42.0)),
        completion => panic!("expected return after stepping, got {completion:?}"),
    }
}

#[cfg(debug_assertions)]
#[test]
fn production_debugger_reads_closure_capture_bindings() {
    let source_text = "function run(seed) {\n  let count = seed;\n  function increment() {\n    count = count + 1;\n    return count;\n  }\n  return increment();\n}\n";
    let (module, run_id) = compile_debugger_module("ogm49-capture-debugger", source_text);
    let (source_id, line) = source_breakpoint_any(&module, 5);
    let (mut session, handle) = module
        .create_debugger_session()
        .expect("verified bytecode source metadata creates a debugger session");
    let breakpoint = session
        .set_breakpoint(source_id, line, None, None, None)
        .expect("closure source breakpoint is accepted");
    assert!(breakpoint.verified, "{breakpoint:?}");

    let controller = std::thread::spawn(move || {
        let stopped = stopped_event(&handle);
        assert_eq!(stopped.reason, "breakpoint");
        let stack = handle.get_stack().expect("closure stack can be inspected");
        assert_eq!(stack.len(), 2);
        assert_eq!(stack[0].function_name, "increment");
        let scopes = handle
            .get_scopes(stack[0].id.clone())
            .expect("closure lexical scope is readable");
        let root_scope_id = scopes
            .iter()
            .find(|scope| scope.parent_id.is_none())
            .expect("closure frame exposes its function root scope")
            .id
            .clone();
        let variables = handle
            .get_variables(stack[0].id.clone(), root_scope_id)
            .expect("captured variables are available through the variables API");
        let captured = variables
            .iter()
            .find(|variable| variable.name == "count")
            .expect("closure capture is included in the active root scope");
        assert_eq!(captured.availability, "available");
        assert_eq!(captured.value.preview, "41");
        assert_eq!(
            handle
                .evaluate_identifier(stack[0].id.clone(), captured.binding_id.clone())
                .expect("capture ID resolves without evaluating user code")
                .preview,
            "41"
        );
        handle.resume().expect("closure execution resumes");
    });

    let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");
    let report = module
        .execute_debugged(
            &mut heap,
            &run_id,
            &[Value::number(40.0)],
            &mut NoHost,
            ExecutionOptions::default(),
            &mut session,
        )
        .expect("debugged closure execution completes");
    controller
        .join()
        .expect("closure debugger controller completes");
    match report.completion {
        ExecutionCompletion::Return(value) => assert_eq!(value.as_number(), Some(41.0)),
        completion => panic!("expected closure result, got {completion:?}"),
    }
}

#[cfg(debug_assertions)]
#[test]
fn production_debugger_inspects_own_data_descriptors_without_user_code() {
    let source_text = "function run(subject) {\n  return subject;\n}\n";
    let (module, run_id) = compile_debugger_module("ogm49-inspect-debugger", source_text);
    let (source_id, line) = source_breakpoint(&module, &run_id, 2);
    let (mut session, handle) = module
        .create_debugger_session()
        .expect("verified source metadata creates a debugger session");
    let breakpoint = session
        .set_breakpoint(source_id, line, None, None, None)
        .expect("data inspector source breakpoint is accepted");
    assert!(breakpoint.verified, "{breakpoint:?}");

    let controller = std::thread::spawn(move || {
        let stopped = stopped_event(&handle);
        assert_eq!(stopped.reason, "breakpoint");
        assert_eq!(
            handle.backend_capabilities().get("safeInspection"),
            Some(&true),
            "the heap-backed inspector is advertised only while the Runtime session is active"
        );
        let stack = handle
            .get_stack()
            .expect("paused object frame is available");
        let scopes = handle
            .get_scopes(stack[0].id.clone())
            .expect("object parameter scope is readable");
        let value = scopes
            .iter()
            .flat_map(|scope| &scope.variables)
            .find(|variable| variable.value.object_id.is_some())
            .expect("managed object gets an opaque inspection reference");
        let inspected = handle
            .inspect_object(value.value.object_id.as_deref().expect("object id"), 128)
            .expect("own descriptors can be inspected safely");
        assert!(!inspected.is_proxy);
        let property = inspected
            .properties
            .iter()
            .find(|property| property.name == "answer")
            .expect("own data property is returned");
        assert_eq!(property.value.preview, "42");
        assert!(!property.is_accessor);
        let object_id = value
            .value
            .object_id
            .clone()
            .expect("object ID remains available");
        handle
            .resume()
            .expect("resume releases temporary inspection roots");
        assert!(
            handle.inspect_object(object_id, 128).is_err(),
            "an object reference expires as soon as the target resumes"
        );
    });

    let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");
    let object = heap.allocate_object().expect("managed object");
    let object_scope = heap.open_scope().expect("temporary object root scope");
    heap.scope_root(object_scope, object)
        .expect("object remains rooted while it is initialized");
    let key = heap
        .intern_property_key("answer")
        .expect("property key atom");
    assert!(
        heap.define_own_property(
            object,
            key,
            PropertyDescriptor::data(Value::number(42.0), true, true, true),
        )
        .expect("own property descriptor is compatible")
    );
    heap.close_scope(object_scope)
        .expect("temporary object root scope closes before execution");
    let report = module
        .execute_debugged(
            &mut heap,
            &run_id,
            &[object],
            &mut NoHost,
            ExecutionOptions::default(),
            &mut session,
        )
        .expect("debugged object inspection run completes");
    controller
        .join()
        .expect("object inspector controller completes");
    match report.completion {
        ExecutionCompletion::Return(value) => assert_eq!(value.value_type(), ValueType::Object),
        completion => panic!("expected returned object, got {completion:?}"),
    }
}

#[cfg(debug_assertions)]
#[test]
fn production_debugger_pauses_at_safe_points_and_reports_uncaught_exceptions() {
    let (module, run_id) = compile_debugger_module(
        "ogm49-pause-exception",
        "function run() {\n  throw 17;\n}\n",
    );
    let (mut session, handle) = module
        .create_debugger_session()
        .expect("debugger session is created");
    session
        .request_pause()
        .expect("owner-thread pause request is queued before execution");

    let controller = std::thread::spawn(move || {
        let paused = stopped_event(&handle);
        assert_eq!(paused.reason, "pause");
        let stack = handle
            .get_stack()
            .expect("paused exception frame is readable");
        assert_eq!(stack.len(), 1);
        handle
            .resume()
            .expect("pause resumes before the throw instruction");

        match handle
            .recv_event_timeout(std::time::Duration::from_secs(5))
            .expect("exception event channel remains connected")
            .expect("uncaught throw is reported")
        {
            DebuggerEvent::Exception(exception) => {
                assert_eq!(exception.value.preview, "17");
                assert!(!exception.caught);
                assert_eq!(exception.frame_id, stack[0].id);
            }
            DebuggerEvent::Stopped(event) => {
                panic!("expected exception event, got stop event: {event:?}")
            }
        }
        handle
            .resume()
            .expect("execution continues after exception inspection");
    });

    let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");
    let report = module
        .execute_debugged(
            &mut heap,
            &run_id,
            &[],
            &mut NoHost,
            ExecutionOptions::default(),
            &mut session,
        )
        .expect("debugger does not change throw completion semantics");
    controller
        .join()
        .expect("pause and exception controller completes");
    match report.completion {
        ExecutionCompletion::Throw(value) => assert_eq!(value.as_number(), Some(17.0)),
        completion => panic!("expected the original throw completion, got {completion:?}"),
    }
}

#[cfg(debug_assertions)]
#[test]
fn production_debugger_implements_backend_neutral_port_contract() {
    let (module, _) =
        compile_debugger_module("ogm49-port-contract", "function run() {\n  return 1;\n}\n");
    let (_session, handle) = module
        .create_debugger_session()
        .expect("production debugger session is initialized");
    let port: &dyn DebuggerPort = &handle;
    assert_eq!(port.backend_name(), "interpreter");
    assert!(port.session_id().starts_with("debug-"));
    let capabilities = port.backend_capabilities();
    for capability in [
        "pause",
        "breakpoints",
        "stepInto",
        "stepOver",
        "stepOut",
        "callStack",
        "scopes",
        "exceptionEvents",
    ] {
        assert_eq!(capabilities.get(capability), Some(&true), "{capability}");
    }
    assert_eq!(capabilities.get("safeInspection"), Some(&false));
}
