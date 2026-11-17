"""In-process DAP command translator over the backend-neutral DebuggerPort.

This is an adapter-boundary proof only: it is not a DAP transport/server.
"""
from __future__ import annotations

from dataclasses import fields, is_dataclass
from enum import Enum
from itertools import count
from typing import Any, Mapping

from debugger_api import DebuggerError, DebuggerPort


def _camel_case(name: str) -> str:
    parts = name.split("_")
    return parts[0] + "".join(part[:1].upper() + part[1:] for part in parts[1:])


def _wire(value: Any) -> Any:
    if is_dataclass(value):
        return {
            _camel_case(item.name): _wire(getattr(value, item.name))
            for item in fields(value)
            if getattr(value, item.name) is not None
        }
    if isinstance(value, Enum):
        return value.value
    if isinstance(value, Mapping):
        return {str(key): _wire(item) for key, item in value.items()}
    if isinstance(value, (tuple, list)):
        return [_wire(item) for item in value]
    return value


class DapAdapter:
    """Translate normalized DAP request commands to the debugger port."""

    def __init__(self, debugger: DebuggerPort) -> None:
        self._debugger = debugger
        self._frame_refs: dict[str, int] = {}
        self._frame_ids: dict[int, str] = {}
        self._scope_refs: dict[int, tuple[str, str]] = {}
        self._next_frame_id = count(1)
        self._next_scope_ref = count(1)
        self._next_breakpoint_id = count(1)

    def _frame_id(self, dap_frame_id: Any) -> str:
        if not isinstance(dap_frame_id, int) or isinstance(dap_frame_id, bool):
            raise DebuggerError("frameId must be an integer DAP reference")
        frame_id = self._frame_ids.get(dap_frame_id)
        if frame_id is None:
            raise DebuggerError("unknown or expired DAP frame reference")
        return frame_id

    def dispatch(self, request: Mapping[str, Any]) -> dict[str, Any]:
        if not isinstance(request, Mapping):
            return {"success": False, "command": None, "message": "request must be an object"}
        command = request.get("command")
        args = request.get("arguments", {})
        if not isinstance(command, str) or not command:
            return {"success": False, "command": command, "message": "command must be a non-empty string"}
        if not isinstance(args, Mapping):
            return {"success": False, "command": command, "message": "arguments must be an object"}

        try:
            if command == "initialize":
                capabilities = self._debugger.backend_capabilities
                body = {"capabilities": {
                    "supportsConfigurationDoneRequest": True,
                    "supportsFunctionBreakpoints": False,
                    "supportsConditionalBreakpoints": False,
                    "supportsHitConditionalBreakpoints": False,
                    "supportsLogPoints": False,
                    "supportsStepBack": False,
                    "supportsPauseRequest": capabilities.get("pause", False),
                    "supportsDelayedStackTraceLoading": False,
                    "supportsEvaluateForHovers": False,
                    "supportsExceptionInfoRequest": False,
                }}
            elif command == "configurationDone":
                body = {}
            elif command == "setBreakpoints":
                source_id = args.get("sourceId")
                requested = args.get("breakpoints")
                digest = args.get("sha256")
                if not isinstance(source_id, str) or not source_id:
                    raise DebuggerError("sourceId must be a non-empty string")
                if not isinstance(requested, list):
                    raise DebuggerError("breakpoints must be an array")
                if digest is not None and not isinstance(digest, str):
                    raise DebuggerError("sha256 must be a string")
                parsed = []
                for item in requested:
                    if not isinstance(item, Mapping):
                        raise DebuggerError("each breakpoint must be an object")
                    line, column = item.get("line"), item.get("column")
                    if not isinstance(line, int) or isinstance(line, bool) or line < 1:
                        raise DebuggerError("breakpoint line must be a positive integer")
                    if column is not None and (
                        not isinstance(column, int) or isinstance(column, bool) or column < 0
                    ):
                        raise DebuggerError("breakpoint column must be a non-negative integer")
                    parsed.append((line, column))

                # DAP setBreakpoints replaces the complete source-specific set.
                # Validate the whole request before mutating the active set, then
                # stage replacements so a failed backend command can be rolled back.
                previous = tuple(
                    item for item in self._debugger.list_breakpoints()
                    if item.source_id == source_id
                )
                created = []
                try:
                    for line, column in parsed:
                        created.append(self._debugger.set_breakpoint(
                            source_id, line, column, sha256=digest
                        ))
                except Exception:
                    for item in created:
                        self._debugger.remove_breakpoint(item.id)
                    raise

                try:
                    for item in previous:
                        if not self._debugger.remove_breakpoint(item.id):
                            raise DebuggerError("could not replace an existing source breakpoint")
                except Exception:
                    for item in created:
                        self._debugger.remove_breakpoint(item.id)
                    raise

                entries = []
                for bp in created:
                    entries.append({
                        "id": next(self._next_breakpoint_id),
                        "verified": bp.verified,
                        "line": bp.line,
                        **({"message": bp.message} if bp.message else {}),
                    })
                body = {"breakpoints": entries}
            elif command == "pause":
                self._debugger.request_pause()
                body = {}
            elif command in {"continue", "next", "stepIn", "stepOut"}:
                operation = {
                    "continue": self._debugger.resume,
                    "next": self._debugger.step_over,
                    "stepIn": self._debugger.step_into,
                    "stepOut": self._debugger.step_out,
                }[command]
                operation()
                body = {"allThreadsContinued": True}
            elif command == "threads":
                body = {"threads": [{"id": 1, "name": self._debugger.backend_name}]}
            elif command == "stackTrace":
                frames = []
                current_frames = self._debugger.get_stack()
                live_ids = {frame.id for frame in current_frames}
                for internal_id in tuple(self._frame_refs):
                    if internal_id not in live_ids:
                        dap_id = self._frame_refs.pop(internal_id)
                        self._frame_ids.pop(dap_id, None)
                        for ref, (frame_id, _scope_id) in tuple(self._scope_refs.items()):
                            if frame_id == internal_id:
                                self._scope_refs.pop(ref, None)
                for frame in current_frames:
                    dap_id = self._frame_refs.get(frame.id)
                    if dap_id is None:
                        dap_id = next(self._next_frame_id)
                        self._frame_refs[frame.id] = dap_id
                        self._frame_ids[dap_id] = frame.id
                    location = frame.location
                    frames.append({
                        "id": dap_id,
                        "name": frame.function_name,
                        "line": location.line if location else 0,
                        "column": location.column if location else 0,
                        "presentationHint": "normal",
                        "source": {"name": location.source_id} if location and location.source_id else None,
                    })
                body = {"stackFrames": frames}
            elif command == "scopes":
                internal_frame_id = self._frame_id(args.get("frameId"))
                for ref, (frame_id, _scope_id) in tuple(self._scope_refs.items()):
                    if frame_id == internal_frame_id:
                        self._scope_refs.pop(ref, None)
                scopes = []
                for scope in self._debugger.get_scopes(internal_frame_id):
                    variables_reference = next(self._next_scope_ref)
                    self._scope_refs[variables_reference] = (internal_frame_id, scope.id)
                    scopes.append({
                        "name": scope.name,
                        "variablesReference": variables_reference,
                        "expensive": False,
                    })
                body = {"scopes": scopes}
            elif command == "variables":
                variables_reference = args.get("variablesReference")
                if not isinstance(variables_reference, int) or isinstance(variables_reference, bool):
                    raise DebuggerError("variablesReference must be an integer DAP reference")
                target = self._scope_refs.get(variables_reference)
                if target is None:
                    raise DebuggerError("unknown or expired DAP variables reference")
                internal_frame_id, scope_id = target
                body = {"variables": [
                    {
                        "name": variable.name,
                        "value": variable.value.preview,
                        "type": variable.value.type,
                        "evaluateName": variable.name,
                        "presentationHint": {
                            "attributes": ["readOnly"] if not variable.mutable else []
                        },
                        "variablesReference": 0,
                    }
                    for variable in self._debugger.get_variables(internal_frame_id, scope_id)
                ]}
            elif command == "evaluate":
                expression = args.get("expression")
                if not isinstance(expression, str) or not expression.isidentifier():
                    raise DebuggerError("the M0 adapter supports identifier-only evaluation")
                internal_frame_id = self._frame_id(args.get("frameId"))
                body = {"result": _wire(
                    self._debugger.evaluate_identifier(internal_frame_id, expression)
                ), "variablesReference": 0}
            elif command == "disconnect":
                self._debugger.detach()
                self._frame_refs.clear()
                self._frame_ids.clear()
                self._scope_refs.clear()
                body = {}
            else:
                raise DebuggerError(f"unsupported DAP command {command!r}")
            return {"success": True, "command": command, "body": body}
        except (DebuggerError, KeyError, ValueError, TypeError) as exc:
            return {"success": False, "command": command, "message": str(exc)}
