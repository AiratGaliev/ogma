from __future__ import annotations

import unittest

from model import (
    CacheState, InlineCache, JSObject, JSProxy, JSRuntimeError, ShapeRegistry,
    UNDEFINED, define_accessor_property, define_data_property, delete_property,
    get_property, same_value, set_property,
)


class ShapeIdentityTests(unittest.TestCase):
    def setUp(self):
        self.registry = ShapeRegistry()

    def make(self, **values):
        obj = JSObject(self.registry)
        for key, value in values.items():
            define_data_property(obj, key, value)
        return obj

    def test_equal_ordered_layouts_share_shape(self):
        a = self.make(x=1, y=2)
        b = self.make(x=10, y=20)
        self.assertIs(a.shape, b.shape)
        self.assertEqual(a.slots, [1, 2])
        self.assertEqual(b.slots, [10, 20])

    def test_property_order_is_part_of_shape_identity(self):
        a = self.make(x=1, y=2)
        b = self.make(y=2, x=1)
        self.assertNotEqual(a.shape.identity, b.shape.identity)

    def test_value_update_does_not_change_shape(self):
        obj = self.make(x=1)
        shape = obj.shape
        self.assertTrue(set_property(obj, "x", 2))
        self.assertIs(obj.shape, shape)
        self.assertEqual(get_property(obj, "x"), 2)

    def test_add_delete_are_explicit_transitions(self):
        obj = self.make(x=1)
        initial = obj.shape.identity
        define_data_property(obj, "y", 2)
        added = obj.shape.identity
        self.assertNotEqual(initial, added)
        self.assertTrue(delete_property(obj, "x"))
        self.assertNotEqual(added, obj.shape.identity)
        reasons = {entry[2] for entry in self.registry.transitions}
        self.assertIn("add-data", reasons)
        self.assertIn("delete", reasons)

    def test_prototype_identity_participates_in_shape(self):
        proto_a = self.make(p=1)
        proto_b = self.make(p=1)
        a = JSObject(self.registry, proto_a)
        b = JSObject(self.registry, proto_b)
        self.assertNotEqual(a.shape.identity, b.shape.identity)

    def test_descriptor_reconfiguration_changes_shape(self):
        obj = self.make(x=1)
        old = obj.shape
        self.assertTrue(define_data_property(obj, "x", 1, writable=False))
        self.assertNotEqual(old.identity, obj.shape.identity)

    def test_non_configurable_property_cannot_be_redefined(self):
        obj = JSObject(self.registry)
        define_data_property(obj, "x", 1, configurable=False)
        with self.assertRaisesRegex(JSRuntimeError, "non-configurable"):
            define_data_property(obj, "x", 2, configurable=True)

    def test_non_extensible_object_cannot_gain_new_property(self):
        obj = JSObject(self.registry)
        obj.extensible = False
        self.assertFalse(define_data_property(obj, "x", 1))
        with self.assertRaises(JSRuntimeError):
            define_data_property(obj, "x", 1, strict=True)


class InlineCacheTests(unittest.TestCase):
    def setUp(self):
        self.registry = ShapeRegistry()

    def make(self, value):
        obj = JSObject(self.registry)
        define_data_property(obj, "x", value)
        return obj

    def test_monomorphic_cache_hits_across_shared_shape_instances(self):
        first, second = self.make(1), self.make(2)
        cache = InlineCache("x")
        self.assertEqual(cache.get(first), 1)   # generic warm-up
        self.assertEqual(cache.get(second), 2)  # same shape guard
        self.assertEqual(cache.state, CacheState.MONOMORPHIC)
        self.assertEqual(cache.counters.ic_hits, 1)

    def test_cached_slot_loads_current_value_not_stale_value(self):
        obj = self.make(1)
        cache = InlineCache("x")
        self.assertEqual(cache.get(obj), 1)
        set_property(obj, "x", 99)
        self.assertEqual(cache.get(obj), 99)

    def test_polymorphic_cache_accepts_distinct_shapes(self):
        a = self.make(1)
        b = JSObject(self.registry)
        define_data_property(b, "other", 0)
        define_data_property(b, "x", 2)
        cache = InlineCache("x", max_cases=2)
        self.assertEqual(cache.get(a), 1)
        self.assertEqual(cache.get(b), 2)
        self.assertEqual(cache.state, CacheState.POLYMORPHIC)
        self.assertEqual(cache.get(a), 1)
        self.assertEqual(cache.counters.ic_hits, 1)

    def test_negative_lookup_is_invalidated_by_property_addition(self):
        obj = JSObject(self.registry)
        cache = InlineCache("x")
        self.assertIs(cache.get(obj), UNDEFINED)
        self.assertTrue(cache.get(obj) is UNDEFINED)
        self.assertEqual(cache.counters.ic_hits, 1)
        define_data_property(obj, "x", 7)
        self.assertEqual(cache.get(obj), 7)
        self.assertEqual(cache.counters.guard_failures, 1)

    def test_delete_invalidates_cached_property_slot(self):
        obj = self.make(3)
        cache = InlineCache("x")
        self.assertEqual(cache.get(obj), 3)
        delete_property(obj, "x")
        self.assertIs(cache.get(obj), UNDEFINED)
        self.assertGreaterEqual(cache.counters.guard_failures, 1)

    def test_prototype_shape_guard_participates_in_lookup(self):
        proto = self.make(12)
        child = JSObject(self.registry, proto)
        cache = InlineCache("x")
        self.assertEqual(cache.get(child), 12)
        define_data_property(proto, "x", 13)
        self.assertEqual(cache.get(child), 13)
        self.assertEqual(cache.counters.guard_failures, 1)

    def test_getter_is_never_cached_and_receives_receiver(self):
        receiver = self.make(0)
        calls = []
        define_accessor_property(receiver, "value", getter=lambda this: calls.append(this) or 5)
        cache = InlineCache("value")
        self.assertEqual(cache.get(receiver), 5)
        self.assertEqual(cache.get(receiver), 5)
        self.assertEqual(calls, [receiver, receiver])
        self.assertEqual(cache.counters.ic_hits, 0)

    def test_inherited_getter_receives_original_receiver(self):
        calls = []
        proto = JSObject(self.registry)
        define_accessor_property(proto, "value", getter=lambda this: calls.append(this) or 8)
        receiver = JSObject(self.registry, proto)
        self.assertEqual(get_property(receiver, "value"), 8)
        self.assertEqual(calls, [receiver])

    def test_inherited_setter_receives_original_receiver(self):
        calls = []
        proto = JSObject(self.registry)
        define_accessor_property(proto, "value", setter=lambda this, value: calls.append((this, value)))
        receiver = JSObject(self.registry, proto)
        self.assertTrue(set_property(receiver, "value", 17))
        self.assertEqual(calls, [(receiver, 17)])
        self.assertIs(receiver.own_descriptor("value"), None)

    def test_inherited_writable_data_creates_receiver_own_property(self):
        proto = self.make(4)
        child = JSObject(self.registry, proto)
        self.assertTrue(set_property(child, "x", 9))
        self.assertEqual(get_property(child, "x"), 9)
        self.assertEqual(get_property(proto, "x"), 4)

    def test_inherited_non_writable_data_rejects_assignment(self):
        proto = JSObject(self.registry)
        define_data_property(proto, "x", 4, writable=False)
        child = JSObject(self.registry, proto)
        self.assertFalse(set_property(child, "x", 9))
        with self.assertRaises(JSRuntimeError):
            set_property(child, "x", 9, strict=True)

    def test_missing_accessor_setter_rejects_assignment(self):
        obj = JSObject(self.registry)
        define_accessor_property(obj, "x", getter=lambda this: 1)
        self.assertFalse(set_property(obj, "x", 2))
        with self.assertRaises(JSRuntimeError):
            set_property(obj, "x", 2, strict=True)

    def test_set_cache_uses_guarded_slot_and_current_value(self):
        obj = self.make(1)
        cache = InlineCache("x", operation="set")
        self.assertTrue(cache.set(obj, 2))
        self.assertTrue(cache.set(obj, 3))
        self.assertEqual(get_property(obj, "x"), 3)
        self.assertEqual(cache.counters.ic_hits, 1)

    def test_megamorphic_site_falls_back_permanently(self):
        cache = InlineCache("x", max_cases=2)
        objects = []
        for index in range(5):
            obj = JSObject(self.registry)
            for slot in range(index + 1):
                define_data_property(obj, f"p{slot}", slot)
            define_data_property(obj, "x", index)
            objects.append(obj)
            self.assertEqual(cache.get(obj), index)
        self.assertEqual(cache.state, CacheState.MEGAMORPHIC)
        before = cache.counters.fallback_calls
        self.assertEqual(cache.get(objects[-1]), 4)
        self.assertEqual(cache.counters.fallback_calls, before + 1)
        self.assertEqual(cache.counters.megamorphic_fallbacks, 2)


class ProxyFallbackTests(unittest.TestCase):
    def setUp(self):
        self.registry = ShapeRegistry()

    def test_proxy_get_trap_runs_and_is_never_cached(self):
        target = JSObject(self.registry)
        calls = []
        proxy = JSProxy(target, get_trap=lambda target, key, receiver: calls.append(key) or 42)
        cache = InlineCache("x")
        self.assertEqual(cache.get(proxy), 42)
        self.assertEqual(cache.get(proxy), 42)
        self.assertEqual(calls, ["x", "x"])
        self.assertEqual(cache.counters.proxy_fallbacks, 2)

    def test_proxy_get_trap_must_preserve_frozen_data_invariant(self):
        target = JSObject(self.registry)
        define_data_property(target, "x", 1, writable=False, configurable=False)
        proxy = JSProxy(target, get_trap=lambda target, key, receiver: 2)
        with self.assertRaisesRegex(JSRuntimeError, "frozen data"):
            get_property(proxy, "x")

    def test_proxy_get_trap_cannot_hide_getterless_accessor(self):
        target = JSObject(self.registry)
        define_accessor_property(target, "x", configurable=False)
        proxy = JSProxy(target, get_trap=lambda target, key, receiver: 1)
        with self.assertRaisesRegex(JSRuntimeError, "getter-less"):
            get_property(proxy, "x")

    def test_proxy_set_trap_false_obeys_strictness(self):
        target = JSObject(self.registry)
        proxy = JSProxy(target, set_trap=lambda target, key, value, receiver: False)
        self.assertFalse(set_property(proxy, "x", 1))
        with self.assertRaisesRegex(JSRuntimeError, "returned false"):
            set_property(proxy, "x", 1, strict=True)

    def test_proxy_set_trap_must_preserve_frozen_data_invariant(self):
        target = JSObject(self.registry)
        define_data_property(target, "x", 1, writable=False, configurable=False)
        proxy = JSProxy(target, set_trap=lambda target, key, value, receiver: True)
        with self.assertRaisesRegex(JSRuntimeError, "frozen data"):
            set_property(proxy, "x", 2)

    def test_proxy_set_trap_must_preserve_setterless_accessor_invariant(self):
        target = JSObject(self.registry)
        define_accessor_property(target, "x", configurable=False)
        proxy = JSProxy(target, set_trap=lambda target, key, value, receiver: True)
        with self.assertRaisesRegex(JSRuntimeError, "setter-less"):
            set_property(proxy, "x", 2)

    def test_proxy_delete_trap_must_not_hide_non_configurable_property(self):
        target = JSObject(self.registry)
        define_data_property(target, "x", 1, configurable=False)
        proxy = JSProxy(target, delete_trap=lambda target, key: True)
        with self.assertRaisesRegex(JSRuntimeError, "non-configurable"):
            delete_property(proxy, "x")

    def test_same_value_distinguishes_signed_zero_and_accepts_nan(self):
        self.assertFalse(same_value(0.0, -0.0))
        self.assertTrue(same_value(float("nan"), float("nan")))


if __name__ == "__main__":
    unittest.main()
