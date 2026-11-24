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
    use ogma_core_ir::verify_document;
    use ogma_frontend::{ParseRequestOptions, SourceFile, parse};
    use oxc_allocator::Allocator;

    #[test]
    fn compiler_is_wired_to_the_authoritative_ir_and_bytecode_contracts() {
        assert_eq!(super::CORE_IR_CONTRACT_ID, "ogma.core-ir/1.0.0");
        assert_eq!(super::OUTPUT_FORMAT_VERSION, 1);
    }

    #[test]
    fn compiles_real_oxc_syntax_through_semantics_and_verified_core_ir() {
        let allocator = Allocator::default();
        let source = SourceFile::new(
            "compile-test",
            "memory:///compile-test.js",
            None,
            "function run(seed) { const value = seed + 1; return () => value; }",
        )
        .expect("source is valid");
        let parsed = parse(&allocator, &source, ParseRequestOptions::default())
            .expect("JavaScript parses and resolves");
        let compiled = compile(&parsed).expect("semantic analysis and lowering succeed");

        assert_eq!(compiled.functions.len(), parsed.syntax.functions.len());
        assert_eq!(compiled.semantic.source_id, source.id().as_str());
        assert_eq!(compiled.semantic.source_sha256, source.content_sha256());
        assert_eq!(
            compiled
                .semantic
                .functions
                .iter()
                .find(|function| function.is_arrow)
                .map(|function| function.captures.len()),
            Some(1)
        );
        assert!(
            compiled
                .functions
                .iter()
                .all(|document| verify_document(document).is_empty())
        );

        let closure = compiled
            .functions
            .iter()
            .flat_map(|document| document.function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
            .find(|instruction| {
                instruction.op == "closure.create"
                    && instruction
                        .attributes
                        .get("captures")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|captures| !captures.is_empty())
            })
            .expect("real arrow closure is lowered");
        assert_eq!(
            closure
                .attributes
                .get("captures")
                .and_then(serde_json::Value::as_array)
                .and_then(|captures| captures.first())
                .and_then(|capture| capture.get("mode"))
                .and_then(serde_json::Value::as_str),
            Some("value")
        );
    }
}
