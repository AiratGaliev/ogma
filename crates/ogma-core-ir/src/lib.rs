//! Canonical, backend-neutral function-level Core IR model for Ogma.
//!
//! The owned Rust representation and validation boundary are implemented by
//! the M1 Core IR work. This crate starts with the existing versioned contract
//! as its single source of truth.

#![deny(missing_docs)]

/// Identifier of the canonical Core IR v1 contract.
pub const CONTRACT_ID: &str = "ogma.core-ir/1.0.0";

#[cfg(test)]
mod tests {
    #[test]
    fn contract_identifier_matches_the_existing_versioned_contract() {
        assert_eq!(super::CONTRACT_ID, "ogma.core-ir/1.0.0");
    }
}
