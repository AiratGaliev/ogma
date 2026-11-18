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

The latest reproducible paired run is [CI run #340](https://github.com/AiratGaliev/ogma/actions/runs/44326700282). Its specialized OGM-26 job and repository `fast-checks` job both passed. The benchmarked source revision is `4a572f992bf522278b053335be38768254fb3109`; the CI-tested merge revision is `044beb35569eac2a9a3ed6738dc76b67b5f8bda8`.

Environment and sampling: Rust 1.100.0, Linux x86_64, AMD EPYC 7763 64-Core Processor; 2,048 objects, 3 warmups, 101 samples per workload and collector, nearest-rank percentiles, alternating candidate order. The report contains every raw sample.

The build timer stops after deterministic heap, edge and root construction. Graph fingerprinting is performed after that timer and is used only for equivalence validation. Root-scan time measures creation and validation of the common root inventory; RC-only per-slot root multiplicities are built inside RC collection and are not charged to the tracing collector.

All times below are microseconds. Collection shows p50/p95/p99; other latency columns show p50.

| Workload | Collector | Collection p50/p95/p99 | Root scan p50 | Build p50 | End-to-end p50 | Avg. edges examined | Avg. RC ops | Live / reclaimed |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| Balanced roots and cycles | RC + cycle removal | 46.057 / 53.361 / 55.905 | 0.932 | 67.517 | 114.556 | 3,875 | 6,406 | 1,696 / 352 |
| Balanced roots and cycles | Mark-sweep | 25.428 / 28.464 / 40.316 | 0.942 | 65.373 | 91.212 | 1,754 | 0 | 1,696 / 352 |
| Persistent host retention | RC + cycle removal | 45.016 / 57.519 / 65.434 | 1.142 | 65.985 | 111.039 | 3,938 | 6,486 | 1,756 / 292 |
| Persistent host retention | Mark-sweep | 24.015 / 26.320 / 35.376 | 1.122 | 65.033 | 89.018 | 1,817 | 0 | 1,756 / 292 |
| Allocation churn | RC + cycle removal | 60.744 / 68.910 / 71.805 | 0.370 | 73.208 | 135.124 | 2,274 | 5,453 | 148 / 1,900 |
| Allocation churn | Mark-sweep | 41.128 / 45.365 / 52.228 | 0.360 | 67.958 | 110.138 | 153 | 0 | 148 / 1,900 |

Mark-sweep reduced collection p50 by 44.8% in the balanced workload, 46.7% with persistent host handles and 32.3% under allocation churn. End-to-end p50 (build plus collection) improved by 20.4%, 19.8% and 18.5%, respectively. Its p95 and p99 collection pauses were also lower in each workload. Root-inventory scan cost was similar between candidates. Equivalent graph fingerprints and per-sample liveness/reclamation matched across all 101 pairs in all three workloads.

**M0 recommendation:** use non-moving tracing mark-sweep as the preferred next candidate for Runtime integration. This is a decision based on this shared synthetic model, not a final production GC selection. Production choice still requires the actual Runtime object layout, complete native root maps/safepoints, host-reference integration, and stress tests covering the real interpreter/native boundary.

Logical heap bytes are common-model object/edge accounting, not collector-specific memory consumption. They do not measure mark bitmaps, trial-deletion scratch arrays, root registry capacity, Vec spare capacity, allocator metadata, fragmentation or process RSS. This proof therefore makes no claim that the two production collectors would have equal memory overhead.

## Reproducible report artifact

The raw paired JSON is retained as a gzip file in this repository at `benchmarks/gc-comparison/results/ogm-26-gc-comparison-report.json.gz` and was uploaded as artifact `ogm-26-gc-comparison-evidence` in [CI run #340](https://github.com/AiratGaliev/ogma/actions/runs/44326700282) (artifact ID `14438170549`).

- Raw JSON SHA-256: `81cdf5e46b553d0043c055bff523a3543fcf880e6060a6e140cbd71a45da7d4f`
- Compressed gzip SHA-256: `31d3a9d977949c4a1ebf26205cce14d297ab26b63369a62eefd92c40beb67f03`

The validator cross-checks the reported percentiles, averages, throughput, heap/lifecycle summaries and root inventory against the complete raw samples, and verifies paired graph identity and liveness.
