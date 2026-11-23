//! Backend-neutral JavaScript Runtime and heap boundary.
//!
//! Runtime Values, objects, explicit roots, heap allocation and collection are
//! implemented by the M1 Runtime work. The Runtime must not depend on parser,
//! compiler, Core IR or bytecode implementation crates.

#![deny(missing_docs)]
