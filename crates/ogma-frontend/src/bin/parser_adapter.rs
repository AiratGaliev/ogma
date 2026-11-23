//! Line-delimited JSON parser adapter for the pinned Ogma conformance runner.

use std::io::{self, BufRead, Read, Write};

use oxc_allocator::Allocator;
use serde_json::{Value, json};

use ogma_frontend::{
    DiagnosticPhase, ParseGoal, ParseMode, ParseRequestOptions, SourceFile, normalized_ast_sha256,
    parse,
};

const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

fn response_envelope(request_id: u64, body: Value) -> Value {
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

fn error_response(request_id: u64, outcome: &str, reason: &str) -> Value {
    response_envelope(
        request_id,
        json!({
            "outcome": outcome,
            "reason": reason
        }),
    )
}

fn process_request(request: Value) -> Value {
    let request_id = request
        .get("requestId")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if request.get("protocol").and_then(Value::as_str) != Some("ogma-test-runner-adapter")
        || request.get("version").and_then(Value::as_u64) != Some(1)
    {
        return error_response(
            request_id,
            "harness_error",
            "invalid adapter request envelope",
        );
    }

    let Some(test) = request.get("test").and_then(Value::as_object) else {
        return error_response(
            request_id,
            "harness_error",
            "request.test must be an object",
        );
    };
    if test.get("suite").and_then(Value::as_str) != Some("test262-parser-tests") {
        return error_response(
            request_id,
            "unsupported",
            "this adapter implements parse-only test262-parser-tests requests; runtime Test262 execution is owned by a separate adapter",
        );
    }

    let string_field = |name: &str| test.get(name).and_then(Value::as_str).map(str::to_owned);
    let Some(test_id) = string_field("id") else {
        return error_response(request_id, "harness_error", "test.id must be a string");
    };
    let Some(source) = string_field("source") else {
        return error_response(request_id, "harness_error", "test.source must be a string");
    };
    let Some(test_path) = string_field("testPath") else {
        return error_response(
            request_id,
            "harness_error",
            "test.testPath must be a string",
        );
    };
    let Some(source_type) = string_field("sourceType") else {
        return error_response(
            request_id,
            "harness_error",
            "test.sourceType must be a string",
        );
    };

    let goal = match source_type.as_str() {
        "script" => ParseGoal::Script,
        "module" => ParseGoal::Module,
        _ => {
            return error_response(
                request_id,
                "harness_error",
                "test.sourceType must be script or module",
            );
        }
    };
    let mode = match string_field("mode").as_deref() {
        Some("script") | Some("sloppy") => ParseMode::Sloppy,
        Some("module") | Some("strict") => ParseMode::Strict,
        Some(other) => {
            return error_response(
                request_id,
                "harness_error",
                &format!("unsupported parser mode: {other}"),
            );
        }
        None => {
            return error_response(request_id, "harness_error", "test.mode must be a string");
        }
    };

    let source_file = match SourceFile::new(
        test_id,
        format!("test262-parser-tests://{test_path}"),
        None,
        source,
    ) {
        Ok(source_file) => source_file,
        Err(error) => {
            return error_response(request_id, "harness_error", &error.to_string());
        }
    };
    let allocator = Allocator::default();
    let options = ParseRequestOptions { goal, mode };
    match parse(&allocator, &source_file, options) {
        Ok(parsed) => {
            let mut body = json!({ "outcome": "parsed" });
            if test
                .get("equivalenceGroup")
                .is_some_and(|value| !value.is_null())
            {
                body["normalizedAstSha256"] = json!(normalized_ast_sha256(&parsed.program));
            }
            response_envelope(request_id, body)
        }
        Err(failure) => {
            let phase = match failure.phase {
                DiagnosticPhase::Parse => "parse",
                DiagnosticPhase::Early => "early",
            };
            response_envelope(
                request_id,
                json!({
                    "outcome": "rejected",
                    "phase": phase,
                    "diagnostics": failure.diagnostics.iter().map(|diagnostic| {
                        json!({
                            "phase": phase,
                            "code": diagnostic.code,
                            "message": diagnostic.message,
                            "labels": diagnostic.labels.iter().map(|label| {
                                json!({
                                    "startByte": label.span.start_byte,
                                    "endByte": label.span.end_byte,
                                    "startLine": label.span.start.line,
                                    "startColumnUtf16": label.span.start.column_utf16,
                                    "endLine": label.span.end.line,
                                    "endColumnUtf16": label.span.end.column_utf16,
                                    "primary": label.primary
                                })
                            }).collect::<Vec<_>>()
                        })
                    }).collect::<Vec<_>>()
                }),
            )
        }
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
            Err(error) => error_response(
                0,
                "harness_error",
                &format!("request is not valid JSON: {error}"),
            ),
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
    use serde_json::json;

    use super::process_request;

    fn request(test_id: &str, source: &str, source_type: &str) -> serde_json::Value {
        json!({
            "protocol": "ogma-test-runner-adapter",
            "version": 1,
            "requestId": 7,
            "test": {
                "id": test_id,
                "suite": "test262-parser-tests",
                "source": source,
                "mode": source_type,
                "sourceType": source_type,
                "testPath": test_id,
                "equivalenceGroup": null
            }
        })
    }

    #[test]
    fn adapter_returns_parse_success_for_valid_script() {
        let response = process_request(request("pass/example.js", "const value = 1;", "script"));
        assert_eq!(response["outcome"], "parsed");
        assert_eq!(response["requestId"], 7);
    }

    #[test]
    fn adapter_classifies_early_errors() {
        let response = process_request(request(
            "early/example.js",
            "let duplicate; let duplicate;",
            "script",
        ));
        assert_eq!(response["outcome"], "rejected");
        assert_eq!(response["phase"], "early");
    }

    #[test]
    fn adapter_classifies_invalid_grammar_as_parse_failure() {
        let response = process_request(request("fail/example.js", "var x, ;", "script"));
        assert_eq!(response["outcome"], "rejected");
        assert_eq!(response["phase"], "parse");
    }

    #[test]
    fn runtime_test262_is_not_misreported_as_parser_acceptance() {
        let mut request = request("language/example.js", "1 + 1", "script");
        request["test"]["suite"] = json!("test262");
        let response = process_request(request);
        assert_eq!(response["outcome"], "unsupported");
    }
}
