#!/usr/bin/env python3
"""Deterministic persistent fake adapter; this process never executes JavaScript."""

from __future__ import annotations

import json
import os
import sys
import time


for raw_request in sys.stdin:
    request = json.loads(raw_request)
    test = request["test"]
    source = test["source"]
    response = {
        "protocol": "ogma-test-runner-adapter",
        "version": 1,
        "requestId": request["requestId"],
        "workerPid": os.getpid(),
    }

    if test.get("sourceType") == "module" and "import_FIXTURE.js" in source:
        fixture = os.path.join(test["testDirectory"], "import_FIXTURE.js")
        if not os.path.isfile(fixture):
            response.update({
                "outcome": "harness_error",
                "reason": f"module fixture was not resolvable from test directory: {fixture}",
            })
        else:
            response.update({"outcome": "completed"})
    elif test["suite"] == "test262-parser-tests":
        if "// parser: parsed" in source:
            response.update({"outcome": "parsed", "phase": "parse"})
        elif "// parser: rejected:early" in source:
            response.update({"outcome": "rejected", "phase": "early"})
        elif "// parser: rejected:parse" in source:
            response.update({"outcome": "rejected", "phase": "parse"})
        else:
            response.update({"outcome": "harness_error", "reason": "unknown parser fixture"})
    else:
        if "// result: timeout" in source:
            time.sleep((request["timeoutMs"] / 1000) + 0.2)
            response.update({"outcome": "completed"})
        elif "// result: expected-negative" in source:
            negative = test["negative"]
            response.update({
                "outcome": "threw",
                "phase": negative["phase"],
                "errorName": negative["type"],
            })
        elif "// result: missing-async-done" in source:
            response.update({"outcome": "completed"})
        elif "// result: async-failure" in source:
            response.update({
                "outcome": "completed",
                "asyncSignal": "Test262:AsyncTestFailure: fixture failure",
            })
        elif "// result: pass" in source:
            response.update({
                "outcome": "completed",
                "modeEcho": test["mode"],
                "includeNames": [item["name"] for item in test["includes"]],
            })
            if test["async"]:
                response["asyncSignal"] = "Test262:AsyncTestComplete"
        else:
            response.update({
                "outcome": "threw",
                "phase": "runtime",
                "errorName": "ReferenceError",
            })

    print(json.dumps(response), flush=True)
