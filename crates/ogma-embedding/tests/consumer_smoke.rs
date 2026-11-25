//! External Rust consumer smoke tests for the production embedding API.
//!
//! These tests compile JavaScript to verified OGMB artifacts, execute them
//! through the public Engine/Runtime/Context API, and exercise host callbacks.

use ogma_compiler::compile_to_bytecode;
use ogma_embedding::{
    Engine, EngineConfig, ExecutionCompletion, ExecutionRequest, HostCallCompletion,
    HostFunctionDescriptor, PersistentCompletion, Value,
};
use ogma_frontend::{ParseRequestOptions, SourceFile, parse};
use oxc_allocator::Allocator;

fn compile_module(source_text: &str) -> Vec<Vec<u8>> {
    let allocator = Allocator::default();
    let source = SourceFile::new(
        "rust-consumer-smoke",
        "memory:///rust-consumer-smoke.js",
        None,
        source_text,
    )
    .expect("valid source");
    let parsed = parse(&allocator, &source, ParseRequestOptions::default()).expect("source parses");
    compile_to_bytecode(&parsed)
        .expect("source compiles to verified M1 OGMB")
        .functions
        .into_iter()
        .map(|function| function.artifact.bytes)
        .collect()
}

#[test]
fn external_rust_consumer_creates_runtime_executes_and_releases_persistent_results() {
    let engine = Engine::new(EngineConfig::default()).expect("validated engine");
    let mut runtime = engine.create_runtime().expect("owner-thread Runtime");
    let runtime_id = runtime.id();
    let mut context = runtime.create_context().expect("scoped Context");
    context
        .load_module(compile_module("function add(a, b) { return a + b; }"))
        .expect("verified module");
    let add_id = context
        .functions()
        .expect("verified function metadata")
        .into_iter()
        .find(|function| function.name == "add")
        .expect("compiled add function")
        .id;

    let report = context
        .execute_persistent(
            &add_id,
            &[Value::number(19.0), Value::number(23.0)],
            ExecutionRequest::default(),
        )
        .expect("real interpreter execution");
    let PersistentCompletion::Return(result) = report.completion else {
        panic!("expected normal Return completion");
    };
    assert_eq!(
        context
            .resolve(result)
            .expect("persistent Runtime value")
            .as_number(),
        Some(42.0)
    );
    context.collect().expect("explicit GC");
    assert_eq!(
        context
            .resolve(result)
            .expect("value survives explicit GC")
            .as_number(),
        Some(42.0)
    );
    context.release(result).expect("release Runtime root");
    assert!(context.resolve(result).is_err());
    drop(context);

    let next_context = runtime.create_context().expect("Context can be recreated");
    drop(next_context);
    assert_eq!(runtime.id(), runtime_id);
}

#[test]
fn external_rust_consumer_registers_a_host_callback_and_observes_throw() {
    let engine = Engine::new(EngineConfig::default()).expect("validated engine");
    let mut runtime = engine.create_runtime().expect("Runtime");
    let mut context = runtime.create_context().expect("Context");
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/core-ir/fixtures/positive-host-call.json"
    ))
    .expect("Core IR fixture JSON");
    let document: ogma_core_ir::FunctionDocument =
        serde_json::from_value(fixture).expect("Core IR schema");
    let module = ogma_compiler::bytecode::compile_functions(&[document])
        .expect("Core IR compiles to OGMB")
        .into_iter()
        .map(|function| function.artifact.bytes)
        .collect::<Vec<_>>();
    context
        .load_module(module)
        .expect("verified host-call OGMB");
    context
        .register_host_function(
            HostFunctionDescriptor::new(1, "benchmark.host.echo"),
            |_call, _arguments| Ok(HostCallCompletion::Throw(Value::number(73.0))),
        )
        .expect("register Rust host callback");
    let report = context
        .execute(
            "fn_host_call",
            &[Value::number(1.0)],
            ExecutionRequest::default(),
        )
        .expect("host call completes as JS Throw");
    assert!(matches!(
        report.completion,
        ExecutionCompletion::Throw(value) if value.as_number() == Some(73.0)
    ));
}
