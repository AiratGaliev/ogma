//! Compile-time pipeline boundary for JavaScript source, Core IR and OGMB.
//!
//! Frontend parsing, semantic analysis, canonical Core IR lowering and
//! bytecode emission are implemented by their respective M1 issues. This
//! crate is the only production boundary that composes those compile-time
//! dependencies; interpreter-only applications must not depend on it.

#![deny(missing_docs)]

pub mod bytecode;
pub mod lowering;
pub mod semantic;

use ogma_core_ir::{CONTRACT_ID, FunctionDocument};
use ogma_frontend::{ParseDiagnostic, ParsedProgram};

/// Canonical Core IR contract consumed by the compiler pipeline.
pub const CORE_IR_CONTRACT_ID: &str = CONTRACT_ID;

/// OGMB container version emitted by the compiler pipeline.
pub const OUTPUT_FORMAT_VERSION: u16 = ogma_bytecode::FORMAT_VERSION;

/// A compilation unit after semantic analysis and canonical Core IR lowering.
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledModule {
    /// Owned semantic bindings, environments, parameters and closure captures.
    pub semantic: semantic::SemanticModule,
    /// One independently verified Core IR v1 document per Program/function unit.
    pub functions: Vec<FunctionDocument>,
    /// Non-fatal frontend diagnostics retained from parsing.
    pub frontend_diagnostics: Vec<ParseDiagnostic>,
}

/// Failure during semantic analysis or canonical Core IR lowering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompileFailure {
    /// Frontend-resolved semantic facts cannot be represented by the M1 model.
    Semantic(Vec<semantic::SemanticDiagnostic>),
    /// A verified Core IR function cannot be faithfully represented by the M1 bytecode profile.
    Bytecode(Vec<ogma_bytecode::Diagnostic>),
    /// A source construct cannot be faithfully represented by Core IR v1.
    Lowering(Vec<lowering::LoweringDiagnostic>),
}

/// Runs the compiler's semantic-analysis and Core IR lowering stages for one
/// successfully parsed source unit.
///
/// The frontend remains responsible for syntax and early-error validation.
/// This function consumes its owned syntax/semantic projection, constructs the
/// compiler-owned semantic model, then emits and verifies canonical Core IR v1
/// documents. It does not execute code or emit bytecode.
pub fn compile(parsed: &ParsedProgram<'_>) -> Result<CompiledModule, CompileFailure> {
    let semantic =
        semantic::analyze(parsed.source, &parsed.semantic).map_err(CompileFailure::Semantic)?;
    let functions = lowering::lower(parsed.source, &semantic, &parsed.syntax)
        .map_err(CompileFailure::Lowering)?;
    Ok(CompiledModule {
        semantic,
        functions,
        frontend_diagnostics: parsed.diagnostics.clone(),
    })
}

impl CompiledModule {
    /// Emits and verifies one portable OGMB v1 artifact per Core IR function.
    ///
    /// Each artifact is independently encoded and validated by the bytecode
    /// boundary. This method does not execute code or claim interpreter
    /// compatibility beyond the registered M1 encoding profile.
    pub fn emit_bytecode(&self) -> Result<bytecode::CompiledBytecodeModule, CompileFailure> {
        let functions = bytecode::compile_functions(&self.functions)
            .map_err(CompileFailure::Bytecode)?;
        Ok(bytecode::CompiledBytecodeModule { functions })
    }
}

/// Compiles a parsed source unit directly to independently verified OGMB v1
/// function artifacts.
///
/// The existing Core IR-only compile API remains available for other
/// backends. Unsupported M1 semantics are returned as bytecode diagnostics
/// instead of being silently discarded.
pub fn compile_to_bytecode(
    parsed: &ParsedProgram<'_>,
) -> Result<bytecode::CompiledBytecodeModule, CompileFailure> {
    compile(parsed)?.emit_bytecode()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ogma_core_ir::{Source, verify_document};
    use ogma_frontend::{ParseRequestOptions, SourceFile, parse};
    use oxc_allocator::Allocator;

    #[test]
    fn compiler_is_wired_to_the_authoritative_ir_and_bytecode_contracts() {
        assert_eq!(super::CORE_IR_CONTRACT_ID, "ogma.core-ir/1.0.0");
        assert_eq!(super::OUTPUT_FORMAT_VERSION, 1);
    }

    #[test]
    fn compile_fails_closed_for_unsupported_try_catch_with_a_source_diagnostic() {
        let allocator = Allocator::default();
        let source = SourceFile::new(
            "compile-test",
            "memory:///compile-test.js",
            None,
            "try { throw 1; } catch (error) { error; }",
        )
        .expect("source is valid");
        let parsed = parse(&allocator, &source, ParseRequestOptions::default())
            .expect("valid try/catch syntax parses");
        let failure = compile(&parsed).expect_err("unsupported handler lowering must fail closed");
        let CompileFailure::Lowering(diagnostics) = failure else {
            panic!("valid syntax should reach the lowering diagnostic boundary");
        };
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "lowering.unsupported_statement")
            .expect("unsupported try statement diagnostic");
        let span = diagnostic
            .span
            .expect("unsupported syntax retains its source span");
        assert!(span.end_byte > span.start_byte);
        assert!(!diagnostic.message.is_empty());
    }

    #[test]
    fn rejects_const_tdz_read_through_real_frontend_pipeline() {
        let allocator = Allocator::default();
        let source = SourceFile::new(
            "const-tdz-test",
            "memory:///const-tdz-test.js",
            None,
            "function run() { const value = value; return value; }",
        )
        .expect("source is valid");
        let parsed = parse(&allocator, &source, ParseRequestOptions::default())
            .expect("valid const TDZ reference parses");

        let failure = compile(&parsed).expect_err("const TDZ read must fail closed");
        let CompileFailure::Lowering(diagnostics) = failure else {
            panic!("valid syntax should reach the const TDZ lowering diagnostic");
        };
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "lowering.const_tdz")
            .expect("const TDZ diagnostic");
        assert!(diagnostic.span.is_some());
        assert!(!diagnostic.message.is_empty());
    }

    #[test]
    fn rejects_hoisted_closure_capture_before_const_init_through_real_frontend_pipeline() {
        let allocator = Allocator::default();
        let source = SourceFile::new(
            "const-capture-tdz-test",
            "memory:///const-capture-tdz-test.js",
            None,
            "function run() { function read() { return value; } const value = 1; return read(); }",
        )
        .expect("source is valid");
        let parsed = parse(&allocator, &source, ParseRequestOptions::default())
            .expect("valid hoisted closure capture parses");

        let failure = compile(&parsed).expect_err("uninitialized const capture must fail closed");
        let CompileFailure::Lowering(diagnostics) = failure else {
            panic!("valid syntax should reach the const capture lowering diagnostic");
        };
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "lowering.const_capture_tdz")
            .expect("const capture TDZ diagnostic");
        assert!(diagnostic.span.is_some());
        assert!(!diagnostic.message.is_empty());
    }

    #[test]
    fn compiles_typeof_unresolvable_identifier_through_real_frontend_pipeline() {
        let allocator = Allocator::default();
        let source = SourceFile::new(
            "typeof-test",
            "memory:///typeof-test.js",
            None,
            "typeof missing;",
        )
        .expect("source is valid");
        let parsed = parse(&allocator, &source, ParseRequestOptions::default())
            .expect("typeof unresolved identifier is valid JavaScript");
        let compiled =
            compile(&parsed).expect("frontend semantic resolution and Core IR lowering succeed");
        let program = compiled
            .functions
            .iter()
            .find(|document| document.function.name == "<program>")
            .expect("Program Core IR document");
        let instructions: Vec<_> = program
            .function
            .blocks
            .iter()
            .flat_map(|block| block.instructions.iter())
            .collect();
        assert_eq!(instructions.len(), 2);

        let typeof_value = instructions
            .iter()
            .find(|instruction| {
                instruction
                    .attributes
                    .get("value")
                    .and_then(serde_json::Value::as_str)
                    == Some("undefined")
            })
            .expect("typeof unresolved identifier produces the string \"undefined\"");
        assert_eq!(typeof_value.op, "js.constant");
        assert_eq!(
            typeof_value
                .attributes
                .get("literal")
                .and_then(serde_json::Value::as_str),
            Some("string")
        );

        let return_value = program
            .function
            .blocks
            .iter()
            .find_map(|block| match &block.terminator {
                ogma_core_ir::Terminator::Return { value, .. } => value.as_deref(),
                _ => None,
            })
            .expect("Program completion returns a value");
        let completion = instructions
            .iter()
            .find(|instruction| {
                instruction
                    .result
                    .as_ref()
                    .is_some_and(|result| result.id == return_value)
            })
            .expect("Program completion value definition");
        assert_eq!(
            completion
                .attributes
                .get("literal")
                .and_then(serde_json::Value::as_str),
            Some("undefined")
        );
        assert!(
            completion
                .attributes
                .get("value")
                .is_some_and(serde_json::Value::is_null)
        );
        assert_eq!(
            program
                .function
                .return_signature
                .as_ref()
                .map(|signature| signature.semantic_type),
            Some(ogma_core_ir::SemanticType::Undefined)
        );
        assert!(program.function.effect_entry.is_none());
        assert!(verify_document(program).is_empty());
    }

    #[test]
    fn compiles_real_oxc_syntax_through_semantics_and_verified_core_ir() {
        let allocator = Allocator::default();
        let source = SourceFile::new(
            "compile-test",
            "memory:///compile-test.js",
            None,
            "let counter = 0; function increment() { counter = counter + 1; return counter; } function run(seed) { const value = seed + 1; return () => value; }",
        )
        .expect("source is valid");
        let parsed = parse(&allocator, &source, ParseRequestOptions::default())
            .expect("JavaScript parses and resolves");
        let compiled = compile(&parsed).expect("semantic analysis and lowering succeed");

        assert_eq!(compiled.functions.len(), parsed.syntax.functions.len());
        assert_eq!(compiled.semantic.source_id, source.id().as_str());
        assert_eq!(compiled.semantic.source_sha256, source.content_sha256());
        let increment = compiled
            .semantic
            .functions
            .iter()
            .find(|function| function.name == "increment")
            .expect("top-level function semantic descriptor exists");
        assert_eq!(
            increment.parent_function_scope.as_deref(),
            Some(parsed.semantic.root_scope.as_str())
        );
        assert_eq!(increment.captures.len(), 1);
        let arrow = compiled
            .semantic
            .functions
            .iter()
            .find(|function| function.is_arrow)
            .expect("nested arrow semantic descriptor exists");
        assert_eq!(arrow.captures.len(), 1);
        assert!(
            compiled
                .functions
                .iter()
                .all(|document| verify_document(document).is_empty())
        );

        let const_binding = compiled
            .semantic
            .bindings
            .iter()
            .find(|binding| binding.ir.name == "value")
            .expect("const binding in semantic model")
            .ir
            .id
            .as_str();
        let run_document = compiled
            .functions
            .iter()
            .find(|document| document.function.name == "run")
            .expect("run Core IR document");
        let initializer = run_document
            .function
            .blocks
            .iter()
            .flat_map(|block| block.instructions.iter())
            .find(|instruction| instruction.op == "js.add")
            .expect("const initializer SSA operation");
        let initializer_bindings = match &initializer.source {
            Source::User { bindings, .. } | Source::Generated { bindings, .. } => bindings,
        };
        assert!(
            initializer_bindings
                .iter()
                .any(|binding| binding == const_binding),
            "const binding identity must be attached to its SSA initializer definition"
        );

        let capture_modes: Vec<_> = compiled
            .functions
            .iter()
            .flat_map(|document| document.function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| instruction.op == "closure.create")
            .filter_map(|instruction| instruction.attributes.get("captures"))
            .filter_map(serde_json::Value::as_array)
            .flatten()
            .filter_map(|capture| capture.get("mode"))
            .filter_map(serde_json::Value::as_str)
            .collect();
        assert!(capture_modes.contains(&"cell"));
        assert!(capture_modes.contains(&"value"));
        let captured_const = compiled
            .functions
            .iter()
            .flat_map(|document| document.function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| instruction.op == "closure.create")
            .filter_map(|instruction| instruction.attributes.get("captures"))
            .filter_map(serde_json::Value::as_array)
            .flatten()
            .find(|capture| {
                capture.get("binding").and_then(serde_json::Value::as_str) == Some(const_binding)
            })
            .expect("closure captures the exact const binding identity");
        assert_eq!(
            captured_const
                .get("mode")
                .and_then(serde_json::Value::as_str),
            Some("value")
        );
    }
    #[test]
    fn returns_undefined_for_fallthrough_and_bare_return_through_real_frontend_pipeline() {
        let allocator = Allocator::default();
        let source = SourceFile::new(
            "undefined-return-test",
            "memory:///undefined-return-test.js",
            None,
            "function implicit() {} function explicit() { return; } function explicitUndefined() { return undefined; }",
        )
        .expect("source is valid");
        let parsed = parse(&allocator, &source, ParseRequestOptions::default())
            .expect("functions with implicit and bare returns parse");
        let compiled = compile(&parsed).expect("all JavaScript return paths lower to values");

        for name in ["implicit", "explicit", "explicitUndefined"] {
            let document = compiled
                .functions
                .iter()
                .find(|document| document.function.name == name)
                .expect("function Core IR document");
            let signature = document
                .function
                .return_signature
                .as_ref()
                .expect("JavaScript functions always complete with a value");
            assert_eq!(
                signature.semantic_type,
                ogma_core_ir::SemanticType::Undefined
            );
            assert_eq!(
                signature.representation,
                ogma_core_ir::Representation::Boxed
            );

            let returned: Vec<_> = document
                .function
                .blocks
                .iter()
                .filter_map(|block| match &block.terminator {
                    ogma_core_ir::Terminator::Return { value, .. } => value.as_ref(),
                    _ => None,
                })
                .collect();
            assert_eq!(returned.len(), 1);
            let return_value = returned[0];
            assert!(document.function.blocks.iter().any(|block| {
                block.instructions.iter().any(|instruction| {
                    instruction.op == "js.constant"
                        && instruction
                            .result
                            .as_ref()
                            .is_some_and(|result| result.id == *return_value)
                        && instruction
                            .attributes
                            .get("literal")
                            .and_then(serde_json::Value::as_str)
                            == Some("undefined")
                })
            }));
            assert!(verify_document(document).is_empty());
        }
    }
    #[test]
    fn rejects_default_parameters_through_real_frontend_pipeline() {
        let allocator = Allocator::default();
        let source = SourceFile::new(
            "default-parameter-lowering-test",
            "memory:///default-parameter-lowering-test.js",
            None,
            "function run(value = 1) { return value; } const callback = (value = 2) => value;",
        )
        .expect("source is valid");
        let parsed = parse(&allocator, &source, ParseRequestOptions::default())
            .expect("default parameters are valid JavaScript syntax");

        let failure = compile(&parsed)
            .expect_err("default parameter initializers must not be silently discarded");
        let CompileFailure::Semantic(diagnostics) = failure else {
            panic!("non-simple parameters must fail during semantic validation");
        };
        let parameter_diagnostics: Vec<_> = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "semantic.unsupported_parameter_pattern")
            .collect();
        assert_eq!(parameter_diagnostics.len(), 2);
        assert!(parameter_diagnostics.iter().all(|diagnostic| {
            diagnostic.span.is_some() && diagnostic.message.contains("default")
        }));
    }
    #[test]
    fn preserves_method_receiver_through_parenthesized_member_callees() {
        let allocator = Allocator::default();
        let source = SourceFile::new(
            "parenthesized-method-call-test",
            "memory:///parenthesized-method-call-test.js",
            None,
            "function invoke(receiver, key) { (receiver.method)(); return ((receiver[key]))(); }",
        )
        .expect("source is valid");
        let parsed = parse(&allocator, &source, ParseRequestOptions::default())
            .expect("parenthesized member calls are valid JavaScript");
        let compiled = compile(&parsed).expect("member calls lower to verified Core IR");

        let invoke = compiled
            .functions
            .iter()
            .find(|document| document.function.name == "invoke")
            .expect("invoke function Core IR document");
        let instructions: Vec<_> = invoke
            .function
            .blocks
            .iter()
            .flat_map(|block| block.instructions.iter())
            .collect();
        let property_get = instructions
            .iter()
            .find(|instruction| instruction.op == "js.get_property")
            .expect("static member lookup");
        let element_get = instructions
            .iter()
            .find(|instruction| instruction.op == "js.get_element")
            .expect("computed member lookup");
        let calls: Vec<_> = instructions
            .iter()
            .filter(|instruction| instruction.op == "js.call")
            .collect();
        assert_eq!(calls.len(), 2, "both parenthesized calls must be lowered");

        assert_eq!(
            calls[0].operands[0],
            property_get.result.as_ref().unwrap().id,
            "static method call must use the fetched method"
        );
        assert_eq!(
            calls[0].operands[1], property_get.operands[0],
            "parentheses must not replace a static member receiver with undefined"
        );
        assert_eq!(
            calls[1].operands[0],
            element_get.result.as_ref().unwrap().id,
            "computed method call must use the fetched method"
        );
        assert_eq!(
            calls[1].operands[1], element_get.operands[0],
            "nested parentheses must preserve a computed member receiver"
        );
        assert!(verify_document(invoke).is_empty());
    }

    #[test]
    fn uses_last_duplicate_sloppy_function_declaration_through_real_frontend_pipeline() {
        let allocator = Allocator::default();
        let source = SourceFile::new(
            "duplicate-function-declarations",
            "memory:///duplicate-function-declarations.js",
            None,
            "function choose() { return 1; } function choose() { return 2; } choose();",
        )
        .expect("source is valid");
        let parsed = parse(&allocator, &source, ParseRequestOptions::default())
            .expect("duplicate function declarations are valid in sloppy Script code");
        let compiled = compile(&parsed).expect("duplicate function binding lowers");

        let declarations: Vec<_> = compiled
            .semantic
            .functions
            .iter()
            .filter(|function| function.name == "choose")
            .collect();
        assert_eq!(declarations.len(), 2);
        let program = compiled
            .functions
            .iter()
            .find(|document| document.function.name == "<program>")
            .expect("Program Core IR document");
        let closures: Vec<_> = program
            .function
            .blocks
            .iter()
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| instruction.op == "closure.create")
            .filter_map(|instruction| instruction.attributes.get("function"))
            .filter_map(serde_json::Value::as_str)
            .collect();
        assert_eq!(closures, vec![declarations[1].id.as_str()]);
        assert!(verify_document(program).is_empty());
    }
    #[test]
    fn preserves_for_header_tdz_load_before_initialization_through_real_frontend_pipeline() {
        let allocator = Allocator::default();
        let source = SourceFile::new(
            "for-header-tdz-test",
            "memory:///for-header-tdz-test.js",
            None,
            "function run() { for (let index = index; index < 1; index = index + 1) {} }",
        )
        .expect("source is valid");
        let parsed = parse(&allocator, &source, ParseRequestOptions::default())
            .expect("self-referential for initializer is valid JavaScript syntax");
        let compiled = compile(&parsed)
            .expect("for-header TDZ reads are represented by runtime environment loads");

        let binding = compiled
            .semantic
            .bindings
            .iter()
            .find(|binding| binding.ir.name == "index")
            .expect("for-header lexical binding");
        let run = compiled
            .functions
            .iter()
            .find(|document| document.function.name == "run")
            .expect("run function Core IR document");
        let instructions: Vec<_> = run
            .function
            .blocks
            .iter()
            .flat_map(|block| block.instructions.iter())
            .collect();
        let load_index = instructions
            .iter()
            .position(|instruction| {
                instruction.op == "env.load"
                    && instruction
                        .attributes
                        .get("binding")
                        .and_then(serde_json::Value::as_str)
                        == Some(binding.ir.id.as_str())
            })
            .expect("self-reference must retain a runtime TDZ load");
        let store_index = instructions
            .iter()
            .position(|instruction| {
                instruction.op == "env.store"
                    && instruction
                        .attributes
                        .get("binding")
                        .and_then(serde_json::Value::as_str)
                        == Some(binding.ir.id.as_str())
            })
            .expect("for-header initializer store");
        assert!(
            load_index < store_index,
            "TDZ load must precede the first initialization store"
        );
        assert!(verify_document(run).is_empty());
    }

    #[test]
    fn rejects_for_header_let_when_the_for_statement_is_reentered_by_an_outer_loop() {
        let allocator = Allocator::default();
        let source = SourceFile::new(
            "nested-for-header-lifetime-test",
            "memory:///nested-for-header-lifetime-test.js",
            None,
            "function run() { let outer = 0; while (outer < 2) { for (let index = outer === 0 ? 0 : index; index < 1; index = index + 1) {} outer = outer + 1; } }",
        )
        .expect("source is valid");
        let parsed = parse(&allocator, &source, ParseRequestOptions::default())
            .expect("conditional for initializer is valid JavaScript syntax");

        let failure = compile(&parsed)
            .expect_err("a re-entered for-header scope needs a fresh lexical environment");
        let CompileFailure::Lowering(diagnostics) = failure else {
            panic!("valid syntax should reach lexical environment lifetime validation");
        };
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "lowering.block_environment_lifetime")
            .expect("re-entered for-header lifetime diagnostic");
        assert!(diagnostic.span.is_some());
        assert!(diagnostic.message.contains("loop-reentered"));
    }
    #[test]
    fn rejects_reentered_block_let_without_a_fresh_lexical_environment() {
        let allocator = Allocator::default();
        let source = SourceFile::new(
            "block-tdz-reentry-test",
            "memory:///block-tdz-reentry-test.js",
            None,
            "function run() { let index = 0; while (index < 2) { if (index === 1) { value; } let value = 1; index = index + 1; } }",
        )
        .expect("source is valid");
        let parsed = parse(&allocator, &source, ParseRequestOptions::default())
            .expect("lexical reference before declaration parses");

        let failure = compile(&parsed)
            .expect_err("block-scoped let must not be flattened into a persistent function slot");
        let CompileFailure::Lowering(diagnostics) = failure else {
            panic!("valid syntax should reach the lexical environment lowering diagnostic");
        };
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "lowering.block_environment_lifetime")
            .expect("block environment lifetime diagnostic");
        assert!(diagnostic.span.is_some());
        assert!(diagnostic.message.contains("fresh lexical environment"));
    }
    #[test]
    fn lowers_for_header_let_binding_through_real_frontend_pipeline() {
        let allocator = Allocator::default();
        let source = SourceFile::new(
            "for-header-let-test",
            "memory:///for-header-let-test.js",
            None,
            "function run() { for (let index = 0; index < 2; index = index + 1) { index; } }",
        )
        .expect("source is valid");
        let parsed = parse(&allocator, &source, ParseRequestOptions::default())
            .expect("for-header lexical declaration parses");
        let compiled = compile(&parsed).expect(
            "for-header lexical environment is supported by the synchronous lowering subset",
        );

        let run = compiled
            .functions
            .iter()
            .find(|document| document.function.name == "run")
            .expect("run function Core IR document");
        assert!(verify_document(run).is_empty());
        assert!(run.function.blocks.iter().any(|block| {
            block.instructions.iter().any(|instruction| {
                instruction.op == "env.store" && instruction.attributes.contains_key("binding")
            })
        }));
    }
    #[test]
    fn compiles_register_artifacts_and_accepts_them_with_the_reference_verifier() {
        use std::process::Command;

        let cases = [
            (
                "arithmetic_and_calls",
                "function add(left, right) { return left + right; } add(40, 2);",
            ),
            (
                "branch_and_loop",
                "function sum(value) { let total = 0; while (value > 0) { total = total + value; value = 0; } if (total > 0) { return total; } return 0; } sum(3);",
            ),
            (
                "property_access",
                "function read(value) { return value.name; } read(42);",
            ),
            (
                "closure_capture",
                "function makeCounter() { let count = 0; const step = 1; function increment() { count = count + step; return count; } return increment; } makeCounter();",
            ),
            (
                "propagated_exception",
                "function fail() { throw 42; } fail();",
            ),
        ];

        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repository_root = manifest_dir
            .parent()
            .and_then(std::path::Path::parent)
            .expect("compiler crate is inside the workspace");
        let verifier = repository_root.join(".github/scripts/verify_ogmb.py");
        assert!(verifier.is_file(), "repository OGMB verifier must exist");

        for (case_name, source_text) in cases {
            let allocator = Allocator::default();
            let file = SourceFile::new(
                case_name,
                format!("fixtures/{case_name}.js"),
                None,
                source_text,
            )
            .expect("source fixture is valid");
            let parsed = parse(&allocator, &file, ParseRequestOptions::default())
                .expect("source fixture parses");
            let compiled = compile_to_bytecode(&parsed)
                .unwrap_or_else(|error| panic!("{case_name} did not compile: {error:?}"));
            assert!(!compiled.functions.is_empty(), "{case_name} emitted no functions");

            for function in compiled.functions {
                assert!(function.metrics.instruction_count > 0);
                assert!(function.metrics.code_bytes > 0);
                assert_eq!(
                    function.metrics.instruction_count as usize,
                    function.artifact.verification_input["instructions"].as_array().unwrap().len()
                );
                assert_eq!(function.metrics.code_bytes, function.artifact.code.len());
                assert_eq!(function.metrics.constant_count, function.artifact.constants.len());
                assert_eq!(
                    function.metrics.register_count,
                    function.artifact.manifest["metadata"]["frame_layout"]["register_count"].as_u64().unwrap() as u32
                );

                let safe_function_id = function.id.chars().map(|character| {
                    if matches!(character, ':' | '/' | '\\') { '_' } else { character }
                }).collect::<String>();
                let sequence = format!("{}_{}", std::process::id(), safe_function_id);
                let container_path = std::env::temp_dir().join(format!("ogm45_{sequence}.ogmb"));
                let program_path = std::env::temp_dir().join(format!("ogm45_{sequence}.json"));
                std::fs::write(&container_path, &function.artifact.bytes)
                    .expect("write generated OGMB artifact");
                std::fs::write(
                    &program_path,
                    serde_json::to_vec(&function.artifact.verification_input)
                        .expect("serialize verifier input"),
                )
                .expect("write normalized verification input");

                let mut result = None;
                for interpreter in ["python3", "python"] {
                    match Command::new(interpreter)
                        .arg(&verifier)
                        .arg("--container")
                        .arg(&container_path)
                        .arg("--program")
                        .arg(&program_path)
                        .current_dir(repository_root)
                        .output()
                    {
                        Ok(output) => {
                            result = Some(output);
                            break;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => panic!("could not launch reference verifier: {error}"),
                    }
                }
                let output = result.expect("Python is required by the repository Fast CI");
                let _ = std::fs::remove_file(&container_path);
                let _ = std::fs::remove_file(&program_path);
                assert!(
                    output.status.success(),
                    "reference verifier rejected {case_name}/{}: stdout={} stderr={}",
                    function.id,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                );
            }
        }
    }

}
