//! Portable bytecode interpreter boundary.
//!
//! The VM consumes verified OGMB and delegates language operations to the
//! shared Runtime. Keeping this crate separate from the compiler makes the
//! interpreter-only dependency graph independent of frontend and Core IR
//! implementation dependencies.

#![deny(missing_docs)]

/// OGMB container version consumed by the interpreter boundary.
pub const INPUT_FORMAT_VERSION: u16 = ogma_bytecode::FORMAT_VERSION;

#[cfg(test)]
mod tests {
    #[test]
    fn interpreter_uses_the_existing_portable_bytecode_version() {
        assert_eq!(super::INPUT_FORMAT_VERSION, 1);
    }
}
