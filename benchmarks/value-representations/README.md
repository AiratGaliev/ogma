# OGM-17 — Value representation and SSA lowering proof

## Purpose

This M0 research spike compares two independent decisions: (1) Runtime Value layout: 16-byte explicit tagged value vs candidate 64-bit NaN-box word; (2) compiler SSA representation: boxed values at every SSA definition vs representation-aware values (f64, i32, references) with conversions at dynamic/ABI boundaries.

These axes must not be conflated. A compact runtime ABI does not imply that every intermediate SSA value should be boxed, and representation-aware SSA does not require NaN-boxing.

## Current result

The executable model in model.py checks a constrained set of ECMAScript Value semantics, demonstrates 16-byte vs 8-byte payload-size arithmetic, and counts structural boxing/unboxing/conversion/helper boundaries for six workload shapes. It is a proof model only: it does not run Rust, generate machine code, allocate native objects, or measure runtime throughput/register pressure.

The NaN-box prototype uses synthetic 48-bit handles for heap values. It does not establish pointer canonicalization, address-space safety, GC rooting, write barriers, or moving-GC compatibility. Those are mandatory proofs before adopting NaN-boxing.

## Workloads

- numeric arithmetic and mixed integer/f64 inputs;
- strings and dynamic conversion boundaries;
- object identity comparisons;
- mixed dynamic operations and type transitions;
- call/return-heavy flows;
- phi/block-argument and loop-carried value shapes.

The model reports per-candidate structural box/unbox/conversion/helper counts. The report explicitly lists metrics that cannot yet be measured because the repository has no Rust runtime/backend.

## Reproduce

From the repository root with Python 3.12+:

    python -m unittest discover -s benchmarks/value-representations -p "test_*.py" -v
    python benchmarks/value-representations/run.py

The runner writes benchmarks/value-representations/results/value-ssa-report.json.

## Semantic coverage and limitations

Fixtures cover NaN, signed zero, positive/negative infinity, BigInt, undefined/null, and object identity. BigInt-to-string conversion intentionally omits the source-language literal suffix n. Symbol conversion throws. Object coercion is outside this M0 subset.

A production implementation still needs a normative ECMAScript value model, full ToPrimitive/property semantics, GC integration, FFI/host ABI rules, stack maps, deoptimization strategy, and architecture-specific NaN-box feasibility checks.

## Recommendation

Keep the public/runtime Value ABI explicitly tagged and architecture-neutral until native runtime, GC, host boundary, and portability constraints are proven. Preserve representation-aware SSA as the compiler direction: retain proven primitive/reference representations internally and box only where dynamic semantics or an external ABI requires it. Do not select NaN-boxing based on its nominal 8-byte footprint alone.

This recommendation is architectural guidance from the current proof scope, not a claim that a future Rust implementation has already demonstrated better throughput or memory behavior.
