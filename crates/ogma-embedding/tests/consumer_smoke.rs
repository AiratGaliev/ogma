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
use ogma_interpreter::{ExecutableModule, HostError, NoHost};
use oxc_allocator::Allocator;
use std::{cell::Cell, rc::Rc};

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

#[test]
fn external_rust_consumer_calls_registered_host_function_from_javascript_and_jobs() {
    let engine = Engine::new(EngineConfig::default()).expect("validated engine");
    let mut runtime = engine.create_runtime().expect("Runtime");
    let mut context = runtime.create_context().expect("Context");
    context
        .load_module(compile_module(
            "function invoke(callback, value) { return callback(value); } function schedule(callback) { return Promise.resolve(21).then(callback); }",
        ))
        .expect("source compiles to verified OGMB");
    let functions = context.functions().expect("function metadata");
    let invoke_id = functions
        .iter()
        .find(|function| function.name == "invoke")
        .expect("invoke function")
        .id
        .clone();
    let schedule_id = functions
        .iter()
        .find(|function| function.name == "schedule")
        .expect("schedule function")
        .id
        .clone();

    let nested_module = Rc::new(
        ExecutableModule::load(compile_module(
            "function double(value) { return value * 2; }",
        ))
        .expect("nested callback target is a verified OGMB module"),
    );
    let nested_function_id = nested_module
        .functions()
        .into_iter()
        .find(|function| function.name == "double")
        .expect("nested function")
        .id;
    let callback_module = Rc::clone(&nested_module);
    let callback_function_id = nested_function_id.clone();
    let calls = Rc::new(Cell::new(0));
    let callback_calls = Rc::clone(&calls);
    let slot = context
        .register_host_function(
            HostFunctionDescriptor::new(91, "consumer.double"),
            move |call, arguments| {
                assert!(call.this_value().is_undefined());
                callback_calls.set(callback_calls.get() + 1);
                let mut nested_host = NoHost;
                match call
                    .execute_nested(
                        &callback_module,
                        &callback_function_id,
                        arguments,
                        &mut nested_host,
                    )
                    .map_err(|_| HostError::CallbackFailed)?
                {
                    ExecutionCompletion::Return(value) => {
                        Ok(HostCallCompletion::Normal(value))
                    }
                    ExecutionCompletion::Throw(value) => Ok(HostCallCompletion::Throw(value)),
                    ExecutionCompletion::Interrupted(reason) => {
                        Ok(HostCallCompletion::Interrupted(reason))
                    }
                }
            },
        )
        .expect("register Rust callback");
    let callback_handle = context
        .create_host_function(slot)
        .expect("create persistent JS Function value");

    context.collect().expect("host Function value survives GC");
    let callback = context
        .resolve(callback_handle)
        .expect("callback root survives GC");
    let direct = context
        .execute(
            &invoke_id,
            &[callback, Value::number(21.0)],
            ExecutionRequest::default(),
        )
        .expect("ordinary JavaScript Call dispatches to Rust");
    assert!(matches!(
        direct.completion,
        ExecutionCompletion::Return(value) if value.as_number() == Some(42.0)
    ));
    assert_eq!(calls.get(), 1);

    let scheduled = context
        .execute(&schedule_id, &[callback], ExecutionRequest::default())
        .expect("Promise.resolve(...).then(hostFunction) executes");
    assert!(matches!(
        scheduled.completion,
        ExecutionCompletion::Return(_)
    ));
    assert_eq!(context.pending_promise_jobs(), 1);
    context.collect().expect("queued host callback survives GC");
    let pump = context
        .pump_promise_jobs(None)
        .expect("pump host PromiseJob");
    assert_eq!(pump.outcome, ogma_embedding::JobPumpOutcome::Progress);
    assert_eq!(pump.jobs_executed, 1);
    assert_eq!(context.pending_promise_jobs(), 0);
    assert_eq!(calls.get(), 2);

    context
        .unregister_host_function(slot)
        .expect("unregister invalidates old host Function token");
    let stale = context.execute(
        &invoke_id,
        &[callback, Value::number(3.0)],
        ExecutionRequest::default(),
    );
    assert!(matches!(
        stale,
        Err(ogma_embedding::ContextError::Interpreter(
            ogma_embedding::InterpreterError::HostCapabilityDenied(_)
        ))
    ));
    assert_eq!(
        calls.get(),
        2,
        "stale function must not reach a recycled slot"
    );

    let replacement_calls = Rc::new(Cell::new(0));
    let observed_replacement_calls = Rc::clone(&replacement_calls);
    let replacement_slot = context
        .register_host_function(
            HostFunctionDescriptor::new(91, "consumer.double"),
            move |_call, _arguments| {
                observed_replacement_calls.set(observed_replacement_calls.get() + 1);
                Ok(HostCallCompletion::Normal(Value::number(99.0)))
            },
        )
        .expect("re-register same stable ID and name in a new generation");
    let replacement_handle = context
        .create_host_function(replacement_slot)
        .expect("create replacement function");
    let replacement = context
        .resolve(replacement_handle)
        .expect("replacement value");
    let fresh = context
        .execute(
            &invoke_id,
            &[replacement, Value::number(3.0)],
            ExecutionRequest::default(),
        )
        .expect("replacement function is callable");
    assert!(matches!(
        fresh.completion,
        ExecutionCompletion::Return(value) if value.as_number() == Some(99.0)
    ));
    assert_eq!(replacement_calls.get(), 1);

    context
        .release(callback_handle)
        .expect("release old callback root");
    context
        .release(replacement_handle)
        .expect("release replacement root");
    context
        .unregister_host_function(replacement_slot)
        .expect("unregister replacement");
}
