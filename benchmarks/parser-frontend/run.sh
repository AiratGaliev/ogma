#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

mkdir -p results

if [[ ! -f Cargo.lock ]]; then
    cargo generate-lockfile --manifest-path "$ROOT/Cargo.toml"
fi

cargo fmt --manifest-path "$ROOT/Cargo.toml" --all -- --check
cargo test --manifest-path "$ROOT/Cargo.toml" --workspace --locked
cargo clippy --manifest-path "$ROOT/Cargo.toml" --workspace --all-targets --locked -- -D warnings

BUILD_STARTED=$SECONDS
cargo build --manifest-path "$ROOT/Cargo.toml" --workspace --release --locked
export OGM14_BUILD_SECONDS=$((SECONDS - BUILD_STARTED))

"$ROOT/target/release/ogma-parser-bench-oxc" parse > "$ROOT/results/oxc-parse.json"
"$ROOT/target/release/ogma-parser-bench-oxc" full > "$ROOT/results/oxc-full.json"
"$ROOT/target/release/ogma-parser-bench-oxc" diagnostics > "$ROOT/results/oxc-diagnostics.json"
"$ROOT/target/release/ogma-parser-bench-swc" parse > "$ROOT/results/swc-parse.json"
"$ROOT/target/release/ogma-parser-bench-swc" diagnostics > "$ROOT/results/swc-diagnostics.json"

python3 "$ROOT/collect_results.py"
python3 "$ROOT/validate_results.py"
