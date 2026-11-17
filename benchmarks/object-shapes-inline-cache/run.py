"""Run the OGM-18 repeatable semantic/cost proof and emit a JSON report."""
from __future__ import annotations

import argparse
import json
import os
import platform
import statistics
import sys
import time
import tracemalloc
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from model import (  # noqa: E402
    InlineCache, JSObject, ShapeRegistry, define_data_property, get_property,
    set_property, shape_memory_summary,
)


def percentile(values: list[float], fraction: float) -> float:
    ordered = sorted(values)
    if not ordered:
        return 0.0
    index = min(len(ordered) - 1, max(0, round((len(ordered) - 1) * fraction)))
    return ordered[index]


def measure(operation, *, iterations: int, warmups: int, rounds: int) -> dict:
    for _ in range(warmups):
        operation(iterations)
    samples = []
    for _ in range(rounds):
        started = time.perf_counter_ns()
        operation(iterations)
        elapsed = time.perf_counter_ns() - started
        samples.append(elapsed / 1_000_000_000)
    median_seconds = statistics.median(samples)
    return {
        "iterations_per_round": iterations,
        "rounds": rounds,
        "warmup_rounds": warmups,
        "median_seconds": median_seconds,
        "p95_seconds": percentile(samples, 0.95),
        "max_seconds": max(samples),
        "median_operations_per_second": iterations / median_seconds if median_seconds else None,
        "samples_seconds": samples,
    }


def build_scenario(kind: str, count: int = 8):
    registry = ShapeRegistry()
    objects = []
    if kind == "monomorphic":
        for value in range(count):
            obj = JSObject(registry)
            define_data_property(obj, "value", value)
            define_data_property(obj, "stable", True)
            objects.append(obj)
    elif kind == "polymorphic":
        for value in range(count):
            obj = JSObject(registry)
            if value % 2:
                define_data_property(obj, "extra", value)
            define_data_property(obj, "value", value)
            define_data_property(obj, "stable", True)
            objects.append(obj)
    elif kind == "megamorphic":
        for value in range(count):
            obj = JSObject(registry)
            for index in range(value % 6):
                define_data_property(obj, f"field_{index}", index)
            define_data_property(obj, "value", value)
            objects.append(obj)
    else:
        raise ValueError(kind)
    return registry, objects


def correctness(scenarios: dict, max_cases: int) -> list[dict]:
    results = []
    for name, (_, objects) in scenarios.items():
        cache = InlineCache("value", max_cases=max_cases)
        generic = [get_property(obj, "value") for obj in objects]
        cached = [cache.get(obj) for obj in objects]
        if cached != generic:
            raise AssertionError(f"{name}: cached results differ from generic semantics")
        for obj in objects:
            expected = get_property(obj, "value")
            actual = cache.get(obj)
            if actual != expected:
                raise AssertionError(f"{name}: warm IC result differs for object {obj.identity}")
        results.append({
            "scenario": name,
            "objects": len(objects),
            "unique_shapes": len({obj.shape.identity for obj in objects}),
            "state_after_warmup": cache.state.value,
            "semantic_equivalence": True,
        })
    return results


def shallow_object_graph_size(root: object) -> int:
    """Count each retained metadata object's shallow sys.getsizeof once."""
    seen: set[int] = set()

    def visit(value: object) -> int:
        identity = id(value)
        if identity in seen:
            return 0
        seen.add(identity)
        total = sys.getsizeof(value)
        if isinstance(value, dict):
            total += sum(visit(key) + visit(item) for key, item in value.items())
        elif isinstance(value, (list, tuple, set, frozenset)):
            total += sum(visit(item) for item in value)
        else:
            attributes = getattr(value, "__dict__", None)
            if isinstance(attributes, dict):
                total += visit(attributes)
        return total

    return visit(root)


def profile_memory(objects_per_shape: int = 500) -> dict:
    registry = ShapeRegistry()
    tracemalloc.start()
    objects = []
    for index in range(objects_per_shape):
        obj = JSObject(registry)
        define_data_property(obj, "x", index)
        define_data_property(obj, "y", index + 1)
        objects.append(obj)
    model_current, model_peak = tracemalloc.get_traced_memory()
    model_summary = shape_memory_summary(registry, objects)
    tracemalloc.stop()

    tracemalloc.start()
    plain = [{"x": index, "y": index + 1} for index in range(objects_per_shape)]
    dict_current, dict_peak = tracemalloc.get_traced_memory()
    tracemalloc.stop()
    return {
        "objects": objects_per_shape,
        "shape_model_traced_current_bytes": model_current,
        "shape_model_traced_peak_bytes": model_peak,
        "plain_dict_traced_current_bytes": dict_current,
        "plain_dict_traced_peak_bytes": dict_peak,
        "shape_model_metadata": model_summary,
        "note": "Python tracemalloc comparison for this proof harness only; not Ogma heap or native memory overhead.",
        "keepalive_counts": {"shape_objects": len(objects), "plain_dicts": len(plain)},
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--iterations", type=int, default=20000)
    parser.add_argument("--warmups", type=int, default=2)
    parser.add_argument("--rounds", type=int, default=7)
    parser.add_argument(
        "--revision",
        default=os.environ.get("GITHUB_HEAD_SHA", os.environ.get("GITHUB_SHA", "local-uncommitted")),
        help="source revision (the PR head SHA in GitHub Actions)",
    )
    parser.add_argument(
        "--tested-revision",
        default=os.environ.get("GITHUB_SHA", "local-uncommitted"),
        help="checked-out revision used by the runner (GitHub's PR merge ref in Actions)",
    )
    parser.add_argument("--max-cases", type=int, default=4)
    parser.add_argument(
        "--output", type=Path,
        default=Path(__file__).resolve().parent / "results" / "object-shapes-ic-report.json",
    )
    args = parser.parse_args()
    if args.iterations < 1 or args.warmups < 0 or args.rounds < 1 or args.max_cases < 1:
        parser.error("iterations/rounds/max-cases must be positive and warmups non-negative")

    scenarios = {
        name: build_scenario(name)
        for name in ("monomorphic", "polymorphic", "megamorphic")
    }
    correctness_results = correctness(scenarios, args.max_cases)
    performance = {}
    counters = {}
    memory = {}

    for name, (registry, objects) in scenarios.items():
        cache = InlineCache("value", max_cases=args.max_cases)
        # Warm the IC outside timing so the samples measure the steady-state site.
        for obj in objects:
            cache.get(obj)

        cursor = [0]

        def generic_batch(iterations: int) -> None:
            for _ in range(iterations):
                obj = objects[cursor[0] % len(objects)]
                cursor[0] += 1
                get_property(obj, "value")

        def cached_batch(iterations: int) -> None:
            for _ in range(iterations):
                obj = objects[cursor[0] % len(objects)]
                cursor[0] += 1
                cache.get(obj)

        generic = measure(generic_batch, iterations=args.iterations,
                          warmups=args.warmups, rounds=args.rounds)
        cached = measure(cached_batch, iterations=args.iterations,
                         warmups=args.warmups, rounds=args.rounds)
        generic_rate = generic["median_operations_per_second"]
        cached_rate = cached["median_operations_per_second"]
        performance[name] = {
            "generic": generic,
            "inline_cache": cached,
            "speedup_ratio": cached_rate / generic_rate if generic_rate else None,
            "unique_shapes": len({obj.shape.identity for obj in objects}),
        }
        counters[name] = vars(cache.counters)
        memory[name] = {
            "objects": len(objects),
            "unique_shapes": registry.shape_count,
            "shape_property_layouts": sum(len(shape.properties) for shape in registry._shapes.values()),
            "object_slots": sum(len(obj.slots) for obj in objects),
            "ic_cases": cache.case_count,
            "warmed_ic_metadata_object_graph_shallow_bytes": shallow_object_graph_size(cache),
        }

    # Compare the write IC against the same generic [[Set]] helper on an identical monomorphic workload.
    write_objects = scenarios["monomorphic"][1]
    write_cache = InlineCache("value", operation="set", max_cases=args.max_cases)
    write_cache.set(write_objects[0], get_property(write_objects[0], "value"))
    write_cursor = [0]

    def generic_write_batch(iterations: int) -> None:
        for index in range(iterations):
            obj = write_objects[write_cursor[0] % len(write_objects)]
            write_cursor[0] += 1
            set_property(obj, "value", index)

    def cached_write_batch(iterations: int) -> None:
        for index in range(iterations):
            obj = write_objects[write_cursor[0] % len(write_objects)]
            write_cursor[0] += 1
            write_cache.set(obj, index)

    generic_write = measure(generic_write_batch, iterations=args.iterations,
                            warmups=args.warmups, rounds=args.rounds)
    cached_write = measure(cached_write_batch, iterations=args.iterations,
                           warmups=args.warmups, rounds=args.rounds)
    set_performance = {
        "generic": generic_write,
        "inline_cache": cached_write,
        "speedup_ratio": (
            cached_write["median_operations_per_second"] / generic_write["median_operations_per_second"]
            if generic_write["median_operations_per_second"] else None
        ),
    }

    payload = {
        "schema": "ogm-18-object-shapes-ic-proof-v1",
        "task": "OGM-18",
        "source_revision": args.revision,
        "tested_revision": args.tested_revision,
        "generated_at_utc": datetime.now(timezone.utc).isoformat(),
        "environment": {
            "python": sys.version,
            "implementation": platform.python_implementation(),
            "platform": platform.platform(),
            "machine": platform.machine(),
            "processor": platform.processor(),
        },
        "configuration": {
            "iterations": args.iterations,
            "warmup_rounds": args.warmups,
            "measurement_rounds": args.rounds,
            "max_polymorphic_cases": args.max_cases,
        },
        "correctness": correctness_results,
        "performance": performance,
        "set_performance": set_performance,
        "set_ic_counters": vars(write_cache.counters),
        "ic_counters": counters,
        "logical_memory": memory,
        "memory_profile": profile_memory(),
        "limitations": [
            "This is a Python proof harness, not the Ogma runtime or a native implementation.",
            "Throughput and tracemalloc figures characterize only this harness and machine.",
            "The semantic subset is limited to ordinary objects with string keys and selected Proxy invariants.",
            "Symbols, arrays/exotic objects, private fields, full Reflect semantics and Proxy descriptor traps are not modeled.",
            "No native object-code size, register pressure, allocator/GC cost, process RSS or native throughput is measured.",
        ],
    }
    output = args.output
    if output is not None:
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps(payload, indent=2, sort_keys=True))
    if output:
        print(f"wrote {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
