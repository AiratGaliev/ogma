"""Regression tests for the OGM-33 debugger API and M0 proof harness."""

from __future__ import annotations

import copy
import json
from pathlib import Path
import sys
import threading
import unittest

ROOT = Path(__file__).resolve().parents[2]
DEBUGGER_DIR = ROOT / "benchmarks" / "debugger"
SCRIPTS_DIR = ROOT / ".github" / "scripts"
sys.path.insert(0, str(DEBUGGER_DIR))
sys.path.insert(0, str(SCRIPTS_DIR))

from debugger_api import CodeGeneration, DebuggerError, DebuggerPort
from dap_adapter import DapAdapter
from metadata import lower_core_ir_debug_metadata, strip_debug_metadata
from prototype import Debugger, Heap, Interpreter, PropertyDescriptor, debug_value
from verify_ogmb import validate_source_map


SOURCE_SHA = "a" * 64
GENERATION = {"id": "fixture_codegen", "version": "1"}
GENERATION_OBJECT = CodeGeneration(**GENERATION)


def user_source(line: int, column: int = 0, bindings: tuple[str, ...] = (), scope: str = "env_main"):
    return {
        "origin": "user",
        "file": "src_main",
        "start": {"line": line, "column": column},
        "end": {"line": line, "column": column + 4},
        "scope": scope,
        "bindings": list(bindings),
        "suspension": None,
        "inline_chain": [],
    }


def function_debug_map(sources):
    return {
        "source_map": {
            "version": "1.0.0",
            "files": [{
                "id": "src_main",
                "path": "tests/debugger/fixtures/interpreter.js",
                "language": "javascript",
                "sha256": SOURCE_SHA,
            }],
            "locations": [
                {"offset": offset, "source": source}
                for offset, source in enumerate(sources)
            ],
            "lexical_scopes": [],
            "variable_locations": [],
        }
    }


def make_image():
    main = {
        "id": "fn_main",
        "name": "main",
        "environment": {
            "id": "env_main",
            "bindings": [
                {"id": "total", "name": "total", "mutable": True},
                {"id": "obj", "name": "obj", "mutable": False},
                {"id": "updater", "name": "updater", "mutable": False},
                {"id": "result", "name": "result", "mutable": False},
            ],
        },
        "captures": [],
        "instructions": [
            {"op": "const", "dest": "total", "value": 10},
            {
                "op": "object",
                "dest": "obj",
                "properties": {
                    "value": PropertyDescriptor(value=42),
                    "accessor": PropertyDescriptor(getter_present=True),
                },
            },
            {
                "op": "closure",
                "dest": "updater",
                "function": "fn_update",
                "captures": {"capture_total": "total"},
            },
            {"op": "call", "callee": "updater", "dest": "result"},
            {"op": "return", "value": "result"},
        ],
        "debug_metadata": function_debug_map([
            user_source(1, 0, ("total",)),
            user_source(2, 0, ("obj",)),
            user_source(3, 0, ("updater", "total")),
            user_source(4, 2, ("updater",)),
            user_source(8, 0, ("result",)),
        ]),
    }
    update = {
        "id": "fn_update",
        "name": "update",
        "environment": {
            "id": "env_update",
            "bindings": [
                {"id": "increment", "name": "increment", "mutable": False},
                {"id": "old_total", "name": "oldTotal", "mutable": False},
                {"id": "next", "name": "next", "mutable": False},
            ],
        },
        "captures": [
            {
                "id": "capture_total",
                "name": "total",
                "mutable": True,
                "source_environment": "env_main",
            },
        ],
        "instructions": [
            {"op": "const", "dest": "increment", "value": 5},
            {"op": "load", "dest": "old_total", "binding": "capture_total"},
            {"op": "add", "dest": "next", "left": "old_total", "right": "increment"},
            {"op": "store", "binding": "capture_total", "src": "next"},
            {"op": "return", "value": "next"},
        ],
        "debug_metadata": function_debug_map([
            user_source(4, 8, ("increment",), "env_update"),
            user_source(4, 8, ("capture_total", "old_total"), "env_update"),
            user_source(5, 4, ("old_total", "increment", "next"), "env_update"),
            user_source(6, 4, ("capture_total", "next"), "env_update"),
            user_source(7, 4, ("next",), "env_update"),
        ]),
    }
    return {
        "entry": "fn_main",
        "code_generation": GENERATION,
        "source_files": [{
            "id": "src_main",
            "path": "tests/debugger/fixtures/interpreter.js",
            "language": "javascript",
            "sha256": SOURCE_SHA,
        }],
        "functions": [main, update],
    }


def paused_at_call():
    image = make_image()
    debugger = Debugger(image)
    interpreter = Interpreter(image, debugger)
    breakpoint = debugger.set_breakpoint(
        "src_main", 4, 2, sha256=SOURCE_SHA, code_generation=GENERATION_OBJECT
    )
    result = interpreter.run()
    if result.status != "paused":
        raise AssertionError(f"expected a breakpoint pause, got {result.status}")
    return image, debugger, interpreter, breakpoint


class DebuggerApiTests(unittest.TestCase):
    def test_debugger_implements_backend_neutral_port(self):
        debugger = Debugger(make_image())
        self.assertIsInstance(debugger, DebuggerPort)
        self.assertEqual(debugger.backend_name, "interpreter")
        self.assertTrue(debugger.backend_capabilities["stepInto"])
        self.assertFalse(debugger.backend_capabilities["safeInspection"])

    def test_source_breakpoint_is_bound_to_digest_and_generation(self):
        image, debugger, interpreter, breakpoint = paused_at_call()
        self.assertTrue(breakpoint.verified)
        self.assertEqual(debugger.event_message().reason, "breakpoint")
        self.assertEqual(debugger.get_stack()[0].location.line, 4)
        self.assertEqual(debugger.get_stack()[0].location.column, 2)

        stale = debugger.set_breakpoint(
            "src_main", 4, 2, sha256="b" * 64, code_generation=GENERATION_OBJECT
        )
        synthetic = debugger.set_breakpoint("src_main", 99)
        wrong_generation = debugger.set_breakpoint(
            "src_main", 4, 2, sha256=SOURCE_SHA,
            code_generation=CodeGeneration("old-generation", "1"),
        )
        self.assertFalse(stale.verified)
        self.assertFalse(synthetic.verified)
        self.assertFalse(wrong_generation.verified)
        self.assertTrue(debugger.remove_breakpoint(breakpoint.id))

    def test_step_into_exposes_child_and_preserves_caller_call_site(self):
        _, debugger, interpreter, _ = paused_at_call()
        debugger.step_into()
        result = interpreter.run()
        self.assertEqual(result.status, "paused")
        stack = debugger.get_stack()
        self.assertEqual([frame.function_name for frame in stack], ["update", "main"])
        self.assertEqual(stack[0].location.line, 4)
        self.assertEqual(stack[0].location.column, 8)
        # The suspended caller stays at the actual call expression.
        self.assertEqual(stack[1].location.line, 4)
        self.assertEqual(stack[1].location.column, 2)

        scope = debugger.get_scopes(stack[0].id)[0]
        capture = next(item for item in scope.variables if item.binding_id == "capture_total")
        self.assertEqual(capture.value.preview, "10")
        self.assertEqual(capture.availability, "available")
        optimized = next(item for item in scope.variables if item.binding_id == "next")
        self.assertEqual(optimized.availability, "optimized_out")

    def test_step_over_runs_callee_and_stops_at_next_caller_statement(self):
        _, debugger, interpreter, _ = paused_at_call()
        debugger.step_over()
        result = interpreter.run()
        self.assertEqual(result.status, "paused")
        stack = debugger.get_stack()
        self.assertEqual(len(stack), 1)
        self.assertEqual(stack[0].function_name, "main")
        self.assertEqual(stack[0].location.line, 8)
        self.assertEqual(interpreter.result, None)

    def test_step_out_returns_to_the_calling_frame(self):
        _, debugger, interpreter, _ = paused_at_call()
        debugger.step_into()
        self.assertEqual(interpreter.run().status, "paused")
        debugger.step_out()
        result = interpreter.run()
        self.assertEqual(result.status, "paused")
        stack = debugger.get_stack()
        self.assertEqual(len(stack), 1)
        self.assertEqual(stack[0].function_name, "main")
        self.assertEqual(stack[0].location.line, 8)

    def test_pause_request_from_control_thread_is_applied_at_owner_safepoint(self):
        image = make_image()
        debugger = Debugger(image)
        interpreter = Interpreter(image, debugger)
        thread = threading.Thread(target=debugger.request_pause)
        thread.start()
        thread.join()
        result = interpreter.run()
        self.assertEqual(result.status, "paused")
        self.assertEqual(debugger.event_message().reason, "pause")
        self.assertEqual(debugger.event_message().location.line, 1)

    def test_variable_snapshots_and_identifier_evaluation_are_immutable_values(self):
        _, debugger, _, _ = paused_at_call()
        frame = debugger.get_stack()[0]
        value = debugger.evaluate_identifier(frame.id, "total")
        self.assertEqual(value.type, "number")
        self.assertEqual(value.preview, "10")
        with self.assertRaises((AttributeError, TypeError)):
            value.preview = "changed"

    def test_safe_inspection_does_not_call_accessors_and_releases_temporary_root(self):
        _, debugger, interpreter, _ = paused_at_call()
        frame = debugger.get_stack()[0]
        object_id = debugger.evaluate_identifier(frame.id, "obj").object_id
        interpreter.frames[0].locals.pop("obj")
        interpreter.heap.before_inspection = lambda: interpreter.heap.collect()
        inspected = debugger.inspect_object(object_id)
        self.assertFalse(inspected.is_proxy)
        self.assertEqual([item.name for item in inspected.properties], ["value", "accessor"])
        accessor = next(item for item in inspected.properties if item.name == "accessor")
        self.assertTrue(accessor.is_accessor)
        self.assertEqual(accessor.value.availability, "not_evaluated")
        self.assertEqual(interpreter.heap._temporary_roots, {})
        interpreter.heap.before_inspection = None
        self.assertEqual(interpreter.heap.collect(), (object_id,))

    def test_proxy_inspection_never_queries_proxy_traps(self):
        image, debugger, interpreter, _ = paused_at_call()
        proxy = interpreter.heap.allocate({"must_not_read": PropertyDescriptor(value=9)}, is_proxy=True)
        result = debugger.inspect_object(proxy.object_id)
        self.assertTrue(result.is_proxy)
        self.assertEqual(result.properties, ())

    def test_object_id_strings_are_not_gc_roots_and_unsafe_values_are_not_stringified(self):
        heap = Heap()
        reference = heap.allocate({"x": 1})
        self.assertEqual(heap.collect(["heap-1"]), (reference.object_id,))

        class HostObject:
            def __repr__(self):
                raise AssertionError("user repr must not be called")

            def __str__(self):
                raise AssertionError("user str must not be called")

        value = debug_value(HostObject())
        self.assertEqual(value.availability, "available")
        self.assertEqual(value.preview, "<not inspectable>")

    def test_owner_thread_restriction_and_detach_lifecycle(self):
        _, debugger, _, breakpoint = paused_at_call()
        failures = []

        def inspect_on_other_thread():
            try:
                debugger.get_stack()
            except Exception as exc:
                failures.append(exc)

        thread = threading.Thread(target=inspect_on_other_thread)
        thread.start()
        thread.join()
        self.assertEqual(len(failures), 1)
        self.assertIsInstance(failures[0], DebuggerError)

        debugger.detach()
        self.assertEqual(debugger.state, "detached")
        self.assertFalse(debugger.list_breakpoints())
        with self.assertRaises(DebuggerError):
            debugger.resume()

    def test_dap_adapter_uses_only_the_facade_and_advertises_actual_capabilities(self):
        _, debugger, _, _ = paused_at_call()
        adapter = DapAdapter(debugger)
        initialized = adapter.dispatch({"command": "initialize"})
        self.assertTrue(initialized["success"])
        self.assertTrue(initialized["body"]["capabilities"]["supportsPauseRequest"])
        self.assertFalse(initialized["body"]["capabilities"]["supportsConditionalBreakpoints"])
        stack = adapter.dispatch({"command": "stackTrace"})
        self.assertTrue(stack["success"])
        self.assertEqual(stack["body"]["stackFrames"][0]["name"], "main")
        invalid = adapter.dispatch({
            "command": "setBreakpoints",
            "arguments": {"sourceId": "src_main", "breakpoints": [{"line": True}]},
        })
        self.assertFalse(invalid["success"])

    def test_uncaught_exception_is_reported_as_structured_event(self):
        image = make_image()
        main = image["functions"][0]
        main["instructions"] = [
            {"op": "const", "dest": "error", "value": "boom"},
            {"op": "throw", "value": "error"},
        ]
        main["environment"]["bindings"].append(
            {"id": "error", "name": "error", "mutable": False}
        )
        main["debug_metadata"] = function_debug_map([
            user_source(1, bindings=("error",)),
            user_source(2, bindings=("error",)),
        ])
        debugger = Debugger(image)
        interpreter = Interpreter(image, debugger)
        result = interpreter.run()
        self.assertEqual(result.status, "exception")
        self.assertEqual(result.event.value.preview, "boom")
        self.assertFalse(result.event.caught)
        self.assertEqual(result.event.location.line, 2)


class SourceMetadataTests(unittest.TestCase):
    def setUp(self):
        self.core_ir = json.loads(
            (ROOT / "tests" / "core-ir" / "fixtures" / "positive-closure-source.json").read_text(encoding="utf-8")
        )
        self.function = self.core_ir["function"]
        self.offsets = {
            "make_closure": 0,
            "load_total": 1,
            "store_seed": 2,
            "terminator:entry": 3,
        }
        self.variables = [
            {"binding": "bind_param", "kind": "local", "index": 0, "start_offset": 0, "end_offset": 4},
            {"binding": "capture_total", "kind": "capture", "index": 0, "start_offset": 0, "end_offset": 4},
        ]

    def lower(self, variables=None):
        return lower_core_ir_debug_metadata(
            self.core_ir, self.offsets, 4, GENERATION_OBJECT, [0, 1, 2, 3, 4],
            self.variables if variables is None else variables,
        )

    def test_core_ir_source_metadata_round_trips_to_ogmb_v1(self):
        result = self.lower()
        source_map = result["source_map"]
        self.assertEqual(result["ir_to_bytecode"], self.offsets)
        self.assertEqual([item["offset"] for item in source_map["locations"]], [0, 1, 2, 3])
        self.assertEqual(source_map["lexical_scopes"][0]["bindings"], ["bind_param", "capture_total"])

        portable_metadata = {
            "environment": {
                "bindings": self.function["environment"]["bindings"],
                "captures": self.function["captures"],
                "locations": [{"binding": "bind_param", "kind": "local", "index": 0}],
            },
            "suspension_points": [],
        }
        self.assertEqual(validate_source_map(source_map, portable_metadata, 4), [])

    def test_capture_slot_must_match_manifest_capture_descriptor_index(self):
        invalid = copy.deepcopy(self.variables)
        invalid[1]["index"] = 1
        with self.assertRaisesRegex(DebuggerError, "capture storage index"):
            self.lower(invalid)

    def test_ir_to_bytecode_mapping_must_be_complete_and_instruction_aligned(self):
        missing = dict(self.offsets)
        missing.pop("terminator:entry")
        with self.assertRaisesRegex(DebuggerError, "map mismatch"):
            lower_core_ir_debug_metadata(
                self.core_ir, missing, 4, GENERATION_OBJECT, [0, 1, 2, 3, 4], self.variables
            )
        misaligned = dict(self.offsets)
        misaligned["load_total"] = 9
        with self.assertRaisesRegex(DebuggerError, "instruction boundary"):
            lower_core_ir_debug_metadata(
                self.core_ir, misaligned, 4, GENERATION_OBJECT, [0, 1, 2, 3, 4], self.variables
            )

    def test_binding_and_capture_ids_cannot_collide(self):
        invalid = copy.deepcopy(self.core_ir)
        invalid["function"]["captures"][0]["id"] = "bind_param"
        with self.assertRaisesRegex(DebuggerError, "IDs must be globally unique"):
            lower_core_ir_debug_metadata(
                invalid, self.offsets, 4, GENERATION_OBJECT, [0, 1, 2, 3, 4], self.variables
            )

    def test_release_stripping_removes_only_source_debug_metadata(self):
        image = make_image()
        stripped = strip_debug_metadata(image)
        self.assertNotIn("source_files", stripped)
        self.assertNotIn("debug_metadata", stripped)
        self.assertEqual(stripped["entry"], image["entry"])
        self.assertEqual(stripped["code_generation"], image["code_generation"])
        for before, after in zip(image["functions"], stripped["functions"]):
            self.assertNotIn("debug_metadata", after)
            self.assertEqual(after["instructions"], before["instructions"])
            self.assertEqual(after["environment"], before["environment"])
            self.assertEqual(after["captures"], before["captures"])


if __name__ == "__main__":
    unittest.main()
