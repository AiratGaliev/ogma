# OGM-26 — RC cycle removal versus non-moving tracing mark-sweep

## Purpose and boundary

This is an isolated M0 architecture proof. It compares two GC policies over the same Rust-native, non-moving, generation-checked object-slot model. It is not an ECMAScript heap, production allocator, interpreter, native AOT runtime, or a production GC implementation.

Both candidates use the same object records, slot reuse rules, payload sizes, edge graph, root inventory and deterministic workload generator. Only the collection policy differs:

- **RC + cycle removal:** object-edge and root mutations maintain incoming reference counts. Releasing the last reference recursively reclaims acyclic objects. A collection performs trial deletion: subtract all internal graph edges from the reference counts; the remaining external counts must exactly equal the inventoried roots; then mark from nodes with remaining external references and reclaim the unreachable subgraph, including cycles.
- **Non-moving tracing mark-sweep:** collection materializes the same root inventory, marks from each root through object edges, then sweeps unmarked slots. Object slots are not relocated; reclaiming a slot advances its generation.

Ordinary reference-counted Rust pointer graphs are intentionally not used for the cyclic object graph. The harness has no external crate dependencies.

## Root and handle contract

The root inventory is explicit and separate from internal object references:

| Root provider | Proof representation | Lifetime |
|---|---|---|
| Interpreter frames | Interpreter frame roots | Frame open/close |
| Native frame prototype roots | Native prototype frame roots | Explicit stand-in for a future native root map/shadow frame |
| Persistent external host handles | HostHandle registry slots | Host retain/release |
| Temporary scopes | Strictly LIFO ScopeToken stack | Scope open/close |

GcRef is an internal heap slot + generation reference. HostHandle is a different external token with its own slot + generation. The host-handle registry roots its target while retained; releasing it invalidates the host token and removes that root. A reclaimed/reused object slot advances its generation, so stale internal references cannot resolve to the replacement object.

Native frame roots are deliberately a prototype root provider only. This proof does not claim compiler-emitted AOT stack maps, native safepoints, register spill correctness or interpreter/native transitions; those are covered by the separate native-frame integration proof.

## Workloads and equivalence

The runner uses identical deterministic graph construction for both candidates:

- fixed-size cyclic components with cross-component edges;
- reachable and unreachable subgraphs;
- interpreter, native-prototype, persistent-host and temporary-scope roots;
- a host-retention-heavy case;
- an allocation-churn case that closes temporary scopes before collection while retaining selected host/frame roots.

Before collection, the runner compares a deterministic graph fingerprint and exact root inventory for both strategies. It also compares post-collection live object counts, total reclaimed object counts and logical live bytes. A mismatch is a hard benchmark failure; the report is not written as a successful result.

The benchmark defaults to 2,048 objects, three warmup rounds and 101 measured samples for each of three workloads and two collector policies. Each raw sample records graph identity, build time, collection pause, root-scan time, edges examined, roots scanned, live/reclaimed objects and logical heap bytes. Percentiles use nearest-rank. There are no machine-independent timing thresholds.

## Metrics

- collection/pause latency p50/p95/p99;
- root inventory scan p50/p95/p99;
- graph build latency and objects/second;
- end-to-end build + collection latency and graph-operations/second;
- roots and edges scanned;
- peak logical heap bytes and post-collection live logical heap bytes;
- reclaimed objects during the collection and cumulative reclaimed objects;
- RC reference-count operations.

The root-scan measurement is separately exposed from the total collection pause. RC also pays reference-count maintenance on every edge/root mutation; its operation counter and lifecycle behavior are part of the comparison rather than hidden as collector work. RC may reclaim acyclic structures before the explicit cycle pass, so the report compares cumulative reclamation and final liveness, not only the number swept by the final pass.

Logical heap bytes are an intentionally bounded proxy: fixed Rust object-header size + requested payload bytes + logical edge slots. It excludes Vec spare capacity, allocator metadata, root registries, collector side tables and process overhead. It is not RSS or an allocator fragmentation measurement.

## Reproduction

With Rust 1.100.0 from the repository root:

    cargo fmt --manifest-path benchmarks/gc-comparison/Cargo.toml --all -- --check
    cargo clippy --manifest-path benchmarks/gc-comparison/Cargo.toml --all-targets --all-features --locked -- -D warnings
    cargo test --manifest-path benchmarks/gc-comparison/Cargo.toml --all-features --locked
    cargo run --manifest-path benchmarks/gc-comparison/Cargo.toml --locked --release --bin ogma-gc-comparison-bench -- --objects 2048 --warmups 3 --samples 101 --revision <source-sha> --tested-revision <tested-sha> --cpu "<cpu model>" --output ogm-26-gc-comparison-report.json
    python3 .github/scripts/validate_gc_comparison.py ogm-26-gc-comparison-report.json

The ogm26 topic-PR CI job executes formatting, Clippy, unit tests, the release benchmark and the machine-readable report validator. It uploads the raw JSON artifact. The benchmark output is intentionally produced on the CI host so it records the actual toolchain, platform and CPU metadata.

## Correctness coverage

Unit tests exercise:
- reachable cycles and unreachable cycles for both candidates;
- each of the four root providers retaining and then releasing its object;
- strict LIFO temporary scopes and frame roots;
- host-handle release and stale-generation rejection;
- object-slot reuse with a bumped generation;
- RC trial-deletion accounting and its equality with the explicit root inventory;
- immediate RC reclamation of acyclic chains after the last external reference is released.

## Interpretation and decision policy

This proof can select an initial algorithmic direction only for this model and workload matrix. It does not establish JavaScript/ECMAScript semantics, finalization behavior, weak references, ephemerons, concurrent or incremental collection, generational collection, AOT stack maps or production runtime latency. Those gaps must remain explicit in any M0 decision.

No production GC choice is declared by this source alone. The recommendation must be based on the attached machine-readable CI report, the lifecycle trade-offs recorded here, and the remaining root protocol requirements. If measurements are noisy or inconclusive, keep the decision open rather than inventing a winner.
