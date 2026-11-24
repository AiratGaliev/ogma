//! Compile-time pipeline boundary for JavaScript source, Core IR and OGMB.
//!
//! Frontend parsing, semantic analysis, canonical Core IR lowering and
//! bytecode emission are implemented by their respective M1 issues. This
//! crate is the only production boundary that composes those compile-time
//! dependencies; interpreter-only applications must not depend on it.

#![deny(missing_docs)]

pub mod semantic;
pub mod lowering;

/// Canonical Core IR contract consumed by the compiler pipeline.
pub const CORE_IR_CONTRACT_ID: &str = ogma_core_ir::CONTRACT_ID;

/// OGMB container version emitted by the compiler pipeline.
pub const OUTPUT_FORMAT_VERSION: u16 = ogma_bytecode::FORMAT_VERSION;

#[cfg(test)]
mod tests {
    #[test]
    fn compiler_is_wired_to_the_authoritative_ir_and_bytecode_contracts() {
        assert_eq!(super::CORE_IR_CONTRACT_ID, "ogma.core-ir/1.0.0");
        assert_eq!(super::OUTPUT_FORMAT_VERSION, 1);
    }
}
