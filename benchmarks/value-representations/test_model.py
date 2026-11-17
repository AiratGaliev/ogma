from __future__ import annotations

import math
import unittest

from corpus import load_corpus
from model import (
    CANONICAL_NAN,
    HeapHandles,
    NaNBoxCodec,
    NAN_TAGS,
    NULL,
    Tag,
    UNDEFINED,
    analyze_corpus,
    bigint,
    js_add,
    js_string,
    number,
    object_value,
    strict_equal,
    symbol_value,
    to_js_string,
)


class ValueSemanticsTests(unittest.TestCase):
    def setUp(self):
        self.codec = NaNBoxCodec()

    def test_undefined_and_null_round_trip_with_distinct_tags(self):
        self.assertNotEqual(self.codec.encode(UNDEFINED), self.codec.encode(NULL))
        self.assertEqual(self.codec.decode(self.codec.encode(UNDEFINED)), UNDEFINED)
        self.assertEqual(self.codec.decode(self.codec.encode(NULL)), NULL)

    def test_nan_is_canonical_and_round_trips(self):
        word = self.codec.encode(number(float("nan")))
        self.assertEqual(word, CANONICAL_NAN)
        self.assertTrue(math.isnan(self.codec.decode(word).payload))

    def test_signed_zero_is_preserved(self):
        negative = self.codec.decode(self.codec.encode(number(-0.0))).payload
        positive = self.codec.decode(self.codec.encode(number(0.0))).payload
        self.assertEqual(negative, 0.0)
        self.assertEqual(positive, 0.0)
        self.assertLess(math.copysign(1.0, negative), 0)
        self.assertGreater(math.copysign(1.0, positive), 0)

    def test_positive_and_negative_infinity_round_trip(self):
        self.assertEqual(self.codec.decode(self.codec.encode(number(float("inf")))).payload, float("inf"))
        self.assertEqual(self.codec.decode(self.codec.encode(number(float("-inf")))).payload, float("-inf"))

    def test_heap_values_round_trip_without_truncation_or_hashing(self):
        values = (
            js_string("stable"),
            bigint(2**120 + 19),
            object_value(2**80 + 7),
            symbol_value(2**80 + 9),
        )
        for value in values:
            self.assertEqual(self.codec.decode(self.codec.encode(value)), value)

    def test_heap_handles_are_stable_for_equal_values(self):
        value = js_string("same")
        self.assertEqual(self.codec.encode(value), self.codec.encode(value))

    def test_heap_handle_exhaustion_is_detected(self):
        codec = NaNBoxCodec(HeapHandles(max_handle=1))
        codec.encode(js_string("first"))
        with self.assertRaises(OverflowError):
            codec.encode(js_string("second"))

    def test_malformed_words_are_rejected(self):
        bad_boolean = (NAN_TAGS[Tag.BOOLEAN] << 48) | 2
        with self.assertRaises(ValueError):
            self.codec.decode(bad_boolean)
        with self.assertRaises(ValueError):
            self.codec.decode(0x7FF9000000000001)

    def test_bigint_string_and_addition_subset(self):
        self.assertEqual(to_js_string(bigint(12345678901234567890)), "12345678901234567890")
        self.assertEqual(js_add(bigint(7), js_string("px")), js_string("7px"))
        self.assertEqual(js_add(bigint(4), bigint(5)), bigint(9))

    def test_bigint_and_number_addition_throws_in_both_orders(self):
        with self.assertRaises(TypeError):
            js_add(bigint(4), number(5))
        with self.assertRaises(TypeError):
            js_add(number(5), bigint(4))

    def test_nan_is_not_strictly_equal_to_itself(self):
        nan = number(float("nan"))
        self.assertFalse(strict_equal(nan, nan))

    def test_positive_and_negative_zero_are_strictly_equal(self):
        self.assertTrue(strict_equal(number(0.0), number(-0.0)))

    def test_object_and_symbol_identity(self):
        self.assertTrue(strict_equal(object_value(3), object_value(3)))
        self.assertFalse(strict_equal(object_value(3), object_value(4)))
        self.assertTrue(strict_equal(symbol_value(8), symbol_value(8)))
        self.assertFalse(strict_equal(symbol_value(8), symbol_value(9)))


class CoreIRDrivenComparisonTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.corpus = load_corpus()
        cls.report = analyze_corpus(cls.corpus)

    def test_candidates_use_identical_canonical_corpus(self):
        candidates = self.report["candidate_matrix"]
        self.assertEqual(len(candidates), 4)
        self.assertEqual(
            {item["corpus_sha256"] for item in candidates},
            {self.corpus.corpus_sha256},
        )
        self.assertEqual(
            [item["path"] for item in self.report["corpus"]["fixtures"]],
            [item.path for item in self.corpus.fixtures],
        )

    def test_actual_ir_fixtures_cover_required_workload_shapes(self):
        coverage = self.report["corpus"]["workload_coverage"]
        for label in (
            "numeric", "string", "object_identity", "mixed_dynamic",
            "call_heavy", "phi_block_arguments", "loops",
        ):
            self.assertTrue(coverage[label], f"missing Core IR coverage for {label}")
        self.assertIn(
            "tests/core-ir/fixtures/positive-call-boundary.json",
            coverage["call_heavy"],
        )
        self.assertIn(
            "tests/core-ir/fixtures/positive-loop-carried-values.json",
            coverage["loops"],
        )

    def test_boxed_and_representation_aware_boundaries_differ(self):
        candidates = {item["candidate_id"]: item for item in self.report["candidate_matrix"]}
        boxed = candidates["tagged-16+boxed-ssa"]["aggregate"]
        aware = candidates["tagged-16+representation-aware-ssa"]["aggregate"]
        self.assertEqual(boxed["representation_conversion_count"], 0)
        self.assertGreater(aware["box_conversion_count"], 0)
        self.assertGreater(aware["unbox_conversion_count"], 0)
        self.assertGreater(aware["helper_call_sites"], 0)
        self.assertLess(aware["helper_call_sites"], boxed["helper_call_sites"])
        self.assertLess(
            candidates["tagged-16+representation-aware-ssa"]["aggregate"]["logical_payload_estimate_bytes"],
            candidates["tagged-16+boxed-ssa"]["aggregate"]["logical_payload_estimate_bytes"],
        )

    def test_phi_edge_arguments_and_loop_back_edges_are_counted(self):
        aggregate = self.report["candidate_matrix"][0]["aggregate"]
        self.assertGreater(aggregate["block_value_parameter_count"], 0)
        self.assertGreater(aggregate["phi_value_edge_argument_count"], 0)
        self.assertGreater(aggregate["loop_back_edge_count"], 0)

    def test_local_parameter_return_phi_and_loop_boundaries_are_explicit(self):
        candidates = {item["candidate_id"]: item for item in self.report["candidate_matrix"]}
        aware = candidates["tagged-16+representation-aware-ssa"]
        loop = next(
            fixture for fixture in aware["per_fixture"]
            if fixture["fixture"] == "tests/core-ir/fixtures/positive-loop-carried-values.json"
        )
        breakdown = loop["conversion_breakdown"]
        self.assertEqual(breakdown["function_parameters"]["unbox"], 3)
        self.assertEqual(breakdown["returns"]["box"], 1)
        self.assertGreater(breakdown["dynamic_semantic_operands"]["box"], 0)
        self.assertEqual(breakdown["phi_block_arguments"]["box"], 0)
        self.assertEqual(breakdown["phi_block_arguments"]["unbox"], 0)
        self.assertEqual(breakdown["loop_carried_edges"]["box"], 0)
        self.assertEqual(breakdown["loop_carried_edges"]["unbox"], 0)
        self.assertEqual(breakdown["loop_carried_edges"]["back_edge_count"], 1)
        self.assertEqual(breakdown["lexical_environment_locals"]["box"], 0)
        self.assertEqual(breakdown["lexical_environment_locals"]["unbox"], 0)

        closure = next(
            fixture for fixture in aware["per_fixture"]
            if fixture["fixture"] == "tests/core-ir/fixtures/positive-closure-source.json"
        )
        closure_breakdown = closure["conversion_breakdown"]["lexical_environment_locals"]
        self.assertGreater(closure_breakdown["slot_count"], 0)
        self.assertIn("env.load", closure["operation_counts"])
        self.assertIn("env.store", closure["operation_counts"])
        self.assertEqual(closure_breakdown["box"], 0)
        self.assertEqual(closure_breakdown["unbox"], 0)

    def test_liveness_is_nonzero_and_reported_as_abstract_payload(self):
        for candidate in self.report["candidate_matrix"]:
            self.assertGreater(candidate["aggregate"]["peak_live_ssa_values"], 0)
            self.assertGreater(candidate["aggregate"]["peak_live_ssa_payload_bytes"], 0)

    def test_native_metrics_are_explicitly_not_measured(self):
        missing = self.report["not_measured"]
        self.assertIn("native_object_code_size_bytes", missing)
        self.assertIn("native_throughput", missing)
        self.assertIn("physical_register_pressure_and_spills", missing)
        self.assertIn("allocator_gc_costs_and_peak_rss", missing)
        self.assertIn("observable_candidate_differential_execution", missing)


if __name__ == "__main__":
    unittest.main()
