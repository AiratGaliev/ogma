//! The single user-facing Ogma M1 executable.

fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(ogma_cli::run_cli(std::env::args().skip(1).collect()))
}
