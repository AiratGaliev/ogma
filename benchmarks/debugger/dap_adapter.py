"""In-process DAP command translator over the backend-neutral DebuggerPort.

This is an adapter-boundary proof only: it is not a DAP transport/server.
"""
from __future__ import annotations

from dataclasses import fields, is_dataclass
from enum import Enum
from typing import Any, Mapping

from debugger_api import DebuggerError, DebuggerPort


def _wire(value: Any) -> Any:
    if is_dataclass(value):
        return {
            item.name: _wire(getattr(value, item.name))
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
                entries = []
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
                    bp = self._debugger.set_breakpoint(
                        source_id, line, column, sha256=digest
                    )
                    entries.append({
                        "id": int(bp.id.rsplit("-", 1)[1]),
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
                for index, frame in enumerate(self._debugger.get_stack()):
                    location = frame.location
                    frames.append({
                        "id": index + 1,
                        "name": frame.function_name,
                        "line": location.line if location else 0,
                        "column": location.column if location else 0,
                        "presentationHint": "normal",
                        "source": {"name": location.source_id} if location and location.source_id else None,
                    })
                body = {"stackFrames": frames}
            elif command == "scopes":
                body = {"scopes": [
                    {
                        "name": scope.name,
                        "variablesReference": scope.id,
                        "expensive": False,
                    }
                    for scope in self._debugger.get_scopes(args["frameId"])
                ]}
            elif command == "variables":
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
                    for variable in self._debugger.get_variables(args["frameId"], args["scopeId"])
                ]}
            elif command == "evaluate":
                expression = args.get("expression")
                if not isinstance(expression, str) or not expression.isidentifier():
                    raise DebuggerError("the M0 adapter supports identifier-only evaluation")
                body = {"result": _wire(
                    self._debugger.evaluate_identifier(args["frameId"], expression)
                ), "variablesReference": 0}
            elif command == "disconnect":
                self._debugger.detach()
                body = {}
            else:
                raise DebuggerError(f"unsupported DAP command {command!r}")
            return {"success": True, "command": command, "body": body}
        except (DebuggerError, KeyError, ValueError, TypeError) as exc:
            return {"success": False, "command": command, "message": str(exc)}
