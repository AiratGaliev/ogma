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

The benchmark defaults to 2,048 objects, three warmup rounds and 101 measured samples for each of three workloads and two collector policies. Within each workload, the candidates run as paired samples with deterministic alternating order across warmups and measurements, reducing systematic first/second-candidate, scheduler and thermal bias. Each raw sample records graph identity, build time, collection pause, root-scan time, edges examined, roots scanned, live/reclaimed objects and logical heap bytes. Percentiles use nearest-rank. There are no machine-independent timing thresholds.

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


## Recorded result and M0 recommendation (2026-11-18)

The latest paired measurement report was produced by [CI run 44323273442](https://github.com/AiratGaliev/ogma/actions/runs/44323273442); the OGM-26 proof job and repository Fast CI both succeeded.

**Environment:** Rust 1.100.0; Linux x86_64; Intel Xeon 6975P-C.

**Configuration:** 2,048 objects; three warmup rounds; 101 paired samples per collector/workload; nearest-rank percentiles; alternating collector order per sample.

**Measured source revision:** `3d831df33948eb0b0b6b79c7d894e65d74c0ed34`.

**CI tested revision:** `75933f40dd2e7807b37103d0eb9ca1f2c88dad8e`.

Collection and root-scan values are microseconds; end-to-end is graph build + collection. RC maintenance operations are averaged over the 101 samples.

| Workload | Collector | Collection p50 / p95 / p99 (µs) | Root scan p50 (µs) | End-to-end p50 (µs) | Edges examined (avg.) | RC ops (avg.) | Live / reclaimed objects |
|---|---|---:|---:|---:|---:|---:|---:|
| Balanced roots and cycles | RC + cycle removal | 31.662 / 32.751 / 39.872 | 1.211 | 164.975 | 3,875 | 6,406 | 1,696 / 352 |
| Balanced roots and cycles | Non-moving mark-sweep | 17.546 / 18.074 / 27.060 | 1.216 | 150.522 | 1,754 | 0 | 1,696 / 352 |
| Persistent host retention | RC + cycle removal | 33.001 / 52.721 / 59.517 | 1.460 | 170.020 | 3,938 | 6,486 | 1,756 / 292 |
| Persistent host retention | Non-moving mark-sweep | 17.884 / 25.786 / 32.047 | 1.441 | 154.198 | 1,817 | 0 | 1,756 / 292 |
| Allocation churn | RC + cycle removal | 39.906 / 63.054 / 69.961 | 0.534 | 174.944 | 2,274 | 5,453 | 148 / 1,900 |
| Allocation churn | Non-moving mark-sweep | 28.040 / 42.507 / 47.839 | 0.547 | 159.592 | 153 | 0 | 148 / 1,900 |

### Recommendation

**Prefer non-moving tracing mark-sweep as the next GC candidate for Ogma Runtime integration.** In this model it reduced collection-pause p50 by 42.3–84.5% across the three workloads; p95 and p99 were also lower in every workload. Build + collection p50 improved by 9.6–10.3%. Paired graph fingerprints and final liveness/reclaimed counts matched exactly for both collectors.

The model also indicates the lifecycle trade-off: RC reclaims acyclic chains immediately when their last external reference is released, but pays reference-count maintenance on root/edge mutations and still needs a full trial-deletion/cycle pass. Mark-sweep avoids that per-edge/root reference-count traffic and traversed fewer edges in these generated graphs. Root-inventory scan costs were close between candidates.

This is an **M0 model-level recommendation, not a production GC lock**. Equal `peak_heap_bytes` values are not evidence of equal collector memory cost: the metric accounts for the common logical object records, payloads and edge slots, but excludes collector-specific scratch/side tables, Vec spare capacity, allocator metadata, root-registry storage and process overhead. The proof also does not model finalization, weak references, ephemerons, real ECMAScript object layouts, or compiler-generated native stack maps/safepoints. Those require real Runtime integration and separate GC/root stress evidence before the production choice is final.

The raw paired report for this run is available as the CI artifact attached to [run 44323273442](https://github.com/AiratGaliev/ogma/actions/runs/44323273442). A local copy of the exact JSON and gzip report is attached to this task response. The repository gzip copy should be refreshed from that artifact and verified against its SHA-256 before treating the raw-sample commit criterion as complete.
