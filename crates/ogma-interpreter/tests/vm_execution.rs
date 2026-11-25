//! End-to-end tests for verified OGMB loading and M1 interpreter execution.

use ogma_bytecode::{
    BinaryOperation, CallKind, ConstantKind, RegisterInstruction, RegisterOperation, build_ogmb_v1,
    encode_register_code,
};
use ogma_compiler::compile_to_bytecode;
use ogma_frontend::{ParseRequestOptions, SourceFile, parse};
use ogma_interpreter::{
    CancellationToken, ExecutableModule, ExecutionCompletion, ExecutionInterruption,
    ExecutionOptions, Host, HostError, InterpreterError,
};
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
                "max_register_index": max_register_index,
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
                interrupt_poll_interval: 1,
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
    let mut heap = Heap::new(HeapConfig::default()).expect("Runtime heap");
    let report = module
        .execute_without_host(
            &mut heap,
            &entry_id,
            &[Value::number(40.0)],
            ExecutionOptions::default(),
        )
        .expect("compiler-produced artifact executes");

    match report.completion {
        ExecutionCompletion::Return(value) => assert_eq!(value.as_number(), Some(85.0)),
        completion => panic!("expected closure-backed compiler result, got {completion:?}"),
    }
    assert!(report.instructions_executed > 0);
    assert_eq!(report.max_call_depth_reached, 2);
}
