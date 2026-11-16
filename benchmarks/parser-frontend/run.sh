#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

mkdir -p results

LOCKFILE="$ROOT/Cargo.lock"
LOCKFILE_SNAPSHOT="$ROOT/Cargo.lock.gz"
EXPECTED_LOCK_SHA256="73ffa651c29a26e0831f163b6c448ecb4a50e5af4d504173e1d177bafc0731c0"

if [[ ! -f "$LOCKFILE" ]]; then
    if [[ ! -f "$LOCKFILE_SNAPSHOT" ]]; then
        echo "::error::Pinned Cargo.lock snapshot is missing: $LOCKFILE_SNAPSHOT" >&2
        exit 1
    fi

    snapshot_tmp="$(mktemp "$ROOT/Cargo.lock.XXXXXX")"
    trap 'rm -f "$snapshot_tmp"' EXIT
    gzip -t "$LOCKFILE_SNAPSHOT"
    gzip -dc "$LOCKFILE_SNAPSHOT" > "$snapshot_tmp"
    mv "$snapshot_tmp" "$LOCKFILE"
    trap - EXIT
fi

actual_lock_sha256="$(sha256sum "$LOCKFILE" | awk '{print $1}')"
if [[ "$actual_lock_sha256" != "$EXPECTED_LOCK_SHA256" ]]; then
    echo "::error::Cargo.lock SHA-256 mismatch. Expected $EXPECTED_LOCK_SHA256, got $actual_lock_sha256." >&2
    exit 1
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
