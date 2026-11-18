# OGM-23 — FrameSnapshot and batch host API proof

## Purpose and measurement boundary

This is a standalone M0 benchmark proof for the frame-snapshot and bulk-host-API requirements in architecture specification sections 8.9.4 and 8.9.7. It is **not** a production Ogma API, Runtime implementation, or performance claim about Ogma.

At this repository revision there is no executable Ogma JavaScript Runtime or production application host adapter. The JavaScript consumer workload therefore runs on the pinned reference binding **rquickjs 0.14.0**, matching the dependency version and default + std feature profile of the inspected CGE Experiments revision:

- CGE repository: https://github.com/AiratGaliev/cge-experiments
- Reference revision: c47ad08c3da9a3857f8429cebbed6ce88969715d
- Reference host API: [pinned CGE scripting.rs](https://github.com/AiratGaliev/cge-experiments/blob/c47ad08c3da9a3857f8429cebbed6ce88969715d/rust-brick-breaker/src/scripting.rs)
- Existing consumer benchmark: [pinned CGE runtime benchmark](https://github.com/AiratGaliev/cge-experiments/blob/c47ad08c3da9a3857f8429cebbed6ce88969715d/rust-brick-breaker/benches/runtime.rs)
- Existing host physics/ECS measurements: [pinned CGE BENCHMARKS.md](https://github.com/AiratGaliev/cge-experiments/blob/c47ad08c3da9a3857f8429cebbed6ce88969715d/rust-brick-breaker/BENCHMARKS.md)

The report labels the actual JS measurements as a **reference-consumer rquickjs workload** and reports ogma_runtime_dispatch = not_measured. Do not use these timings as Ogma Runtime throughput or as an Ogma-vs-rquickjs comparison.

## Compared consumer paths

The pinned workload preserves the CGE-style synchronous update and compares equivalent checksums for each path:

| Concern | Scalar/reference path | Batch path |
|---|---|---|
| Keyboard/gamepad input | String names and set lookup | Numeric IDs backed by bitsets |
| Transform reads | X and Y callbacks per entity | One callback for a typed ID array and reusable output array |
| Collision reads | Count plus two callbacks per pair | One bulk collision read |
| Typed-array writes | Per-element TypedArray.set | One validated contiguous backing-store write |
| JS execution baseline | N/A | JS-only iteration over already populated arrays |

JS functions are cached as Persistent<Function>, and caller-owned Uint32Array, Float32Array and collision output arrays are created before timed iterations. All modes use the same frame, iterate the same logical values and validate equal checksums before timing. The update verifies that the published frame ID does not change during one synchronous execution.

The bulk path validates the typed-array class, exact element count, attachment and alignment, and rejects overlapping input/output backing ranges. Its unsafe slice access is limited to the synchronous host callback; it does not invoke or re-enter JavaScript while either slice is alive. The implementation follows the rquickjs 0.14.0 safety contract: a JS-owned typed-array slice must not outlive the callback or any subsequent JS execution.

## Pinned CGE consumer workload

In addition to the extended collision/input workload above, the proof reproduces the pinned `quickjs_host_api/frame_submit_and_update` operation from `rust-brick-breaker/benches/runtime.rs` at its original entity counts (16, 64 and 256): a reusable Float32Array, position summation, eight repeated `engine.input.isDown("A")` reads and `deltaSeconds()`. Its variants compare scalar X/Y callbacks, the pinned Array-based `readPositions()` API (including its per-element `TypedArray.set` loop), numeric input IDs, and the checked contiguous bulk path. The extended consumer also includes the CGE gamepad connection/analog fields (`connected`, `leftStickX`) in addition to digital buttons.

The extended collision/input workload (40/256/1024 entities) remains a separate family; do not combine its timings with the pinned CGE host-API cases.

Snapshot construction and `publish_snapshot()` handoff are measured as separate phases. The handoff correctness test publishes a different frame identity, delta, input, gamepad state, transforms and collision set, then verifies that the next JS execution observes that complete frame. The pinned CGE implementation's existing `update_frame()` buffer swaps and `Persistent` hook caching remain unchanged reference baselines.

The physics active-pose/deactivation cache and sparse/dense ECS phases in this crate are deliberately synthetic algorithm models. They isolate host work from JS dispatch; they do not replace the actual Jolt/Flecs measurements already recorded at the pinned revision in CGE's `rust-brick-breaker/BENCHMARKS.md`. That file is the source for the reference host-runtime measurements. No Jolt/Flecs timing is attributed to this proof's synthetic cases.

## Separate phase measurements

The JSON report separates the following phases:

- frame_snapshot_construction: allocate-and-copy a new snapshot versus refresh of reusable HashMap/Vec storage.
- frame_snapshot_handoff: publishing prebuilt frame snapshots into Runtime-owned storage through the host handoff mutex.
- reference_consumer_js_update_and_dispatch: extended scalar/string, numeric batch with per-element writes, and numeric batch with bulk writes through rquickjs host callbacks.
- cge_reference_consumer_js_update_and_dispatch: the pinned CGE JavaScript host API at 16/64/256 entities, including scalar callbacks, Array-based readPositions(), numeric input IDs and typed-ID bulk writes.
- cge_javascript_update_only: the pinned CGE workload's JS-only iteration over an already populated positions array.
- javascript_update_only: extended JS iteration over preloaded input/transform/collision arrays without host callbacks.
- host_physics_active_pose_cache_model: synthetic active-pose updates plus final deactivation pose and coalescing, independent of the script bridge.
- host_ecs_sync_model: synthetic sparse changed-entity updates, dense transform traversal and the CGE consumer's 50% adaptive selector.
- command_drain_reuse: clearing the caller vector and swapping prebuilt command storage.

The extended consumer and synthetic host-model sizes are 40, 256 and 1024. The pinned CGE JavaScript host-API cases separately use 16, 64 and 256 entities. Physics/ECS host models cover both sparse changes (two active entities plus a deactivation sample) and dense changes (all entities). These models deliberately do **not** link Jolt or Flecs and are not measurements of their implementations. Jolt stepping, active_body_poses_into, Flecs query execution, graphics and audio are outside this Ogma proof.

Snapshot construction is measured separately from script execution. The existing CGE Persistent lifecycle-hook caching and update_frame() frame-buffer mem::swap remain the reference baseline; this task does not rewrite them or claim a separate improvement for them.

## Metrics and interpretation

Each case records nearest-rank p50/p95/p99, minimum and maximum per-sample mean latency, warmups, sample count, iterations, logical item count, entity/change counts and host callback count. The benchmark uses a Rust global allocator counter for allocations and allocated bytes during the timed samples. **QuickJS allocations through its C allocator are not included**, so these columns are not total-process or JS-heap allocation counts.

There are no hard-coded performance acceptance thresholds in a generic Runtime. Results are machine- and workload-specific. Compare only reports with the same tested revision, toolchain, operating environment, workload and settings. The proof is used to evaluate relative consumer paths and contracts, not to claim universal wins.

The command-drain fixture uses representative queued command variants with owned text payloads (CreateText and SetText), rather than only a trivial Copy opcode, so the timed caller-buffer clear includes dropping previous output payloads.

## Reproduction

The proof uses the Rust version pinned by the Ogma Fast CI job:

    cargo fmt --manifest-path benchmarks/frame-snapshot/Cargo.toml --all -- --check
    cargo clippy --manifest-path benchmarks/frame-snapshot/Cargo.toml --all-targets --all-features --locked -- -D warnings
    cargo test --manifest-path benchmarks/frame-snapshot/Cargo.toml --all-features --locked
    cargo run --manifest-path benchmarks/frame-snapshot/Cargo.toml --locked --release --bin ogma-frame-snapshot-bench -- --iterations 1000 --warmups 3 --samples 101 --revision <source-revision> --tested-revision <tested-revision> --cpu <cpu-model> --output ogm-23-frame-snapshot-report.json

The CI report is uploaded as ogm-23-frame-snapshot-host-api-evidence.

## Scope limits

- No production Ogma Runtime dispatch or native ABI execution is measured.
- The JavaScript reference measurements use rquickjs 0.14.0 with the pinned CGE-compatible feature profile.
- This proof's physics and ECS measurements are synthetic host-side algorithm models, not Jolt/Flecs benchmarks. Actual reference host-runtime measurements are documented in the pinned CGE benchmark notes.
- Rust allocator metrics exclude QuickJS's C allocator.
- There is no browser/gameplay acceptance in this benchmark proof. Any future production adapter must independently preserve frame identity, ownership and lifetime rules, typed-array validation, buffer invalidation and synchronous no-re-entry constraints.
