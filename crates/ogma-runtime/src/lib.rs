//! Backend-neutral JavaScript Runtime and heap boundary.
//!
//! The Runtime owns JavaScript values, a non-moving managed heap, explicit
//! roots and persistent host handles. See `docs/runtime-value-heap-contract.md`
//! for the M1 representation, lifetime and allocation contracts.
//! It must not depend on parser,
//! compiler, Core IR or bytecode implementation crates.

#![deny(missing_docs)]

mod heap;
mod value;

pub use heap::{
    CollectionReport, ExternalMemoryCharge, FrameToken, Heap, HeapConfig, HeapError, HeapStats,
    HostHandle, RootSnapshot, ScopeToken,
};
pub use value::{Value, ValueType};
