//! Portable OGMB v1 register-bytecode interpreter.
//!
//! The interpreter verifies serialized artifacts before dispatch and delegates
//! JavaScript value, object, closure and lexical environment semantics to the
//! shared Runtime. Its production dependency graph intentionally excludes the
//! parser, compiler and Core IR implementation crates.

#![deny(missing_docs)]

mod vm;

pub use vm::{
    CancellationToken, ExecutableFunctionInfo, ExecutableModule, ExecutionCompletion,
    ExecutionInterruption, ExecutionOptions, ExecutionReport, Host, HostCallCompletion,
    HostCallContext, HostCallRoot, HostError, InstructionLocation, InterpreterError, NoHost,
};

/// OGMB container version consumed by the interpreter.
pub const INPUT_FORMAT_VERSION: u16 = ogma_bytecode::FORMAT_VERSION;

#[cfg(test)]
mod tests {
    #[test]
    fn interpreter_uses_the_existing_portable_bytecode_version() {
        assert_eq!(super::INPUT_FORMAT_VERSION, 1);
    }
}
