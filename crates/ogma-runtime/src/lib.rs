//! Backend-neutral JavaScript Runtime and heap boundary.
//!
//! The Runtime owns JavaScript values, a non-moving managed heap, explicit
//! roots and persistent host handles. Values and opaque tokens are !Send/!Sync
//! and remain on the owning Runtime thread. See
//! `docs/runtime-value-heap-contract.md` for the M1 representation, lifetime
//! and allocation contracts. It must not depend on parser, compiler, Core IR or
//! bytecode implementation crates.
//!
//! Thread affinity is enforced at the type level. Each public value/token is
//! independently prevented from satisfying `Send + Sync`:
//!
//! ```compile_fail
//! use ogma_runtime::Value;
//! fn require_send_sync<T: Send + Sync>(_: T) {}
//! require_send_sync(Value::number(1.0));
//! ```
//!
//! ```compile_fail
//! use ogma_runtime::{Heap, HeapConfig};
//! fn require_send_sync<T: Send + Sync>(_: T) {}
//! let mut heap = Heap::new(HeapConfig::default()).unwrap();
//! let value = heap.allocate_object().unwrap();
//! require_send_sync(heap.retain_host_value(value).unwrap());
//! ```
//!
//! ```compile_fail
//! use ogma_runtime::{Heap, HeapConfig};
//! fn require_send_sync<T: Send + Sync>(_: T) {}
//! let mut heap = Heap::new(HeapConfig::default()).unwrap();
//! require_send_sync(heap.enter_frame().unwrap());
//! ```
//!
//! ```compile_fail
//! use ogma_runtime::{Heap, HeapConfig};
//! fn require_send_sync<T: Send + Sync>(_: T) {}
//! let mut heap = Heap::new(HeapConfig::default()).unwrap();
//! require_send_sync(heap.open_scope().unwrap());
//! ```
//!
//! ```compile_fail
//! use ogma_runtime::{Heap, HeapConfig};
//! fn require_send_sync<T: Send + Sync>(_: T) {}
//! let mut heap = Heap::new(HeapConfig::default()).unwrap();
//! require_send_sync(heap.charge_external_memory(1).unwrap());
//! ```
//!
//! The heap and all public values/tokens share the same !Send/!Sync boundary.

#![deny(missing_docs)]

mod heap;
mod value;

pub use heap::{
    CollectionReport, ExternalMemoryCharge, FrameToken, Heap, HeapConfig, HeapError, HeapStats,
    HostHandle, RootSnapshot, ScopeToken,
};
pub use value::{Value, ValueType};
