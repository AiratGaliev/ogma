//! Rust application embedding boundary for Ogma.
//!
//! The production Runtime/Context lifecycle, opaque handles, host calls,
//! callbacks and panic containment are implemented by OGM-47. This crate
//! exposes the consumer-facing surface without pulling in the compiler.

#![deny(missing_docs)]

/// Version of the existing versioned Rust embedding ABI contract.
pub const ABI_VERSION: u16 = 1;

#[cfg(test)]
mod tests {
    #[test]
    fn embedding_uses_the_versioned_abi_baseline_and_portable_interpreter() {
        assert_eq!(super::ABI_VERSION, 1);
        assert_eq!(ogma_interpreter::INPUT_FORMAT_VERSION, 1);
    }
}
