//! Minimal command-line orchestration for the production Ogma M1 compiler and Runtime.

mod artifact;
mod cli;
mod compiler;
mod module_source;
mod runner;

/// Runs the `ogma` command and returns its stable process exit code.
pub fn run_cli(args: Vec<String>) -> u8 {
    cli::run(args)
}
