"""Backend-neutral, immutable debugger contract for Ogma tooling."""

from __future__ import annotations

from dataclasses import dataclass
from types import MappingProxyType
from typing import Any, Mapping, Protocol, runtime_checkable


def _freeze_snapshot(value: Any) -> Any:
    """Recursively freeze nested snapshot data before it crosses the port."""
    if isinstance(value, Mapping):
        return MappingProxyType({key: _freeze_snapshot(item) for key, item in value.items()})
    if isinstance(value, (list, tuple)):
        return tuple(_freeze_snapshot(item) for item in value)
    if isinstance(value, set):
        return frozenset(_freeze_snapshot(item) for item in value)
    return value


class DebuggerError(RuntimeError):
    """A rejected debugger command or an unavailable inspection result."""


@dataclass(frozen=True)
class CodeGeneration:
    id: str
    version: str


@dataclass(frozen=True)
class SourcePosition:
    line: int
    column: int


@dataclass(frozen=True)
class SourceLocation:
    origin: str
    source_id: str | None = None
    source_sha256: str | None = None
    start: SourcePosition | None = None
    end: SourcePosition | None = None
    reason: str | None = None
    scope_id: str | None = None
    binding_ids: tuple[str, ...] = ()
    inline_chain: tuple[Mapping[str, Any], ...] = ()

    def __post_init__(self) -> None:
        object.__setattr__(self, "binding_ids", tuple(self.binding_ids))
        object.__setattr__(
            self, "inline_chain", tuple(_freeze_snapshot(item) for item in self.inline_chain)
        )

    @property
    def line(self) -> int | None:
        return self.start.line if self.start is not None else None

    @property
    def column(self) -> int | None:
        return self.start.column if self.start is not None else None


@dataclass(frozen=True)
class Breakpoint:
    id: str
    source_id: str
    source_sha256: str | None
    code_generation: CodeGeneration | None
    line: int
    column: int | None
    verified: bool
    message: str | None = None


@dataclass(frozen=True)
class DebugValue:
    type: str
    preview: str
    object_id: str | None = None
    availability: str = "available"
    truncated: bool = False


@dataclass(frozen=True)
class VariableSnapshot:
    binding_id: str
    name: str
    mutable: bool
    availability: str
    value: DebugValue


@dataclass(frozen=True)
class ScopeSnapshot:
    id: str
    name: str
    parent_id: str | None
    variables: tuple[VariableSnapshot, ...]

    def __post_init__(self) -> None:
        object.__setattr__(self, "variables", tuple(self.variables))


@dataclass(frozen=True)
class StackFrameSnapshot:
    id: str
    function_id: str
    function_name: str
    code_generation: CodeGeneration
    location: SourceLocation | None
    scope_ids: tuple[str, ...]
    backend: str

    def __post_init__(self) -> None:
        object.__setattr__(self, "scope_ids", tuple(self.scope_ids))


@dataclass(frozen=True)
class StoppedEvent:
    reason: str
    frame_id: str
    location: SourceLocation | None
    description: str | None = None


@dataclass(frozen=True)
class ExceptionEvent:
    value: DebugValue
    frame_id: str
    location: SourceLocation | None
    caught: bool = False


@dataclass(frozen=True)
class InspectedProperty:
    name: str
    value: DebugValue
    enumerable: bool
    writable: bool
    configurable: bool
    is_accessor: bool = False


@dataclass(frozen=True)
class InspectedObject:
    object_id: str
    is_proxy: bool
    properties: tuple[InspectedProperty, ...]
    truncated: bool = False

    def __post_init__(self) -> None:
        object.__setattr__(self, "properties", tuple(self.properties))


@runtime_checkable
class DebuggerPort(Protocol):
    """Stable command port implemented by each execution backend.

    Implementations may expose fewer capabilities; callers must consult the
    capability map before advertising unsupported operations to an IDE.
    No VM frame, register array, heap layout or mutable runtime object escapes
    through this interface.
    """

    @property
    def backend_name(self) -> str: ...

    @property
    def backend_capabilities(self) -> Mapping[str, bool]: ...

    @property
    def session_id(self) -> str: ...

    def set_breakpoint(
        self,
        source_id: str,
        line: int,
        column: int | None = None,
        *,
        sha256: str | None = None,
        code_generation: CodeGeneration | None = None,
    ) -> Breakpoint: ...

    def remove_breakpoint(self, breakpoint_id: str) -> bool: ...

    def list_breakpoints(self) -> tuple[Breakpoint, ...]: ...

    def request_pause(self) -> None: ...

    def resume(self) -> None: ...

    def step_into(self) -> None: ...

    def step_over(self) -> None: ...

    def step_out(self) -> None: ...

    def get_stack(self) -> tuple[StackFrameSnapshot, ...]: ...

    def get_scopes(self, frame_id: str) -> tuple[ScopeSnapshot, ...]: ...

    def get_variables(self, frame_id: str, scope_id: str) -> tuple[VariableSnapshot, ...]: ...

    def evaluate_identifier(self, frame_id: str, identifier: str) -> DebugValue: ...

    def inspect_object(self, object_id: str, *, limit: int = 128) -> InspectedObject: ...

    def event_message(self) -> StoppedEvent | ExceptionEvent | None: ...

    def detach(self) -> None: ...
