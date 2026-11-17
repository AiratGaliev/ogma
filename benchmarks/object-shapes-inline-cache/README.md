# Object shapes and inline-cache proof (OGM-18)

This M0 spike demonstrates the correctness boundary and an initial cost model for shared object shapes, guarded monomorphic/polymorphic inline caches (ICs), and a megamorphic generic fallback. It is intentionally a standalone Python proof model: the repository has no Ogma Rust workspace, heap, interpreter, or native code generator yet. This directory does not add a production runtime subsystem.

## Shape contract

An ordinary object's immutable shape is interned by the ordered sequence of own-property layout records and the identity of its prototype. Shape IDs are unique across registries in the process, so guards from separate runtime/heap registries cannot alias. Each layout record contains the property key, data/accessor kind, and descriptor attributes. The per-object slot vector carries values or accessor pairs; a property value update does not change shape identity when the descriptor layout is unchanged.

Adding/removing a property, changing descriptor kind/attributes, or replacing the prototype transitions the object to an interned shape. Objects with the same ordered layout and the same prototype share a shape. Deletion compacts slots and creates/reuses the resulting canonical shape. Shape descriptors are immutable; an old shape remains safe to reuse by other objects.

## Inline-cache contract

Each IC is associated with one property key and one operation. A get IC caches only ordinary data-property lookups (own or inherited) and complete negative lookups. It stores the shape-identity guard chain up to the owner, plus the owner depth and slot. It loads the current slot value on every hit; it never caches a mutable property value. Negative-cache guards cover the complete ordinary prototype chain. A set IC caches own writable data-property slots and writes the current slot directly.

- **Uninitialized → monomorphic:** the first cacheable ordinary lookup installs one guarded case.
- **Monomorphic → polymorphic:** additional distinct shape/prototype guard chains install further cases, up to the configured capacity.
- **Polymorphic → megamorphic:** the next distinct cacheable case clears the local cases and permanently routes this site through the generic semantic helper.
- **Guard miss:** shape identity mismatch causes a generic fallback and may install another case. This is lazy invalidation: no mutable offset is trusted after a guard failure, and immutable old cases can remain available to other objects that still have the same shape.
- **Accessor/Proxy/unsupported chain:** always use the generic helper. Accessors are called with the original receiver. Proxy traps are invoked and the modeled non-configurable-property invariants are checked.

A change to a prototype object's property value with unchanged descriptor layout does not invalidate a data-slot plan: the IC reloads the value from the current owner slot. A change to the prototype object's shape (for example, data-to-accessor conversion), a prototype replacement, property addition or deletion changes a relevant guard and forces fallback before any fast access. Proxy operations never enter an ordinary-object cache case.

## Generic semantic fallback proof surface

The executable subset covers:

- own and inherited data-property reads;
- accessor getter/setter invocation with the original receiver;
- data assignment, including inherited writable data creating an own receiver property;
- strict/non-strict failures for non-writable properties and non-configurable deletes;
- property addition, descriptor reconfiguration, deletion and prototype replacement;
- Proxy `get`, `set` and `deleteProperty` traps, including the tested frozen-data, setter-less-accessor, non-configurable-delete and non-extensible-target invariants.

This is not a full ECMAScript object model. String property keys are supported by the proof; Symbol keys, arrays/exotic objects, private fields, `getOwnPropertyDescriptor`/`defineProperty` Proxy traps, revoked proxies, primitive boxing and complete Proxy prototype invariants are outside this M0 scope. The Proxy default paths are intentionally limited to the tested forwarding cases. These exclusions must not be mistaken for Ogma runtime compatibility claims.

## Reproduce

From the repository root with Python 3.12+ (standard library only):

```sh
python -m unittest discover -s benchmarks/object-shapes-inline-cache -p "test_*.py" -v
python benchmarks/object-shapes-inline-cache/run.py \
  --iterations 20000 --warmups 2 --rounds 7 --max-cases 4
```

The runner writes `benchmarks/object-shapes-inline-cache/results/object-shapes-ic-report.json` by default. CI writes its report to the runner temp directory and uploads it as the OGM-18 evidence artifact. For pull requests, `source_revision` is the topic branch HEAD and `tested_revision` is the GitHub PR merge-checkout SHA.

## Measurements and interpretation

The report includes the PR source revision and the exact checked-out revision separately; generic-versus-IC median/p95/max latency and operations/second for mono-, poly- and megamorphic reads and monomorphic own-data writes; IC hits/misses, guard failures, helper/fallback calls and Proxy/megamorphic fallback counts; shape-sharing/layout/slot counts; a Python `tracemalloc` comparison against equivalent plain dictionaries; and a recursive retained-metadata object-graph estimate using shallow `sys.getsizeof` per unique object.

**The Python throughput and memory figures are measurements of this proof harness only.** They are not Ogma engine throughput, Rust heap bytes, allocator/GC costs, native code size, physical register pressure or process RSS. There is no Ogma runtime to benchmark yet. Benchmark comparisons use identical objects and operation counts within each generic/IC pair, warmup and repeated measurement rounds. Semantic equivalence is checked before timing; the timed loops then execute the same operation over the same object population. Results must be interpreted together with the environment and configuration recorded in JSON.
