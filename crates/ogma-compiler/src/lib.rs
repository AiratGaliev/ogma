//! Compile-time pipeline boundary for JavaScript source, Core IR and OGMB.
//!
//! Frontend parsing, semantic analysis, canonical Core IR lowering and
//! bytecode emission are implemented by their respective M1 issues. This
//! crate is the only production boundary that composes those compile-time
//! dependencies; interpreter-only applications must not depend on it.

#![deny(missing_docs)]

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
        assert_eq!(instructions.len(), 1);
        assert_eq!(instructions[0].op, "js.constant");
        assert_eq!(
            instructions[0]
                .attributes
                .get("value")
                .and_then(serde_json::Value::as_str),
            Some("undefined")
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
}
