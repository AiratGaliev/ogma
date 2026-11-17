"""M0 interpreter/debugger proof; not an ECMAScript runtime or production VM."""

from __future__ import annotations

from dataclasses import dataclass, field
from itertools import count
from queue import SimpleQueue
from threading import get_ident
from typing import Any, Callable, Mapping

from debugger_api import (
    Breakpoint,
    CodeGeneration,
    DebugValue,
    DebuggerError,
    ExceptionEvent,
    ExecutionResult,
    InspectedObject,
    InspectedProperty,
    ScopeSnapshot,
    SourceLocation,
    SourcePosition,
    StackFrameSnapshot,
    StoppedEvent,
    VariableSnapshot,
)


@dataclass(frozen=True)
class ObjectRef:
    """Opaque, typed reference; arbitrary strings are never treated as roots."""
    object_id: str


@dataclass
class Cell:
    value: Any


@dataclass(frozen=True)
class PropertyDescriptor:
    value: Any = None
    enumerable: bool = True
    writable: bool = True
    configurable: bool = True
    getter_present: bool = False
    setter_present: bool = False


@dataclass
class HeapObject:
    descriptors: dict[str, PropertyDescriptor]
    is_proxy: bool = False


class Heap:
    """Small mark/sweep heap model for proving safe inspection handle lifetime."""

    def __init__(self) -> None:
        self.objects: dict[str, HeapObject] = {}
        self._ids = count(1)
        self._temporary_roots: dict[str, int] = {}
        # Test seam: emulate GC at an inspection safepoint.
        self.before_inspection: Callable[[], Any] | None = None

    def allocate(
        self,
        properties: Mapping[str, PropertyDescriptor | Any] | None = None,
        *,
        is_proxy: bool = False,
    ) -> ObjectRef:
        descriptors: dict[str, PropertyDescriptor] = {}
        for name, value in (properties or {}).items():
            descriptors[name] = value if isinstance(value, PropertyDescriptor) else PropertyDescriptor(value=value)
        ident = f"heap-{next(self._ids)}"
        self.objects[ident] = HeapObject(descriptors, is_proxy)
        return ObjectRef(ident)

    def _references(self, value: Any, seen: set[int] | None = None):
        seen = set() if seen is None else seen
        if isinstance(value, ObjectRef):
            yield value.object_id
        elif isinstance(value, Cell):
            marker = id(value)
            if marker not in seen:
                seen.add(marker)
                yield from self._references(value.value, seen)
        elif isinstance(value, (list, tuple, set)):
            marker = id(value)
            if marker not in seen:
                seen.add(marker)
                for item in value:
                    yield from self._references(item, seen)
        elif isinstance(value, dict):
            marker = id(value)
            if marker not in seen:
                seen.add(marker)
                for item in value.values():
                    yield from self._references(item, seen)
        elif isinstance(value, _Closure):
            marker = id(value)
            if marker not in seen:
                seen.add(marker)
                yield from self._references(value.captures, seen)

    def collect(self, roots: Any = ()) -> tuple[str, ...]:
        marked: set[str] = set(self._temporary_roots)
        work = list(self._references(roots))
        while work:
            ident = work.pop()
            if ident in marked or ident not in self.objects:
                continue
            marked.add(ident)
            obj = self.objects[ident]
            for descriptor in obj.descriptors.values():
                work.extend(self._references(descriptor.value))
        removed = tuple(sorted(set(self.objects) - marked))
        for ident in removed:
            del self.objects[ident]
        return removed

    def inspect(self, reference: ObjectRef, limit: int) -> InspectedObject:
        if not isinstance(reference, ObjectRef):
            raise DebuggerError("inspection requires a typed object reference")
        if limit < 0 or limit > 128:
            raise DebuggerError("inspection limit must be between 0 and 128")
        if reference.object_id not in self.objects:
            raise DebuggerError("object is no longer available")
        self._temporary_roots[reference.object_id] = self._temporary_roots.get(reference.object_id, 0) + 1
        try:
            if self.before_inspection is not None:
                self.before_inspection()
            obj = self.objects.get(reference.object_id)
            if obj is None:
                raise DebuggerError("object was collected during inspection")
            if obj.is_proxy:
                # Do not execute Proxy traps or query its target.
                return InspectedObject(reference.object_id, True, ())
            names = list(obj.descriptors)
            properties = []
            for name in names[:limit]:
                descriptor = obj.descriptors[name]
                if not descriptor.enumerable:
                    continue
                if descriptor.getter_present or descriptor.setter_present:
                    value = DebugValue("accessor", "<not evaluated>", availability="not_evaluated")
                else:
                    value = debug_value(descriptor.value, self)
                properties.append(
                    InspectedProperty(
                        name, value, descriptor.enumerable, descriptor.writable,
                        descriptor.configurable, descriptor.getter_present or descriptor.setter_present,
                    )
                )
            return InspectedObject(reference.object_id, False, tuple(properties), len(names) > limit)
        finally:
            remaining = self._temporary_roots[reference.object_id] - 1
            if remaining:
                self._temporary_roots[reference.object_id] = remaining
            else:
                del self._temporary_roots[reference.object_id]


@dataclass
class ClosureValue:
    function_id: str
    captures: dict[str, Any]


@dataclass
class Frame:
    id: str
    function: Mapping[str, Any]
    pc: int = 0
    locals: dict[str, Any] = field(default_factory=dict)
    captures: dict[str, Any] = field(default_factory=dict)
    location: SourceLocation | None = None
    return_to: "Frame | None" = None
    return_dest: str | None = None

    def _binding(self, binding_id: str) -> Mapping[str, Any] | None:
        for binding in self.function.get("environment", {}).get("bindings", []):
            if binding.get("id") == binding_id:
                return binding
        for capture in self.function.get("captures", []):
            if capture.get("id") == binding_id:
                return capture
        return None

    def read(self, binding_id: str) -> Any:
        if binding_id in self.locals:
            value = self.locals[binding_id]
        elif binding_id in self.captures:
            value = self.captures[binding_id]
        else:
            raise DebuggerError(f"binding {binding_id!r} is unavailable")
        return value.value if isinstance(value, Cell) else value

    def write(self, binding_id: str, value: Any) -> None:
        if binding_id in self.locals and isinstance(self.locals[binding_id], Cell):
            self.locals[binding_id].value = value
        elif binding_id in self.captures and isinstance(self.captures[binding_id], Cell):
            self.captures[binding_id].value = value
        elif binding_id in self.locals or binding_id in self.captures:
            target = self.locals if binding_id in self.locals else self.captures
            target[binding_id] = value
        else:
            descriptor = self._binding(binding_id)
            if descriptor is None:
                raise DebuggerError(f"unknown binding {binding_id!r}")
            target = self.locals if binding_id in {
                item.get("id") for item in self.function.get("environment", {}).get("bindings", [])
            } else self.captures
            target[binding_id] = Cell(value) if descriptor.get("mutable", False) else value

    def ensure_cell(self, binding_id: str) -> Cell:
        if binding_id in self.locals:
            target = self.locals
        elif binding_id in self.captures:
            target = self.captures
        else:
            raise DebuggerError(f"cannot capture missing binding {binding_id!r}")
        value = target[binding_id]
        if isinstance(value, Cell):
            return value
        cell = Cell(value)
        target[binding_id] = cell
        return cell


@dataclass
class _Thrown(Exception):
    value: Any


def _safe_preview(value: Any, maximum: int = 256) -> tuple[str, str, bool]:
    if value is None:
        return "null", "null", False
    if isinstance(value, bool):
        return "boolean", "true" if value else "false", False
    if isinstance(value, (int, float)):
        return "number", str(value), False
    if isinstance(value, str):
        preview = value[:maximum]
        return "string", preview, len(preview) < len(value)
    if isinstance(value, ObjectRef):
        return "object", f"Object ({value.object_id})", False
    if isinstance(value, ClosureValue):
        return "function", f"Function ({value.function_id})", False
    # Never invoke user-controlled __repr__, toString or proxy traps.
    return "unavailable", "<not inspectable>", False


def debug_value(value: Any, heap: Heap | None = None) -> DebugValue:
    kind, preview, truncated = _safe_preview(value)
    object_id = value.object_id if isinstance(value, ObjectRef) else None
    if object_id is not None and heap is not None and object_id not in heap.objects:
        return DebugValue(kind, "<collected>", object_id, "unavailable")
    return DebugValue(kind, preview, object_id, "available", truncated)


def _source_key(function_id: str, location: SourceLocation | None) -> tuple[Any, ...] | None:
    if location is None or location.origin != "user" or location.start is None:
        return None
    return (
        function_id, location.source_id, location.start.line, location.start.column,
    )


class Debugger:
    """Backend-neutral control plane; execution state remains owner-thread only."""

    def __init__(self, image: Mapping[str, Any], backend_name: str = "interpreter") -> None:
        self.image = image
        self._backend_name = backend_name
        self._session_id = f"debug-{id(self):x}"
        self._owner_thread = get_ident()
        self._commands: SimpleQueue[str] = SimpleQueue()
        self._breakpoints: dict[str, Breakpoint] = {}
        self._breakpoint_ids = count(1)
        self._state = "running"
        self._event: StoppedEvent | ExceptionEvent | None = None
        self._frames: list[Frame] = []
        self._heap: Heap | None = None
        self._step: tuple[str, int, tuple[Any, ...] | None] | None = None
        self._capabilities = {
            "pause": backend_name == "interpreter",
            "breakpoints": backend_name == "interpreter",
            "stepInto": backend_name == "interpreter",
            "stepOver": backend_name == "interpreter",
            "stepOut": backend_name == "interpreter",
            "callStack": True,
            "scopes": True,
            "safeInspection": False,
            "exceptionEvents": True,
        }

    @property
    def backend_name(self) -> str:
        return self._backend_name

    @property
    def backend_capabilities(self) -> Mapping[str, bool]:
        return dict(self._capabilities)

    @property
    def session_id(self) -> str:
        return self._session_id

    @property
    def state(self) -> str:
        return self._state

    def _require_owner(self) -> None:
        if get_ident() != self._owner_thread:
            raise DebuggerError("execution-state access must run on the isolate owner thread")

    def attach_heap(self, heap: Heap) -> None:
        self._require_owner()
        self._heap = heap
        self._capabilities["safeInspection"] = True

    def _generation(self) -> CodeGeneration:
        return CodeGeneration(**self.image["code_generation"])

    def _all_source_locations(self):
        for function in self.image.get("functions", []):
            source_map = function.get("debug_metadata", {}).get("source_map", {})
            for item in source_map.get("locations", []):
                yield function, item["source"]

    def set_breakpoint(
        self,
        source_id: str,
        line: int,
        column: int | None = None,
        *,
        sha256: str | None = None,
        code_generation: CodeGeneration | None = None,
    ) -> Breakpoint:
        self._require_owner()
        if self._state == "detached":
            raise DebuggerError("debugger session is detached")
        if not isinstance(source_id, str) or not source_id:
            raise DebuggerError("source id must be a non-empty string")
        if isinstance(line, bool) or not isinstance(line, int) or line < 1:
            raise DebuggerError("line must be a positive integer")
        if column is not None and (
            isinstance(column, bool) or not isinstance(column, int) or column < 0
        ):
            raise DebuggerError("column must be a non-negative integer")
        files = {item["id"]: item for item in self.image.get("source_files", [])}
        source_file = files.get(source_id)
        verified, message = bool(source_file), None
        if source_file is None:
            verified, message = False, "unknown source file"
        elif sha256 is not None and source_file.get("sha256") != sha256:
            verified, message = False, "source digest does not match loaded source"
        if code_generation is not None and code_generation != self._generation():
            verified, message = False, "code generation does not match loaded code"
        executable = any(
            source.get("origin") == "user"
            and source.get("file") == source_id
            and source.get("start", {}).get("line") == line
            and (column is None or source.get("start", {}).get("column") == column)
            for _, source in self._all_source_locations()
        )
        if verified and not executable:
            verified, message = False, "no executable source location matches this line/column"
        ident = f"bp-{next(self._breakpoint_ids)}"
        breakpoint = Breakpoint(
            ident, source_id, sha256, code_generation, line, column, verified, message
        )
        self._breakpoints[ident] = breakpoint
        return breakpoint

    def remove_breakpoint(self, breakpoint_id: str) -> bool:
        self._require_owner()
        return self._breakpoints.pop(breakpoint_id, None) is not None

    def list_breakpoints(self) -> tuple[Breakpoint, ...]:
        self._require_owner()
        return tuple(self._breakpoints.values())

    def request_pause(self) -> None:
        # Thread-safe command hand-off. The VM applies it only at an owner-thread
        # instruction safepoint; transport threads never touch the frame stack.
        if self._state == "detached":
            return
        self._commands.put("pause")

    def _apply_commands(self) -> None:
        while not self._commands.empty():
            command = self._commands.get()
            if command == "pause" and self._state != "detached":
                self._stop("pause")

    def _stop(self, reason: str, description: str | None = None) -> None:
        if self._frames:
            frame = self._frames[-1]
            self._event = StoppedEvent(reason, frame.id, frame.location, description)
        self._state = "paused"

    def _matches_breakpoint(self, location: SourceLocation | None) -> bool:
        if location is None or location.origin != "user" or location.start is None:
            return False
        return any(
            item.verified
            and item.source_id == location.source_id
            and item.line == location.start.line
            and (item.column is None or item.column == location.start.column)
            and (
                item.source_sha256 is None
                or item.source_sha256 == location.source_sha256
            )
            and (
                item.code_generation is None
                or item.code_generation == self._generation()
            )
            for item in self._breakpoints.values()
        )

    def before_instruction(self, frame: Frame, location: SourceLocation | None) -> bool:
        self._require_owner()
        frame.location = location
        self._apply_commands()
        if self._state != "running":
            return False
        if self._matches_breakpoint(location):
            self._stop("breakpoint")
            return False
        if self._step is not None:
            mode, base_depth, base_key = self._step
            key = _source_key(frame.function["id"], location)
            depth = len(self._frames)
            changed = key is not None and key != base_key
            should_stop = (
                (mode == "into" and changed)
                or (mode == "over" and depth <= base_depth and changed)
                or (mode == "out" and depth < base_depth and changed)
            )
            if should_stop:
                self._step = None
                self._stop("step")
                return False
        return True

    def _require_paused(self) -> None:
        self._require_owner()
        if self._state != "paused":
            raise DebuggerError("execution is not paused")

    def resume(self) -> None:
        self._require_paused()
        self._event = None
        self._step = None
        self._state = "running"

    def _begin_step(self, mode: str) -> None:
        self._require_paused()
        frame = self._frames[-1] if self._frames else None
        base_key = _source_key(frame.function["id"], frame.location) if frame else None
        self._step = (mode, len(self._frames), base_key)
        self._event = None
        self._state = "running"

    def step_into(self) -> None:
        self._begin_step("into")

    def step_over(self) -> None:
        self._begin_step("over")

    def step_out(self) -> None:
        self._begin_step("out")

    def get_stack(self) -> tuple[StackFrameSnapshot, ...]:
        self._require_owner()
        generation = self._generation()
        return tuple(
            StackFrameSnapshot(
                frame.id, frame.function["id"], frame.function["name"], generation,
                frame.location, (frame.function["environment"]["id"],), self._backend_name,
            )
            for frame in reversed(self._frames)
        )

    def get_scopes(self, frame_id: str) -> tuple[ScopeSnapshot, ...]:
        self._require_owner()
        frame = next((item for item in self._frames if item.id == frame_id), None)
        if frame is None:
            raise DebuggerError("unknown stack frame")
        variables: list[VariableSnapshot] = []
        bindings = frame.function.get("environment", {}).get("bindings", [])
        for binding in bindings:
            ident = binding["id"]
            availability = "available" if ident in frame.locals else "optimized_out"
            value = debug_value(frame.read(ident), self._heap) if availability == "available" else DebugValue(
                "unavailable", "<optimized out>", availability="optimized_out"
            )
            variables.append(VariableSnapshot(
                ident, binding["name"], binding.get("mutable", False), availability, value
            ))
        for capture in frame.function.get("captures", []):
            ident = capture["id"]
            availability = "available" if ident in frame.captures else "unavailable"
            value = debug_value(frame.read(ident), self._heap) if availability == "available" else DebugValue(
                "unavailable", "<unavailable>", availability="unavailable"
            )
            variables.append(VariableSnapshot(
                ident, capture["name"], capture.get("mutable", False), availability, value
            ))
        scope_id = frame.function["environment"]["id"]
        return (ScopeSnapshot(scope_id, "Locals", None, tuple(variables)),)

    def get_variables(self, frame_id: str, scope_id: str) -> tuple[VariableSnapshot, ...]:
        scopes = self.get_scopes(frame_id)
        scope = next((item for item in scopes if item.id == scope_id), None)
        if scope is None:
            raise DebuggerError("unknown lexical scope")
        return scope.variables

    def evaluate_identifier(self, frame_id: str, identifier: str) -> DebugValue:
        self._require_owner()
        frame = next((item for item in self._frames if item.id == frame_id), None)
        if frame is None:
            raise DebuggerError("unknown stack frame")
        return debug_value(frame.read(identifier), self._heap)

    def inspect_object(self, object_id: str, *, limit: int = 128) -> InspectedObject:
        self._require_paused()
        if self._heap is None:
            raise DebuggerError("safe inspector is unavailable for this backend")
        return self._heap.inspect(ObjectRef(object_id), limit)

    def event_message(self) -> StoppedEvent | ExceptionEvent | None:
        self._require_owner()
        return self._event

    def emit_exception(self, value: Any, *, caught: bool = False) -> ExceptionEvent:
        self._require_owner()
        frame = self._frames[-1] if self._frames else None
        if frame is None:
            raise DebuggerError("exception event requires an active frame")
        event = ExceptionEvent(debug_value(value, self._heap), frame.id, frame.location, caught)
        self._event = event
        self._state = "paused"
        return event

    def detach(self) -> None:
        self._require_owner()
        self._breakpoints.clear()
        self._step = None
        self._event = None
        self._frames.clear()
        while not self._commands.empty():
            self._commands.get()
        self._state = "detached"


class Interpreter:
    """A deterministic fixture VM implementing a deliberately tiny instruction set."""

    def __init__(self, image: Mapping[str, Any], debugger: Debugger) -> None:
        self.image = image
        self.debugger = debugger
        self.functions = {fn["id"]: fn for fn in image["functions"]}
        self.heap = Heap()
        self.debugger.attach_heap(self.heap)
        self.frames: list[Frame] = []
        self.result: Any = None
        self.started = False
        self.executed_instructions = 0

    def _source_location(self, function: Mapping[str, Any], pc: int) -> SourceLocation | None:
        source_map = function.get("debug_metadata", {}).get("source_map", {})
        item = next((loc for loc in source_map.get("locations", []) if loc["offset"] == pc), None)
        if item is None:
            return None
        source = item["source"]
        if source["origin"] == "generated":
            return SourceLocation(
                "generated", reason=source["reason"], scope_id=source.get("scope"),
                binding_ids=tuple(source.get("bindings", [])),
                inline_chain=tuple(source.get("inline_chain", [])),
            )
        files = {file["id"]: file for file in self.image.get("source_files", [])}
        file = files[source["file"]]
        return SourceLocation(
            "user", source["file"], file.get("sha256"),
            SourcePosition(**source["start"]), SourcePosition(**source["end"]),
            scope_id=source.get("scope"), binding_ids=tuple(source.get("bindings", [])),
            inline_chain=tuple(source.get("inline_chain", [])),
        )

    def run(self, max_steps: int = 10000) -> ExecutionResult:
        if self.debugger.state == "detached":
            raise DebuggerError("debugger session is detached")
        if self.debugger.state == "paused":
            raise DebuggerError("resume or step before running")
        if not self.started:
            entry = self.functions[self.image["entry"]]
            self.frames.append(Frame("frame-1", entry))
            self.debugger._frames = self.frames
            self.started = True
        while self.frames and self.executed_instructions < max_steps:
            frame = self.frames[-1]
            instructions = frame.function["instructions"]
            if frame.pc >= len(instructions):
                self._return(frame, None)
                continue
            instruction = instructions[frame.pc]
            location = self._source_location(frame.function, frame.pc)
            if not self.debugger.before_instruction(frame, location):
                return ExecutionResult("paused", event=self.debugger.event_message())
            frame.pc += 1
            self.executed_instructions += 1
            try:
                self._execute(frame, instruction)
            except _Thrown as thrown:
                event = self.debugger.emit_exception(thrown.value)
                return ExecutionResult("exception", event=event)
        if self.frames:
            raise DebuggerError("proof interpreter instruction limit exceeded")
        return ExecutionResult("completed", self.result)

    def _execute(self, frame: Frame, instruction: Mapping[str, Any]) -> None:
        operation = instruction["op"]
        if operation == "const":
            frame.write(instruction["dest"], instruction["value"])
        elif operation == "load":
            frame.write(instruction["dest"], frame.read(instruction["binding"]))
        elif operation == "store":
            frame.write(instruction["binding"], frame.read(instruction["src"]))
        elif operation == "add":
            frame.write(
                instruction["dest"],
                frame.read(instruction["left"]) + frame.read(instruction["right"]),
            )
        elif operation == "object":
            frame.write(instruction["dest"], self.heap.allocate(instruction.get("properties", {})))
        elif operation == "closure":
            function_id = instruction["function"]
            capture_descriptors = {
                item["id"]: item for item in self.functions[function_id].get("captures", [])
            }
            captures: dict[str, Any] = {}
            for capture_id, source_binding in instruction["captures"].items():
                descriptor = capture_descriptors[capture_id]
                if descriptor.get("mutable"):
                    captures[capture_id] = frame.ensure_cell(source_binding)
                else:
                    captures[capture_id] = frame.read(source_binding)
            frame.write(instruction["dest"], ClosureValue(function_id, captures))
        elif operation == "call":
            closure = frame.read(instruction["callee"])
            if not isinstance(closure, ClosureValue):
                raise DebuggerError("call target is not a fixture closure")
            child = self.functions[closure.function_id]
            next_id = f"frame-{len(self.frames) + 1}-{self.executed_instructions}"
            self.frames.append(Frame(
                next_id, child, captures=dict(closure.captures),
                return_to=frame, return_dest=instruction["dest"],
            ))
            self.debugger._frames = self.frames
        elif operation == "return":
            value = frame.read(instruction["value"]) if "value" in instruction else None
            self._return(frame, value)
        elif operation == "throw":
            raise _Thrown(frame.read(instruction["value"]))
        elif operation == "nop":
            pass
        else:
            raise DebuggerError(f"unsupported proof operation {operation!r}")

    def _return(self, frame: Frame, value: Any) -> None:
        self.frames.pop()
        if frame.return_to is not None:
            frame.return_to.write(frame.return_dest, value)
        else:
            self.result = value
        self.debugger._frames = self.frames

    def collect(self) -> tuple[str, ...]:
        roots = [frame.locals for frame in self.frames] + [frame.captures for frame in self.frames]
        return self.heap.collect(roots)
