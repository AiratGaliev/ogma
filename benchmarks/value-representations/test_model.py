from __future__ import annotations

import math
import unittest

from model import (
    JSValue, NANBOX_LAYOUT, NULL, Rep, TAGGED_LAYOUT, Tag, UNDEFINED,
    bigint, decode_nan_box, encode_nan_box, js_add, js_string, number,
    object_value, report, strict_equal, to_js_string, workload_cost,
)


class ValueSemanticsTests(unittest.TestCase):
    def test_undefined_and_null_have_distinct_tags(self):
        self.assertNotEqual(encode_nan_box(UNDEFINED), encode_nan_box(NULL))
        self.assertEqual(decode_nan_box(encode_nan_box(UNDEFINED)), UNDEFINED)
        self.assertEqual(decode_nan_box(encode_nan_box(NULL)), NULL)

    def test_nan_is_canonical_and_remains_nan(self):
        encoded = encode_nan_box(number(float("nan")))
        self.assertTrue(math.isnan(decode_nan_box(encoded).payload))
        self.assertEqual(encoded, encode_nan_box(number(float("nan"))))

    def test_signed_zero_is_preserved_in_number_payload(self):
        negative = decode_nan_box(encode_nan_box(number(-0.0))).payload
        positive = decode_nan_box(encode_nan_box(number(0.0))).payload
        self.assertEqual(negative, 0.0)
        self.assertEqual(positive, 0.0)
        self.assertLess(math.copysign(1.0, negative), 0)
        self.assertGreater(math.copysign(1.0, positive), 0)

    def test_infinities_round_trip(self):
        self.assertEqual(decode_nan_box(encode_nan_box(number(float("inf")))).payload, float("inf"))
        self.assertEqual(decode_nan_box(encode_nan_box(number(float("-inf")))).payload, float("-inf"))

    def test_bigint_string_conversion_has_no_suffix(self):
        self.assertEqual(to_js_string(bigint(12345678901234567890)), "12345678901234567890")
        result = js_add(bigint(7), js_string("px"))
        self.assertEqual(result, js_string("7px"))

    def test_bigint_plus_bigint_stays_bigint(self):
        self.assertEqual(js_add(bigint(4), bigint(5)), bigint(9))

    def test_nan_is_not_strictly_equal_to_itself(self):
        nan = number(float("nan"))
        self.assertFalse(strict_equal(nan, nan))

    def test_positive_and_negative_zero_are_strictly_equal(self):
        self.assertTrue(strict_equal(number(0.0), number(-0.0)))

    def test_object_identity_is_not_structural_equality(self):
        first = object_value(42)
        same = object_value(42)
        other = object_value(43)
        self.assertTrue(strict_equal(first, same))
        self.assertFalse(strict_equal(first, other))

    def test_layout_cost_is_explicitly_separate_from_ssa(self):
        sizes = report()["runtime_layouts"]
        self.assertEqual(TAGGED_LAYOUT.bytes, 16)
        self.assertEqual(NANBOX_LAYOUT.bytes, 8)
        self.assertEqual(sizes["nanbox-8"]["payload_bytes_saved_vs_tagged"], 800_000)

    def test_structural_ssa_comparison_is_not_one_numeric_microbenchmark(self):
        results = report()["workloads"]
        self.assertGreaterEqual(len(results), 5)
        self.assertIn("numeric", results)
        self.assertIn("string", results)
        self.assertIn("object_identity", results)
        self.assertIn("mixed_dynamic", results)
        self.assertIn("call_heavy", results)
        self.assertIn("phi_and_loops", results)
        self.assertNotEqual(
            workload_cost("mixed_dynamic", "boxed"),
            workload_cost("mixed_dynamic", "representation-aware"),
        )


if __name__ == "__main__":
    unittest.main()
