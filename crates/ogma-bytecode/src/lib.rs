//! Portable Ogma bytecode container and verifier boundary.
//!
//! The decoder, encoder and production verifier are implemented by the M1
//! bytecode work. This crate intentionally has no Runtime or compiler
//! dependency; the interpreter and compiler consume the portable format from
//! opposite sides.

#![deny(missing_docs)]

/// Current version of the portable OGMB container contract.
pub const FORMAT_VERSION: u16 = 1;

#[cfg(test)]
mod tests {
    #[test]
    fn container_contract_starts_at_v1() {
        assert_eq!(super::FORMAT_VERSION, 1);
    }
}
