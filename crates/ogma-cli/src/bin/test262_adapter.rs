//! Production M1 source-execution adapter for bounded Test262 and differential smoke tests.
//!
//! This executable is a test tool. It is not linked into the Runtime and does
//! not add a QuickJS dependency to production Ogma.

use std::io::{self, BufRead, Read, Write};
use std::time::Duration;

use ogma_embedding::{Engine, ExecutionRequest, PersistentCompletion};
use ogma_frontend::{
    ParseGoal, ParseMode, ParseRequestOptions, SourceFile, parse,
};
use ogma_compiler::compile_to_bytecode;
use ogma_runtime::ValueType;
use oxc_allocator::Allocator;
use serde_json::{Value, json};

const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
const MAX_TIMEOUT_MS: u64 = 30_000;

fn envelope(request_id: u64, body: Value) -> Value {
    let mut result = json!({
        "protocol": "ogma-test-runner-adapter",
        "version": 1,
        "requestId": request_id
    });
    if let (Some(target), Some(source)) = (result.as_object_mut(), body.as_object()) {
        target.extend(source.clone());
    }
    result
}

fn error_response(request_id: u64, outcome: &str, reason: impl AsRef<str>) -> Value {
    envelope(request_id, json!({"outcome": outcome, "reason": reason.as_ref()}))
}

fn response_value(context: &ogma_embedding::Context<'_>, value: ogma_runtime::Value) -> Result<Value, String> {
    match value.value_type() {
        ValueType::Undefined => Ok(json!({"type": "undefined"})),
        ValueType::Null => Ok(json!({"type": "null"})),
        ValueType::Boolean => Ok(json!({"type": "boolean", "value": value.as_boolean().expect("boolean tag")})),
        ValueType::Number => {
            let number = value.as_number().expect("number tag");
            if number.is_finite() {
                Ok(json!({"type": "number", "value": number}))
            } else {
                let name = if number.is_nan() {
                    "NaN"
                } else if number.is_sign_positive() {
                    "Infinity"
                } else {
                    "-Infinity"
                };
                Ok(json!({"type": "number", "value": name}))
            }
        }
        ValueType::String => {
            let units = context
                .read_string_utf16(value)
                .map_err(|error| format!("cannot read Runtime string: {error:?}"))?;
            let text = String::from_utf16(&units)
                .map_err(|error| format!("differential result contains unpaired UTF-16: {error}"))?;
            Ok(json!({"type": "string", "value": text}))
        }
        ValueType::Object | ValueType::Function => Err(
            "differential smoke currently accepts only observable primitive return values".to_owned(),
        ),
    }
}

fn process_request(request: Value) -> Value {
    let request_id = request
        .get("requestId")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if request.get("protocol").and_then(Value::as_str) != Some("ogma-test-runner-adapter")
        || request.get("version").and_then(Value::as_u64) != Some(1)
    {
        return error_response(request_id, "harness_error", "invalid adapter request envelope");
    }

    let Some(test) = request.get("test").and_then(Value::as_object) else {
        return error_response(request_id, "harness_error", "request.test must be an object");
    };
    let suite = test.get("suite").and_then(Value::as_str).unwrap_or_default();
    let differential = suite == "ogma-differential";
    if suite != "test262" && !differential {
        return error_response(request_id, "unsupported", "this worker executes only bounded Test262 and Ogma differential requests");
    }

    let field = |name: &str| test.get(name).and_then(Value::as_str);
    let Some(test_id) = field("id") else {
        return error_response(request_id, "harness_error", "test.id must be a string");
    };
    let Some(source_text) = field("source") else {
        return error_response(request_id, "harness_error", "test.source must be a string");
    };
    if field("sourceType").unwrap_or("script") != "script" {
        return error_response(request_id, "unsupported", "the bounded M1 runtime smoke supports Script source only");
    }
    if !differential {
        if test.get("async").and_then(Value::as_bool).unwrap_or(false) {
            return error_response(request_id, "unsupported", "async Test262 execution is outside the bounded M1 profile");
        }
        if test.get("includes").and_then(Value::as_array).is_some_and(|items| !items.is_empty()) {
            return error_response(request_id, "unsupported", "Test262 harness includes are outside the bounded raw-test profile");
        }
        if test.get("raw").and_then(Value::as_bool) != Some(true) {
            return error_response(request_id, "unsupported", "only raw Test262 tests are enabled in the M1 smoke profile");
        }
    }

    let mode = match field("mode") {
        Some("strict") => ParseMode::Strict,
        Some("sloppy") | Some("script") | None => ParseMode::Sloppy,
        Some(other) => {
            return error_response(request_id, "harness_error", format!("invalid Script mode {other:?}"));
        }
    };
    let source = match SourceFile::new(
        test_id,
        format!("ogma-test262://{test_id}"),
        None,
        source_text,
    ) {
        Ok(source) => source,
        Err(error) => return error_response(request_id, "harness_error", error.to_string()),
    };
    let allocator = Allocator::default();
    let parsed = match parse(
        &allocator,
        &source,
        ParseRequestOptions {
            goal: ParseGoal::Script,
            mode,
        },
    ) {
        Ok(parsed) => parsed,
        Err(failure) => {
            let diagnostics = failure
                .diagnostics
                .iter()
                .map(|item| json!({
                    "phase": format!("{:?}", item.phase).to_lowercase(),
                    "code": item.code,
                    "message": item.message,
                    "severity": format!("{:?}", item.severity).to_lowercase()
                }))
                .collect::<Vec<_>>();
            return envelope(
                request_id,
                json!({
                    "outcome": "threw",
                    // Test262 classifies ECMAScript early errors as parse-phase SyntaxErrors.
                    "phase": "parse",
                    "errorName": "SyntaxError",
                    "diagnostics": diagnostics
                }),
            );
        }
    };

    let compiled = match compile_to_bytecode(&parsed) {
        Ok(module) => module,
        Err(error) => {
            return error_response(
                request_id,
                "harness_error",
                format!("M1 source compilation failed for an enabled test: {error:?}"),
            );
        }
    };
    if compiled.functions.is_empty() {
        return error_response(request_id, "harness_error", "compiler emitted no executable functions");
    }
    let program_id = compiled.functions[0].id.clone();
    let artifacts = compiled
        .functions
        .iter()
        .map(|function| function.artifact.bytes.as_slice())
        .collect::<Vec<_>>();

    let engine = match Engine::new(Default::default()) {
        Ok(engine) => engine,
        Err(error) => return error_response(request_id, "harness_error", format!("cannot create Ogma Engine: {error:?}")),
    };
    let mut runtime = match engine.create_runtime() {
        Ok(runtime) => runtime,
        Err(error) => return error_response(request_id, "harness_error", format!("cannot create fresh Runtime: {error:?}")),
    };
    let mut context = match runtime.create_context() {
        Ok(context) => context,
        Err(error) => return error_response(request_id, "harness_error", format!("cannot create Runtime Context: {error:?}")),
    };
    if let Err(error) = context.load_module(artifacts) {
        return error_response(request_id, "harness_error", format!("Runtime rejected compiler OGMB output: {error:?}"));
    }
    let functions = match context.functions() {
        Ok(functions) => functions,
        Err(error) => return error_response(request_id, "harness_error", format!("cannot inspect loaded Runtime functions: {error:?}")),
    };
    let entry = if differential {
        let Some(name) = field("entryFunction") else {
            return error_response(request_id, "harness_error", "differential request requires test.entryFunction");
        };
        functions.iter().find(|function| function.name == name)
    } else {
        functions.iter().find(|function| function.id == program_id)
    };
    let Some(entry) = entry else {
        return error_response(request_id, "harness_error", "requested source entry function was not found in the verified Runtime module");
    };

    let requested_timeout = request
        .get("timeoutMs")
        .and_then(Value::as_u64)
        .unwrap_or(5_000)
        .clamp(1, MAX_TIMEOUT_MS);
    let report = match context.execute_persistent(
        &entry.id,
        &[],
        ExecutionRequest {
            instruction_budget: Some(100_000),
            time_budget: Some(Duration::from_millis(requested_timeout)),
            ..ExecutionRequest::default()
        },
    ) {
        Ok(report) => report,
        Err(error) => return error_response(request_id, "harness_error", format!("Runtime execution failed: {error:?}")),
    };

    match report.completion {
        PersistentCompletion::Return(handle) => {
            let value = context.resolve(handle);
            let response = if differential {
                match value {
                    Ok(value) => match response_value(&context, value) {
                        Ok(value) => envelope(request_id, json!({
                            "outcome": "completed",
                            "completion": "return",
                            "value": value,
                            "instructionsExecuted": report.instructions_executed
                        })),
                        Err(reason) => error_response(request_id, "harness_error", reason),
                    },
                    Err(error) => error_response(request_id, "harness_error", format!("cannot resolve Runtime return value: {error:?}")),
                }
            } else {
                envelope(request_id, json!({
                    "outcome": "completed",
                    "instructionsExecuted": report.instructions_executed
                }))
            };
            if let Err(error) = context.release(handle) {
                return error_response(request_id, "harness_error", format!("cannot release persistent Runtime result: {error:?}"));
            }
            response
        }
        PersistentCompletion::Throw(handle) => {
            let _ = context.release(handle);
            envelope(request_id, json!({
                "outcome": "threw",
                "phase": "runtime",
                "errorName": "Error",
                "instructionsExecuted": report.instructions_executed
            }))
        }
        PersistentCompletion::Interrupted(reason) => error_response(
            request_id,
            "harness_error",
            format!("bounded Runtime execution interrupted: {reason:?}"),
        ),
    }
}

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let mut reader = io::BufReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut writer = io::BufWriter::new(stdout.lock());
    let mut line = Vec::new();

    loop {
        line.clear();
        let bytes_read = reader
            .by_ref()
            .take((MAX_REQUEST_BYTES + 1) as u64)
            .read_until(b'\n', &mut line)?;
        if bytes_read == 0 {
            break;
        }
        if line.len() > MAX_REQUEST_BYTES {
            let response = error_response(0, "harness_error", "request exceeds 16 MiB protocol limit");
            serde_json::to_writer(&mut writer, &response)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            writer.write_all(b"\n")?;
            writer.flush()?;
            break;
        }
        let response = match serde_json::from_slice::<Value>(&line) {
            Ok(request) => process_request(request),
            Err(error) => error_response(0, "harness_error", format!("invalid JSON request: {error}")),
        };
        serde_json::to_writer(&mut writer, &response)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(suite: &str, source: &str, mode: &str) -> Value {
        json!({
            "protocol": "ogma-test-runner-adapter",
            "version": 1,
            "requestId": 7,
            "timeoutMs": 5000,
            "test": {
                "id": "ogm-50-adapter-test.js",
                "suite": suite,
                "source": source,
                "sourceType": "script",
                "mode": mode,
                "raw": true,
                "async": false,
                "includes": []
            }
        })
    }

    #[test]
    fn production_adapter_executes_source_through_verified_ogmb_and_runtime() {
        let response = process_request(request(
            "test262",
            "var total = 1 + 2; if (total !== 3) { throw 'bad'; }",
            "sloppy",
        ));
        assert_eq!(response["outcome"], "completed");
        assert_eq!(response["requestId"], 7);
    }

    #[test]
    fn production_adapter_maps_strict_early_errors_to_test262_parse_syntax_errors() {
        let response = process_request(request(
            "test262",
            "'use strict'; eval = 42;",
            "sloppy",
        ));
        assert_eq!(response["outcome"], "threw");
        assert_eq!(response["phase"], "parse");
        assert_eq!(response["errorName"], "SyntaxError");
    }

    #[test]
    fn differential_adapter_returns_observable_runtime_primitive() {
        let mut value = request(
            "ogma-differential",
            "function __ogma_differential_entry__() { return '' + (1 + 2); }",
            "sloppy",
        );
        value["test"]["entryFunction"] = json!("__ogma_differential_entry__");
        let response = process_request(value);
        assert_eq!(response["outcome"], "completed");
        assert_eq!(response["completion"], "return");
        assert_eq!(response["value"], json!({"type": "string", "value": "3"}));
    }
}
