"""M0 object-shape and inline-cache semantic proof model (not an Ogma runtime)."""
from __future__ import annotations

from collections import Counter
from dataclasses import dataclass, field
from enum import Enum
from itertools import count
import math
from typing import Any, Callable, Iterable

UNDEFINED = object()
_IDS = count(1)


class JSRuntimeError(Exception):
    pass


@dataclass(frozen=True)
class PropertyLayout:
    key: str
    kind: str
    writable: bool
    enumerable: bool
    configurable: bool


@dataclass(frozen=True)
class Shape:
    identity: int
    prototype_identity: int | None
    properties: tuple[PropertyLayout, ...]

    def slot_for(self, key: str) -> int | None:
        for index, item in enumerate(self.properties):
            if item.key == key:
                return index
        return None


class ShapeRegistry:
    def __init__(self) -> None:
        self._shapes: dict[tuple[tuple[PropertyLayout, ...], int | None], Shape] = {}
        self._next = 1
        self.transitions: Counter[tuple[int, int, str]] = Counter()

    def intern(self, properties: Iterable[PropertyLayout], prototype: JSObject | JSProxy | None) -> Shape:
        ordered = tuple(properties)
        proto_id = None if prototype is None else prototype.identity
        key = (ordered, proto_id)
        shape = self._shapes.get(key)
        if shape is None:
            shape = Shape(self._next, proto_id, ordered)
            self._next += 1
            self._shapes[key] = shape
        return shape

    def transition(self, source: Shape, properties: Iterable[PropertyLayout],
                   prototype: JSObject | JSProxy | None, reason: str) -> Shape:
        result = self.intern(properties, prototype)
        if source.identity != result.identity:
            self.transitions[(source.identity, result.identity, reason)] += 1
        return result

    @property
    def shape_count(self) -> int:
        return len(self._shapes)


@dataclass(frozen=True)
class AccessorPair:
    getter: Callable[[Any], Any] | None = None
    setter: Callable[[Any, Any], Any] | None = None


@dataclass(frozen=True)
class PropertyDescriptor:
    kind: str
    value: Any = UNDEFINED
    getter: Callable[[Any], Any] | None = None
    setter: Callable[[Any, Any], Any] | None = None
    writable: bool = False
    enumerable: bool = False
    configurable: bool = False


class JSObject:
    def __init__(self, registry: ShapeRegistry, prototype: JSObject | JSProxy | None = None) -> None:
        self.identity = next(_IDS)
        self.registry = registry
        self.prototype = prototype
        self.shape = registry.intern((), prototype)
        self.slots: list[Any] = []
        self.extensible = True

    def own_descriptor(self, key: str) -> PropertyDescriptor | None:
        index = self.shape.slot_for(key)
        if index is None:
            return None
        layout = self.shape.properties[index]
        value = self.slots[index]
        if layout.kind == "data":
            return PropertyDescriptor("data", value=value, writable=layout.writable,
                                      enumerable=layout.enumerable, configurable=layout.configurable)
        pair: AccessorPair = value
        return PropertyDescriptor("accessor", getter=pair.getter, setter=pair.setter,
                                  enumerable=layout.enumerable, configurable=layout.configurable)


@dataclass
class JSProxy:
    target: JSObject
    get_trap: Callable[[JSObject, str, Any], Any] | None = None
    set_trap: Callable[[JSObject, str, Any, Any], bool] | None = None
    delete_trap: Callable[[JSObject, str], bool] | None = None
    identity: int = field(default_factory=lambda: next(_IDS), init=False)


ObjectLike = JSObject | JSProxy


def same_value(left: Any, right: Any) -> bool:
    if left is right:
        return True
    if isinstance(left, bool) or isinstance(right, bool):
        return type(left) is type(right) and left == right
    if isinstance(left, (int, float)) and isinstance(right, (int, float)):
        a, b = float(left), float(right)
        if math.isnan(a) and math.isnan(b):
            return True
        if a == 0 and b == 0:
            return math.copysign(1.0, a) == math.copysign(1.0, b)
        return a == b
    if isinstance(left, (JSObject, JSProxy)) or isinstance(right, (JSObject, JSProxy)):
        return left is right
    return type(left) is type(right) and left == right


def _key(key: str) -> None:
    if not isinstance(key, str):
        raise JSRuntimeError("TypeError: M0 proof supports string keys only")


def _reject(message: str, strict: bool) -> bool:
    if strict:
        raise JSRuntimeError("TypeError: " + message)
    return False


def define_data_property(obj: JSObject, key: str, value: Any, *, writable: bool = True,
                         enumerable: bool = True, configurable: bool = True,
                         strict: bool = False) -> bool:
    _key(key)
    index = obj.shape.slot_for(key)
    old = None if index is None else obj.shape.properties[index]
    descriptor = obj.own_descriptor(key)
    if old is None and not obj.extensible:
        return _reject("object is not extensible", strict)
    if old is not None and not old.configurable:
        if configurable or enumerable != old.enumerable:
            return _reject("cannot redefine non-configurable property", strict)
        if old.kind != "data":
            return _reject("cannot convert non-configurable accessor to data", strict)
        if not old.writable and (writable or not same_value(descriptor.value, value)):
            return _reject("cannot change non-writable property", strict)
    layout = PropertyLayout(key, "data", writable, enumerable, configurable)
    if index is None:
        obj.slots.append(value)
        obj.shape = obj.registry.transition(obj.shape, (*obj.shape.properties, layout),
                                            obj.prototype, "add-data")
    elif old == layout:
        obj.slots[index] = value
    else:
        properties = list(obj.shape.properties)
        properties[index] = layout
        obj.slots[index] = value
        obj.shape = obj.registry.transition(obj.shape, properties, obj.prototype, "reconfigure-data")
    return True


def define_accessor_property(obj: JSObject, key: str, *,
                             getter: Callable[[Any], Any] | None = None,
                             setter: Callable[[Any, Any], Any] | None = None,
                             enumerable: bool = True, configurable: bool = True,
                             strict: bool = False) -> bool:
    _key(key)
    index = obj.shape.slot_for(key)
    old = None if index is None else obj.shape.properties[index]
    descriptor = obj.own_descriptor(key)
    if old is None and not obj.extensible:
        return _reject("object is not extensible", strict)
    if old is not None and not old.configurable:
        if configurable or enumerable != old.enumerable:
            return _reject("cannot redefine non-configurable accessor", strict)
        if old.kind != "accessor" or descriptor.getter is not getter or descriptor.setter is not setter:
            return _reject("cannot change non-configurable property", strict)
    layout = PropertyLayout(key, "accessor", False, enumerable, configurable)
    pair = AccessorPair(getter, setter)
    if index is None:
        obj.slots.append(pair)
        obj.shape = obj.registry.transition(obj.shape, (*obj.shape.properties, layout),
                                            obj.prototype, "add-accessor")
    elif old == layout:
        obj.slots[index] = pair
    else:
        properties = list(obj.shape.properties)
        properties[index] = layout
        obj.slots[index] = pair
        obj.shape = obj.registry.transition(obj.shape, properties, obj.prototype, "reconfigure-accessor")
    return True


def _get_own(target: ObjectLike, key: str) -> PropertyDescriptor | None:
    return target.target.own_descriptor(key) if isinstance(target, JSProxy) else target.own_descriptor(key)


def get_property(target: ObjectLike, key: str, *, receiver: Any = None) -> Any:
    _key(key)
    if receiver is None:
        receiver = target
    if isinstance(target, JSProxy):
        if target.get_trap is None:
            return get_property(target.target, key, receiver=receiver)
        result = target.get_trap(target.target, key, receiver)
        fixed = target.target.own_descriptor(key)
        if fixed and not fixed.configurable:
            if fixed.kind == "data" and not fixed.writable and not same_value(result, fixed.value):
                raise JSRuntimeError("TypeError: Proxy get trap violated frozen data invariant")
            if fixed.kind == "accessor" and fixed.getter is None and result is not UNDEFINED:
                raise JSRuntimeError("TypeError: Proxy get trap violated getter-less accessor invariant")
        return result
    descriptor = target.own_descriptor(key)
    if descriptor:
        if descriptor.kind == "data":
            return descriptor.value
        return UNDEFINED if descriptor.getter is None else descriptor.getter(receiver)
    return UNDEFINED if target.prototype is None else get_property(target.prototype, key, receiver=receiver)


def _define_on_receiver(receiver: Any, key: str, value: Any, strict: bool) -> bool:
    if isinstance(receiver, JSProxy):
        if receiver.set_trap is not None:
            accepted = bool(receiver.set_trap(receiver.target, key, value, receiver))
            if not accepted:
                return _reject("Proxy set trap returned false", strict)
            fixed = receiver.target.own_descriptor(key)
            if fixed and not fixed.configurable:
                if fixed.kind == "data" and not fixed.writable and not same_value(value, fixed.value):
                    raise JSRuntimeError("TypeError: Proxy set trap violated frozen data invariant")
                if fixed.kind == "accessor" and fixed.setter is None:
                    raise JSRuntimeError("TypeError: Proxy set trap violated setter-less accessor invariant")
            return True
        receiver = receiver.target
    if not isinstance(receiver, JSObject):
        return _reject("receiver is not an object", strict)
    own = receiver.own_descriptor(key)
    if own and (own.kind != "data" or not own.writable):
        return _reject("receiver property is not writable data", strict)
    return define_data_property(receiver, key, value, strict=strict)


def set_property(target: ObjectLike, key: str, value: Any, *,
                 receiver: Any = None, strict: bool = False) -> bool:
    _key(key)
    if receiver is None:
        receiver = target
    if isinstance(target, JSProxy):
        if target.set_trap is None:
            return set_property(target.target, key, value, receiver=receiver, strict=strict)
        accepted = bool(target.set_trap(target.target, key, value, receiver))
        if not accepted:
            return _reject("Proxy set trap returned false", strict)
        fixed = target.target.own_descriptor(key)
        if fixed and not fixed.configurable:
            if fixed.kind == "data" and not fixed.writable and not same_value(value, fixed.value):
                raise JSRuntimeError("TypeError: Proxy set trap violated frozen data invariant")
            if fixed.kind == "accessor" and fixed.setter is None:
                raise JSRuntimeError("TypeError: Proxy set trap violated setter-less accessor invariant")
        return True
    descriptor = target.own_descriptor(key)
    if descriptor:
        if descriptor.kind == "accessor":
            if descriptor.setter is None:
                return _reject("property has no setter", strict)
            descriptor.setter(receiver, value)
            return True
        if not descriptor.writable:
            return _reject("property is not writable", strict)
        if receiver is target:
            index = target.shape.slot_for(key)
            target.slots[index] = value
            return True
        return _define_on_receiver(receiver, key, value, strict)
    if target.prototype is not None:
        inherited = _find_descriptor(target.prototype, key)
        if inherited:
            if inherited.kind == "accessor":
                if inherited.setter is None:
                    return _reject("inherited property has no setter", strict)
                inherited.setter(receiver, value)
                return True
            if not inherited.writable:
                return _reject("inherited property is not writable", strict)
    return _define_on_receiver(receiver, key, value, strict)


def _find_descriptor(target: ObjectLike, key: str) -> PropertyDescriptor | None:
    if isinstance(target, JSProxy):
        return _find_descriptor(target.target, key)
    own = target.own_descriptor(key)
    if own:
        return own
    return None if target.prototype is None else _find_descriptor(target.prototype, key)


def delete_property(target: ObjectLike, key: str, *, strict: bool = False) -> bool:
    _key(key)
    if isinstance(target, JSProxy):
        if target.delete_trap is None:
            return delete_property(target.target, key, strict=strict)
        if not target.delete_trap(target.target, key):
            return _reject("Proxy deleteProperty trap returned false", strict)
        fixed = target.target.own_descriptor(key)
        if fixed and not fixed.configurable:
            raise JSRuntimeError("TypeError: Proxy delete trap cannot hide non-configurable property")
        return True
    index = target.shape.slot_for(key)
    if index is None:
        return True
    if not target.shape.properties[index].configurable:
        return _reject("property is not configurable", strict)
    layout = target.shape.properties[:index] + target.shape.properties[index + 1:]
    del target.slots[index]
    target.shape = target.registry.transition(target.shape, layout, target.prototype, "delete")
    return True


class CacheState(str, Enum):
    UNINITIALIZED = "uninitialized"
    MONOMORPHIC = "monomorphic"
    POLYMORPHIC = "polymorphic"
    MEGAMORPHIC = "megamorphic"


@dataclass(frozen=True)
class CachePlan:
    guards: tuple[int, ...]
    owner_depth: int
    slot_index: int | None
    found: bool


@dataclass
class CacheCounters:
    ic_hits: int = 0
    ic_misses: int = 0
    helper_calls: int = 0
    fallback_calls: int = 0
    guard_failures: int = 0
    megamorphic_fallbacks: int = 0
    proxy_fallbacks: int = 0
    cache_installs: int = 0


class InlineCache:
    def __init__(self, key: str, *, operation: str = "get", max_cases: int = 4) -> None:
        _key(key)
        if operation not in {"get", "set"} or max_cases < 1:
            raise ValueError("invalid IC operation or case limit")
        self.key, self.operation, self.max_cases = key, operation, max_cases
        self._entries: list[CachePlan] = []
        self.state = CacheState.UNINITIALIZED
        self.counters = CacheCounters()

    @property
    def case_count(self) -> int:
        return len(self._entries)

    @staticmethod
    def _chain(receiver: Any, limit: int | None = None) -> tuple[list[JSObject], bool]:
        chain: list[JSObject] = []
        cursor = receiver
        while isinstance(cursor, JSObject):
            chain.append(cursor)
            cursor = cursor.prototype
            if limit is not None and len(chain) >= limit:
                return chain, cursor is None
        return chain, cursor is None

    def _matches(self, receiver: Any, plan: CachePlan) -> bool:
        chain, ended = self._chain(receiver, len(plan.guards))
        return (
            len(chain) == len(plan.guards)
            and tuple(obj.shape.identity for obj in chain) == plan.guards
            and (plan.found or ended)
        )

    def _build_get_plan(self, receiver: Any) -> CachePlan | None:
        if not isinstance(receiver, JSObject):
            return None
        chain: list[JSObject] = []
        cursor: JSObject | JSProxy | None = receiver
        while isinstance(cursor, JSObject):
            chain.append(cursor)
            slot = cursor.shape.slot_for(self.key)
            if slot is not None:
                if cursor.shape.properties[slot].kind != "data":
                    return None
                return CachePlan(tuple(item.shape.identity for item in chain), len(chain) - 1, slot, True)
            cursor = cursor.prototype
        if cursor is not None:
            return None
        return CachePlan(tuple(item.shape.identity for item in chain), len(chain) - 1, None, False)

    def _build_set_plan(self, receiver: Any) -> CachePlan | None:
        if not isinstance(receiver, JSObject):
            return None
        slot = receiver.shape.slot_for(self.key)
        if slot is None:
            return None
        layout = receiver.shape.properties[slot]
        if layout.kind != "data" or not layout.writable:
            return None
        return CachePlan((receiver.shape.identity,), 0, slot, True)

    def _install(self, plan: CachePlan | None) -> None:
        if self.state is CacheState.MEGAMORPHIC or plan is None:
            return
        if any(old.guards == plan.guards and old.found == plan.found for old in self._entries):
            return
        if len(self._entries) >= self.max_cases:
            self._entries.clear()
            self.state = CacheState.MEGAMORPHIC
            self.counters.megamorphic_fallbacks += 1
            return
        self._entries.append(plan)
        self.state = CacheState.MONOMORPHIC if len(self._entries) == 1 else CacheState.POLYMORPHIC
        self.counters.cache_installs += 1

    def _fallback(self, receiver: ObjectLike, *, value: Any = None, strict: bool = False) -> Any:
        self.counters.helper_calls += 1
        self.counters.fallback_calls += 1
        if isinstance(receiver, JSProxy):
            self.counters.proxy_fallbacks += 1
        if self.operation == "get":
            result = get_property(receiver, self.key)
            self._install(self._build_get_plan(receiver))
            return result
        result = set_property(receiver, self.key, value, strict=strict)
        self._install(self._build_set_plan(receiver))
        return result

    def get(self, receiver: ObjectLike) -> Any:
        if self.operation != "get":
            raise RuntimeError("get on set IC")
        if self.state is CacheState.MEGAMORPHIC:
            self.counters.megamorphic_fallbacks += 1
            return self._fallback(receiver)
        for plan in self._entries:
            if self._matches(receiver, plan):
                self.counters.ic_hits += 1
                if not plan.found:
                    return UNDEFINED
                chain, _ = self._chain(receiver, plan.owner_depth + 1)
                return chain[plan.owner_depth].slots[plan.slot_index]
        self.counters.ic_misses += 1
        if self._entries:
            self.counters.guard_failures += 1
        return self._fallback(receiver)

    def set(self, receiver: ObjectLike, value: Any, *, strict: bool = False) -> bool:
        if self.operation != "set":
            raise RuntimeError("set on get IC")
        if self.state is CacheState.MEGAMORPHIC:
            self.counters.megamorphic_fallbacks += 1
            return self._fallback(receiver, value=value, strict=strict)
        if isinstance(receiver, JSObject):
            for plan in self._entries:
                if self._matches(receiver, plan):
                    self.counters.ic_hits += 1
                    receiver.slots[plan.slot_index] = value
                    return True
        self.counters.ic_misses += 1
        if self._entries:
            self.counters.guard_failures += 1
        return self._fallback(receiver, value=value, strict=strict)


def shape_memory_summary(registry: ShapeRegistry, objects: list[JSObject]) -> dict[str, int]:
    layouts = sum(len(shape.properties) for shape in registry._shapes.values())
    return {
        "object_count": len(objects),
        "unique_shape_count": registry.shape_count,
        "shape_property_layout_count": layouts,
        "object_slot_count": sum(len(obj.slots) for obj in objects),
        "shared_shape_reference_count": len(objects),
    }


def set_prototype_of(target: JSObject, prototype: JSObject | JSProxy | None, *,
                     strict: bool = False) -> bool:
    cursor = prototype
    visited: set[int] = set()
    while cursor is not None:
        if cursor is target:
            return _reject("cyclic prototype value", strict)
        if cursor.identity in visited:
            raise JSRuntimeError("TypeError: existing cyclic prototype chain")
        visited.add(cursor.identity)
        cursor = cursor.target.prototype if isinstance(cursor, JSProxy) else cursor.prototype
    if not target.extensible and target.prototype is not prototype:
        return _reject("cannot change prototype of a non-extensible object", strict)
    if target.prototype is prototype:
        return True
    old_shape = target.shape
    target.prototype = prototype
    target.shape = target.registry.transition(
        old_shape, old_shape.properties, prototype, "set-prototype"
    )
    return True


def prevent_extensions(target: JSObject) -> None:
    target.extensible = False
