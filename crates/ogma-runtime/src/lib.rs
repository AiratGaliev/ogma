//! Backend-neutral JavaScript Runtime and heap boundary.
//!
//! The Runtime owns JavaScript values, a non-moving managed heap, explicit
//! roots and persistent host handles. Values and opaque tokens are !Send/!Sync
//! and remain on the owning Runtime thread. See
//! `docs/runtime-value-heap-contract.md` for the M1 representation, lifetime
//! and allocation contracts. It must not depend on parser, compiler, Core IR or
//! bytecode implementation crates.
//!
//! Thread affinity is enforced at the type level:
//!
//! ```compile_fail
//! use ogma_runtime::{Heap, HeapConfig};
//!
//! fn require_send<T: Send>(_: T) {}
//!
//! let mut heap = Heap::new(HeapConfig::default()).unwrap();
//! let value = heap.allocate_object().unwrap();
//! let host = heap.retain_host_value(value).unwrap();
//! let frame = heap.enter_frame().unwrap();
//! let scope = heap.open_scope().unwrap();
//! let external = heap.charge_external_memory(1).unwrap();
//! require_send(value);
//! require_send(host);
//! require_send(frame);
//! require_send(scope);
//! require_send(external);
//! ```
//!
//! Opaque handles and root tokens share the same !Send/!Sync boundary.

#![deny(missing_docs)]

mod heap;
mod value;

pub use heap::{
    CollectionReport, ExternalMemoryCharge, FrameToken, Heap, HeapConfig, HeapError, HeapStats,
    HostHandle, RootSnapshot, ScopeToken,
};
pub use value::{Value, ValueType};
