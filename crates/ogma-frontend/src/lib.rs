//! JavaScript source frontend boundary.
//!
//! Parsing, AST ownership, source spans and diagnostics are implemented in
//! OGM-41. Semantic analysis and Core IR lowering remain owned by the
//! compiler/Core IR boundaries; this crate does not depend on the Runtime.

#![deny(missing_docs)]
