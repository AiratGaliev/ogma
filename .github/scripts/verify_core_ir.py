#!/usr/bin/env python3
"""Dependency-free structural and semantic verifier for Ogma Core IR v1."""
from __future__ import annotations

import argparse
import json
import math
import re
import sys
from pathlib import Path, PurePosixPath

CONTRACT = "ogma.core-ir"
VERSION = "1.0.0"
EFFECTS = ("may_allocate", "may_gc", "may_throw", "may_suspend", "may_call_host", "may_reenter", "may_mutate_shape")
ID = re.compile(r"^[A-Za-z_][A-Za-z0-9_.:-]{0,127}$")
REPS = {
    "dynamic": {"boxed"}, "undefined": {"boxed"}, "null": {"boxed"},
    "boolean": {"boxed", "i1"}, "number": {"boxed", "i32", "u32", "f64"},
    "string": {"boxed", "string_ref"}, "symbol": {"boxed", "symbol_ref"},
    "bigint": {"boxed", "bigint_ref"}, "object": {"boxed", "object_ref"},
    "array": {"boxed", "array_ref", "object_ref"},
    "function": {"boxed", "function_ref", "object_ref"},
    "exception": {"boxed", "exception_ref"},
}
DYNAMIC = {"may_allocate", "may_gc", "may_throw", "may_call_host", "may_reenter", "may_mutate_shape"}
OPS = {
    "js.constant": (0, 0, "literal", {"literal", "value"}, set()),
    "js.add": (2, 2, "dynamic", set(), DYNAMIC),
    "js.get_property": (1, 1, "dynamic", {"key"}, DYNAMIC),
    "js.set_property": (2, 2, "operand:1", {"key"}, DYNAMIC),
    "js.get_element": (2, 2, "dynamic", set(), DYNAMIC),
    "js.set_element": (3, 3, "operand:2", set(), DYNAMIC),
    "js.call": (2, None, "dynamic", set(), DYNAMIC),
    "js.construct": (2, None, "dynamic", set(), DYNAMIC),
    "js.to_boolean": (1, 1, "boolean", set(), set()),
    "js.to_number": (1, 1, "number", set(), DYNAMIC),
    "js.to_string": (1, 1, "string", set(), DYNAMIC),
    "js.strict_equal": (2, 2, "boolean", set(), set()),
    "js.typeof": (1, 1, "string", set(), set()),
    "env.load": (0, 0, "binding", {"binding"}, {"may_throw"}),
    "env.store": (1, 1, "binding", {"binding"}, {"may_throw"}),
    "closure.create": (0, 0, "function", {"function", "captures"}, {"may_allocate", "may_gc"}),
    "host.call": (0, None, "dynamic", {"symbol"}, {"may_allocate", "may_gc", "may_throw", "may_call_host", "may_reenter", "may_mutate_shape"}),
    "async.await": (1, 1, "dynamic", set(), {"may_allocate", "may_gc", "may_throw", "may_suspend"}),
    "js.throw": (1, 1, None, set(), {"may_throw"}),
}

class DuplicateKey(ValueError):
    pass

def _pairs(pairs):
    out = {}
    for key, value in pairs:
        if key in out:
            raise DuplicateKey(f"duplicate JSON object key: {key}")
        out[key] = value
    return out

def _constant(value):
    raise ValueError(f"non-finite JSON number is not permitted: {value}")

def load(path: Path):
    return json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=_pairs, parse_constant=_constant)

def schema_check(value, rule, defs, path="$", errors=None):
    errors = [] if errors is None else errors
    ref = rule.get("$ref")
    if ref:
        name = ref.removeprefix("#/$defs/")
        if not ref.startswith("#/$defs/") or name not in defs:
            errors.append(("schema.invalid_ref", path, f"unsupported schema reference {ref}"))
            return errors
        schema_check(value, defs[name], defs, path, errors)
    for keyword in ("allOf", "anyOf", "oneOf"):
        if keyword in rule:
            results = [schema_check(value, branch, defs, path, []) for branch in rule[keyword]]
            matches = sum(not result for result in results)
            valid = matches == len(results) if keyword == "allOf" else matches >= 1 if keyword == "anyOf" else matches == 1
            if not valid:
                errors.append((f"schema.{keyword}", path, f"value does not satisfy {keyword}"))
    if "const" in rule and value != rule["const"]:
        errors.append(("schema.const", path, f"expected {rule['const']!r}"))
    if "enum" in rule and value not in rule["enum"]:
        errors.append(("schema.enum", path, "value is not in the permitted set"))
    typ = rule.get("type")
    if typ is not None:
        expected = typ if isinstance(typ, list) else [typ]
        def match(t):
            return {"object": lambda: isinstance(value, dict), "array": lambda: isinstance(value, list),
                    "string": lambda: isinstance(value, str), "boolean": lambda: isinstance(value, bool),
                    "integer": lambda: isinstance(value, int) and not isinstance(value, bool),
                    "number": lambda: isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value),
                    "null": lambda: value is None}[t]()
        if not any(match(t) for t in expected):
            errors.append(("schema.type", path, f"expected {' or '.join(expected)}"))
            return errors
    if isinstance(value, dict):
        for key in rule.get("required", []):
            if key not in value:
                errors.append(("schema.required", path, f"missing required property {key!r}"))
        props = rule.get("properties", {})
        for key, child in value.items():
            child_rule = props.get(key)
            if child_rule is None:
                additional = rule.get("additionalProperties", True)
                if additional is False:
                    errors.append(("schema.additional_property", f"{path}.{key}", "property is not allowed"))
                elif isinstance(additional, dict):
                    schema_check(child, additional, defs, f"{path}.{key}", errors)
            else:
                schema_check(child, child_rule, defs, f"{path}.{key}", errors)
        if len(value) < rule.get("minProperties", 0): errors.append(("schema.min_properties", path, "too few properties"))
        if len(value) > rule.get("maxProperties", float("inf")): errors.append(("schema.max_properties", path, "too many properties"))
    elif isinstance(value, list):
        if len(value) < rule.get("minItems", 0): errors.append(("schema.min_items", path, "too few items"))
        if len(value) > rule.get("maxItems", float("inf")): errors.append(("schema.max_items", path, "too many items"))
        if "items" in rule:
            for i, child in enumerate(value): schema_check(child, rule["items"], defs, f"{path}[{i}]", errors)
    elif isinstance(value, str):
        if len(value) < rule.get("minLength", 0): errors.append(("schema.min_length", path, "string is too short"))
        if len(value) > rule.get("maxLength", float("inf")): errors.append(("schema.max_length", path, "string is too long"))
        if "pattern" in rule and re.search(rule["pattern"], value) is None: errors.append(("schema.pattern", path, "string does not match required pattern"))
    elif isinstance(value, (int, float)) and not isinstance(value, bool):
        if value < rule.get("minimum", float("-inf")): errors.append(("schema.minimum", path, "number is below minimum"))
    return errors

class Verifier:
    def __init__(self, schema):
        self.schema, self.errors = schema, []
    def err(self, code, path, message): self.errors.append((code, path, message))
    def valid_id(self, value): return isinstance(value, str) and ID.fullmatch(value) is not None
    def rep(self, item, path):
        st, rep = item.get("semantic_type"), item.get("representation")
        if rep not in REPS.get(st, set()): self.err("representation.unsupported", path, f"{rep!r} is not valid for {st!r}")
    def verify(self, doc):
        self.errors = schema_check(doc, self.schema, self.schema.get("$defs", {}))
        if self.errors: return self.errors
        if doc["contract"] != CONTRACT or doc["version"] != VERSION:
            self.err("contract.version", "$.version", f"expected {CONTRACT} {VERSION}"); return self.errors
        f = doc["function"]
        if not self.valid_id(f["id"]): self.err("id.invalid", "$.function.id", "invalid function identity")
        files, env = {}, f["environment"]
        for i, source_file in enumerate(f["source_files"]):
            p = f"$.function.source_files[{i}]"; sid, name = source_file["id"], source_file["path"]
            if sid in files: self.err("source.duplicate_file", p, "duplicate source file id")
            files[sid] = source_file
            posix = PurePosixPath(name)
            if posix.is_absolute() or ".." in posix.parts or "\\" in name or posix.as_posix() != name or name == ".":
                self.err("source.path", p + ".path", "source path must be normalized repository-relative POSIX")
        bindings = {}
        for i, b in enumerate(env["bindings"]):
            if b["id"] in bindings: self.err("environment.duplicate_binding", f"$.function.environment.bindings[{i}]", "duplicate binding id")
            bindings[b["id"]] = b; self.rep(b, f"$.function.environment.bindings[{i}]")
            if b["kind"] in {"const", "import"} and b["mutable"]: self.err("environment.binding_mutability", f"$.function.environment.bindings[{i}]", "const/import bindings cannot be mutable")
        captures = {}
        for i, c in enumerate(f["captures"]):
            p = f"$.function.captures[{i}]"; captures[c["id"]] = c; captures[c["binding"]] = c; self.rep(c, p)
            if c["source_environment"] == env["id"]: self.err("closure.capture_origin", p, "captured binding must originate outside current environment")
            if c["mutable"] and c["mode"] != "cell": self.err("closure.capture_mode", p, "mutable captures must use cell mode")
        params = {}
        for i, p in enumerate(f["parameters"]):
            params[p["binding"]] = p; self.rep(p, f"$.function.parameters[{i}]")
            b = bindings.get(p["binding"])
            if not b or b["kind"] != "parameter" or b["name"] != p["name"] or (b["semantic_type"], b["representation"]) != (p["semantic_type"], p["representation"]):
                self.err("function.parameter_binding", f"$.function.parameters[{i}]", "parameter must match a parameter lexical binding")
        if f["return"]: self.rep(f["return"], "$.function.return")
        blocks = {b["id"]: b for b in f["blocks"]}; order = [b["id"] for b in f["blocks"]]
        if len(blocks) != len(order): self.err("cfg.duplicate_block", "$.function.blocks", "block ids must be unique")
        defs, instr_ids = {}, set()
        def define(item, block, index, kind, path):
            ident = item["id"]
            if not self.valid_id(ident): self.err("id.invalid", path, "invalid SSA identifier")
            if ident in defs: self.err("ssa.duplicate_definition", path, f"duplicate SSA definition {ident!r}")
            else: defs[ident] = (block, index, item, kind)
        for i, p in enumerate(f["parameters"]): define(p, None, -1, "value", f"$.function.parameters[{i}]")
        for bi, b in enumerate(f["blocks"]):
            bp = f"$.function.blocks[{bi}]"
            if b["exception_parameter"]:
                ep = b["exception_parameter"]; self.rep(ep, bp+".exception_parameter")
                if (ep["semantic_type"], ep["representation"]) != ("exception", "exception_ref"): self.err("exception.parameter_type", bp, "handler exception parameter must be exception/exception_ref")
                define(ep, b["id"], -1, "exception", bp+".exception_parameter")
            for pi, p in enumerate(b["parameters"]):
                if p["kind"] == "value": self.rep(p, f"{bp}.parameters[{pi}]")
                define(p, b["id"], -1, p["kind"], f"{bp}.parameters[{pi}]")
            for ii, ins in enumerate(b["instructions"]):
                ip = f"{bp}.instructions[{ii}]"
                if ins["id"] in instr_ids: self.err("instruction.duplicate_id", ip+".id", "instruction ids must be function-unique")
                instr_ids.add(ins["id"])
                if ins["result"]:
                    self.rep(ins["result"], ip+".result"); define(ins["result"], b["id"], ii, "value", ip+".result")
                if ins["effect_out"]: define({"id": ins["effect_out"]}, b["id"], ii, "effect", ip+".effect_out")
        # Build normal and exceptional CFG edges before checking dominance.
        preds = {bid: set() for bid in blocks}; normal_preds = {bid: set() for bid in blocks}; succ = {bid: set() for bid in blocks}; exc_preds = {bid: set() for bid in blocks}
        def term_edges(t):
            if t["kind"] == "jump": return [t]
            if t["kind"] == "branch": return [t["then"], t["else"]]
            return []
        for b in f["blocks"]:
            for e in term_edges(b["terminator"]):
                target=e["target"]
                if target not in blocks: self.err("cfg.edge_target", "$.function.blocks", f"normal edge targets unknown block {target!r}"); continue
                preds[target].add(b["id"]); normal_preds[target].add(b["id"]); succ[b["id"]].add(target)
            for ins in b["instructions"]:
                e=ins["exception"]
                if e and e["kind"] == "edge":
                    target=e["target"]
                    if target not in blocks: self.err("exception.edge_target", "$.function.blocks", f"exception edge targets unknown block {target!r}"); continue
                    preds[target].add(b["id"]); exc_preds[target].add(b["id"]); succ[b["id"]].add(target)
        entry=f["entry_block"]
        if entry not in blocks: self.err("cfg.entry_missing", "$.function.entry_block", "entry block must exist")
        elif preds[entry]: self.err("cfg.entry_predecessor", "$.function.entry_block", "entry block cannot have predecessors")
        elif blocks[entry]["exception_parameter"]: self.err("exception.entry_handler", "$.function.entry_block", "entry block cannot be a handler")
        reachable=set(); work=[entry] if entry in blocks else []
        while work:
            cur=work.pop()
            if cur in reachable: continue
            reachable.add(cur); work.extend(succ[cur]-reachable)
        for bid in blocks:
            if bid not in reachable: self.err("cfg.unreachable_block", f"$.function.blocks[{order.index(bid)}]", "block is unreachable from entry")
            if exc_preds[bid] and not blocks[bid]["exception_parameter"]: self.err("exception.handler_parameter", f"$.function.blocks[{order.index(bid)}]", "exception target needs exception_parameter")
            if blocks[bid]["exception_parameter"] and not exc_preds[bid]: self.err("exception.unused_parameter", f"$.function.blocks[{order.index(bid)}]", "exception_parameter needs incoming exception edge")
            if blocks[bid]["exception_parameter"] and normal_preds[bid]: self.err("exception.normal_predecessor", f"$.function.blocks[{order.index(bid)}]", "handler block cannot have normal predecessors")
        dom={b: ({b} if b==entry else set(reachable)) for b in reachable}
        changed=True
        while changed:
            changed=False
            for bid in reachable-{entry}:
                ps=preds[bid]&reachable; new=({*set.intersection(*(dom[p] for p in ps))} if ps else set())|{bid}
                if new != dom[bid]: dom[bid]=new; changed=True
        def use(ident, ub, ui, path, effect=False):
            d=defs.get(ident)
            if not d: self.err("ssa.undefined_value", path, f"undefined SSA value {ident!r}"); return None
            db, di, item, kind=d
            if kind == "effect" and not effect: self.err("ssa.effect_as_value", path, "effect tokens cannot be used as JavaScript values")
            if db is not None and ((db==ub and di>=ui) or (db!=ub and db not in dom.get(ub,set()))):
                self.err("ssa.dominance" if db!=ub else "ssa.use_before_definition", path, f"definition of {ident!r} does not dominate this use")
            return item
        def value_type(ident):
            d=defs.get(ident)
            return (d[2].get("semantic_type"), d[2].get("representation")) if d and d[3] != "effect" else ("effect","effect") if d else None
        # Source metadata, operation contracts, operand uses and exception shape.
        all_effectful=False
        for bi,b in enumerate(f["blocks"]):
            bid=b["id"]; bp=f"$.function.blocks[{bi}]"
            for ii,ins in enumerate(b["instructions"]):
                ip=f"{bp}.instructions[{ii}]"; op=ins["op"]; contract=OPS.get(op)
                if contract is None: self.err("operation.unsupported", ip+".op", f"unsupported backend-neutral operation {op!r}"); continue
                lo,hi,result_kind,attrs,required=contract; n=len(ins["operands"])
                if n<lo or (hi is not None and n>hi): self.err("operation.operand_count", ip+".operands", f"{op} has invalid operand count {n}")
                if set(ins["attributes"]) != attrs: self.err("operation.attributes", ip+".attributes", f"expected attributes {sorted(attrs)}")
                ef=ins["effects"]
                if set(ef)!=set(EFFECTS): self.err("effect.flags", ip+".effects", "effects must contain exactly seven defined boolean flags")
                elif required-set(k for k,v in ef.items() if v is True):
                    for flag in sorted(required-set(k for k,v in ef.items() if v is True)): self.err("effect.underreported", ip+".effects."+flag, f"{op} requires {flag}=true")
                if any(ef.get(k) for k in EFFECTS): all_effectful=True
                if ef.get("may_suspend") and op!="async.await": self.err("effect.suspend_operation", ip+".effects.may_suspend", "v1 suspension is only valid on async.await")
                if ef.get("may_throw") != bool(ins["exception"]): self.err("exception.metadata_missing" if ef.get("may_throw") else "exception.metadata_unexpected", ip+".exception", "may_throw requires exactly one exception flow record")
                if ins["exception"] and ins["exception"]["kind"] == "edge":
                    target=blocks.get(ins["exception"]["target"])
                    if target and target["exception_parameter"] is None: self.err("exception.handler_parameter", ip+".exception", "exception edge target must declare exception_parameter")
                if result_kind is None and ins["result"] is not None: self.err("operation.unexpected_result", ip+".result", f"{op} must not produce a value")
                elif result_kind is not None and ins["result"] is None: self.err("operation.missing_result", ip+".result", f"{op} must produce a value")
                elif ins["result"]:
                    res=ins["result"]; expected=result_kind
                    if expected=="literal":
                        kinds={"undefined":"undefined","null":"null","boolean":"boolean","number":"number","bigint":"bigint","string":"string"}
                        if kinds.get(ins["attributes"].get("literal")) != res["semantic_type"]: self.err("operation.literal_type",ip+".result","literal attribute and result semantic type differ")
                    elif expected=="binding":
                        ref=ins["attributes"].get("binding"); target=bindings.get(ref) or captures.get(ref)
                        if target and (res["semantic_type"],res["representation"]) != (target["semantic_type"],target["representation"]): self.err("environment.binding_type",ip+".result","environment result type differs from binding")
                        if not target: self.err("environment.unknown_binding",ip+".attributes.binding","unknown binding or capture")
                    elif expected.startswith("operand:"):
                        ix=int(expected.split(":")[1]); actual=value_type(ins["operands"][ix]) if ix<len(ins["operands"]) else None
                        if actual and actual!=(res["semantic_type"],res["representation"]): self.err("operation.result_type",ip+".result","assignment result must preserve assigned operand type")
                    elif res["semantic_type"] != expected: self.err("operation.result_type",ip+".result",f"{op} must produce {expected}")
                    if expected=="dynamic" and res["representation"]!="boxed": self.err("operation.dynamic_representation",ip+".result","dynamic JS result must remain boxed")
                if op in {"js.get_property","js.set_property"} and not isinstance(ins["attributes"].get("key"),str): self.err("operation.property_key",ip+".attributes.key","property key must be a string")
                if op in {"env.load","env.store"}:
                    ref=ins["attributes"].get("binding"); target=bindings.get(ref) or captures.get(ref)
                    if not target:
                        self.err("environment.unknown_binding",ip+".attributes.binding","unknown binding or capture")
                    elif op=="env.store":
                        if not target["mutable"] or (ref in captures and target["mode"]!="cell"):
                            self.err("environment.immutable_store",ip+".attributes.binding","cannot store to immutable or by-value captured binding")
                        source_type=value_type(ins["operands"][0]) if ins["operands"] else None
                        expected_type=(target["semantic_type"],target["representation"])
                        if source_type is not None and source_type!=expected_type:
                            self.err("environment.store_type",ip+".operands[0]","stored value type differs from lexical binding")
                if op=="closure.create":
                    seen=set()
                    closure_captures=ins["attributes"].get("captures",[])
                    for ci,c in enumerate(closure_captures):
                        cp=f"{ip}.attributes.captures[{ci}]"
                        if not isinstance(c,dict) or set(c)!={"binding","mode"}:
                            self.err("closure.capture_descriptor",cp,"capture descriptor must contain only binding and mode")
                            continue
                        ref=c["binding"]; mode=c["mode"]; target=bindings.get(ref) or captures.get(ref)
                        if ref in seen: self.err("closure.duplicate_capture",cp,"closure capture binding must be unique")
                        seen.add(ref)
                        if not target: self.err("closure.unknown_capture",cp,"closure references an unknown lexical binding")
                        else:
                            required_mode="cell" if target["mutable"] or (ref in captures and target["mode"]=="cell") else "value"
                            if mode!=required_mode:
                                self.err("closure.capture_mode",cp+".mode",f"capture of this binding requires {required_mode!r} mode")
                for oi,oid in enumerate(ins["operands"]): use(oid,bid,ii,f"{ip}.operands[{oi}]")
                self._source(ins["source"],ip+".source",files,env,bindings,captures,op=="async.await")
                # Validate effect token input/output and ensure exceptional edges forward effect_out.
                if ef.get("may_throw") and ins["exception"] and ins["exception"]["kind"]=="edge":
                    e=ins["exception"]; target=blocks.get(e["target"])
                    if target:
                        args=e["arguments"]; pars=target["parameters"]
                        if len(args)!=len(pars): self.err("exception.edge_arity",ip+".exception.arguments","exception edge arguments must match ordinary block parameters")
                        if ins["effect_out"] and args and args[-1]!=ins["effect_out"]: self.err("effect.exception_edge",ip+".exception.arguments","exception edge must forward effect_out as its final argument")
                if op=="js.throw" and (ii!=len(b["instructions"])-1 or b["terminator"]["kind"]!="unreachable" or b["terminator"].get("reason")!="after_throw"):
                    self.err("exception.throw_terminator",bp,"js.throw must be final before unreachable(reason=after_throw)")
            term=b["terminator"]; tp=bp+".terminator"; self._source(term["source"],tp+".source",files,env,bindings,captures,False)
            if term["kind"]=="branch":
                use(term["condition"],bid,len(b["instructions"]),tp+".condition")
                if value_type(term["condition"]) and value_type(term["condition"])[0]!="boolean": self.err("cfg.branch_condition",tp+".condition","branch condition must be boolean")
            elif term["kind"]=="return":
                rv=term["value"]
                if f["return"] is None and rv is not None: self.err("function.return_type",tp+".value","void function cannot return value")
                if f["return"] is not None and rv is None: self.err("function.return_missing",tp+".value","non-void function must return a value")
                if rv is not None:
                    use(rv,bid,len(b["instructions"]),tp+".value")
                    if value_type(rv) and value_type(rv)!=(f["return"]["semantic_type"],f["return"]["representation"]): self.err("function.return_mismatch",tp+".value","return value does not match function return signature")
            elif term["kind"]=="unreachable" and term["reason"]=="proven_unreachable" and term["source"]["origin"]!="generated":
                self.err("cfg.unreachable_proof",tp+".source","proven_unreachable requires generated provenance")
        # Check edge arguments, SSA dominance and typed block-parameter contracts.
        for b in f["blocks"]:
            bid=b["id"]; term=b["terminator"]
            for ei,e in enumerate(term_edges(term)):
                target=blocks.get(e["target"])
                if not target: continue
                args=e["arguments"]; pars=target["parameters"]
                if len(args)!=len(pars): self.err("cfg.edge_arity",f"$.function.blocks.{bid}.terminator", "edge argument count differs from target parameter count"); continue
                for ai,(arg,param) in enumerate(zip(args,pars)):
                    path=f"$.function.blocks.{bid}.terminator.edge[{ei}].arguments[{ai}]"; use(arg,bid,len(b["instructions"]),path,effect=param["kind"]=="effect")
                    if value_type(arg)!=("effect","effect") if param["kind"]=="effect" else value_type(arg)!=(param["semantic_type"],param["representation"]): self.err("cfg.edge_argument_type",path,"edge argument type differs from target parameter")
            for ii,ins in enumerate(b["instructions"]):
                e=ins["exception"]
                if e and e["kind"]=="edge" and e["target"] in blocks:
                    target=blocks[e["target"]]; args=e["arguments"]; pars=target["parameters"]
                    if len(args)!=len(pars): continue
                    for ai,(arg,param) in enumerate(zip(args,pars)):
                        path=f"$.function.blocks.{bid}.instructions[{ii}].exception.arguments[{ai}]"; use(arg,bid,ii+1,path,effect=param["kind"]=="effect")
                        if param["kind"]=="effect":
                            if value_type(arg)!=("effect","effect"): self.err("effect.edge_type",path,"effect parameter requires an effect token")
                        elif value_type(arg)!=(param["semantic_type"],param["representation"]): self.err("cfg.edge_argument_type",path,"exception edge argument type differs from target parameter")
        # Effect token is a separate SSA chain, not an ordinary JS value.
        if all_effectful:
            if not f["effect_entry"]: self.err("effect.entry_missing","$.function.effect_entry","effectful function requires an entry effect token")
            for b in f["blocks"]:
                ps=b["parameters"]
                if not ps or ps[-1]["kind"]!="effect" or sum(p["kind"]=="effect" for p in ps)!=1: self.err("effect.block_parameter",f"$.function.blocks.{b['id']}.parameters","effectful block needs exactly one final effect parameter")
            entry_block=blocks.get(entry); expected=entry_block["parameters"][-1]["id"] if entry_block and entry_block["parameters"] and entry_block["parameters"][-1]["kind"]=="effect" else None
            if f["effect_entry"]!=expected: self.err("effect.entry_mismatch","$.function.effect_entry","effect_entry must name final effect parameter of entry block")
            for b in f["blocks"]:
                ps=b["parameters"]; current=ps[-1]["id"] if ps and ps[-1]["kind"]=="effect" else None
                for ii,ins in enumerate(b["instructions"]):
                    effectful=any(ins["effects"].get(k) is True for k in EFFECTS); ip=f"$.function.blocks.{b['id']}.instructions[{ii}]"
                    if effectful:
                        if ins["effect_in"]!=current or not ins["effect_out"]: self.err("effect.chain",ip,"effectful instruction must consume current token and produce next token")
                        current=ins["effect_out"]
                    elif ins["effect_in"] is not None or ins["effect_out"] is not None: self.err("effect.pure_token",ip,"pure instruction must not consume or produce effect tokens")
                for e in term_edges(b["terminator"]):
                    target=blocks.get(e["target"])
                    if target and target["parameters"] and target["parameters"][-1]["kind"]=="effect" and (not e["arguments"] or e["arguments"][-1]!=current): self.err("effect.normal_edge",f"$.function.blocks.{b['id']}.terminator","normal edge must forward current effect token")
        else:
            if f["effect_entry"] is not None: self.err("effect.unexpected_entry","$.function.effect_entry","pure function must not declare effect_entry")
            if any(p["kind"]=="effect" for b in f["blocks"] for p in b["parameters"]): self.err("effect.unexpected_parameter","$.function.blocks","pure function must not declare effect parameters")
            for b in f["blocks"]:
                for ii,ins in enumerate(b["instructions"]):
                    if ins["effect_in"] is not None or ins["effect_out"] is not None:
                        self.err("effect.pure_token",f"$.function.blocks.{b['id']}.instructions[{ii}]","pure function instruction must not consume or produce effect tokens")
        return self.errors
    def _source(self, s, path, files, env, bindings, captures, awaiting):
        if s["origin"]=="generated" and not s["reason"].strip(): self.err("source.generated_reason",path+".reason","generated source requires a reason")
        if s["origin"]=="user":
            if s["file"] not in files: self.err("source.unknown_file",path+".file",f"unknown source file {s['file']!r}")
            if (s["end"]["line"],s["end"]["column"]) < (s["start"]["line"],s["start"]["column"]): self.err("source.invalid_span",path,"span end precedes start")
        elif s.get("file") is not None: self.err("source.generated_file",path+".file","generated source must not fake a user file span")
        if s["scope"] is not None and s["scope"] not in {env["id"],env["parent"],*(c["source_environment"] for c in captures.values())}: self.err("source.unknown_scope",path+".scope","unknown lexical scope")
        visible=set(bindings)|set(captures)
        if len(s["bindings"])!=len(set(s["bindings"])): self.err("source.duplicate_binding",path+".bindings","source binding identities must be unique")
        for i,ref in enumerate(s["bindings"]):
            if ref not in visible: self.err("source.unknown_binding",f"{path}.bindings[{i}]","unknown lexical binding/capture")
        if awaiting and not s["suspension"]: self.err("source.suspension_missing",path+".suspension","async.await requires stable suspension point id")
        if not awaiting and s["suspension"] is not None: self.err("source.suspension_unexpected",path+".suspension","only async.await may declare suspension id")
        for i,frame in enumerate(s["inline_chain"]):
            p=f"{path}.inline_chain[{i}]"
            if frame["file"] not in files: self.err("source.inline_unknown_file",p+".file","unknown inline call-site file")
            if (frame["end"]["line"],frame["end"]["column"]) < (frame["start"]["line"],frame["start"]["column"]): self.err("source.inline_span",p,"inline span end precedes start")


def run_corpus(root, manifest_path, schema_path):
    manifest=load(manifest_path); schema=load(schema_path)
    if manifest.get("contract")!=CONTRACT or manifest.get("version")!=VERSION: raise ValueError("manifest contract/version mismatch")
    if schema.get("$schema")!="https://json-schema.org/draft/2020-12/schema": raise ValueError("schema must use JSON Schema draft 2020-12")
    results=[]
    for entry in manifest["fixtures"]:
        rel=PurePosixPath(entry["path"])
        if rel.is_absolute() or ".." in rel.parts: raise ValueError(f"invalid fixture path: {rel}")
        try:
            doc=load(root.joinpath(*rel.parts)); errors=Verifier(schema).verify(doc)
            actual="rejected" if errors else "accepted"
            codes={e[0] for e in errors}; missing=set(entry.get("diagnostics",[]))-codes
            if missing: errors.append(("fixture.expected_diagnostic_missing","$",f"missing diagnostics: {sorted(missing)}")); actual="unexpected"
        except (OSError, ValueError, json.JSONDecodeError) as ex:
            actual="invalid_fixture"; errors=[("fixture.load",str(rel),str(ex))]
        results.append({"fixture":str(rel),"expected":entry["expect"],"actual":actual,"passed":entry["expect"]==actual,"diagnostics":[{"code":c,"path":p,"message":m} for c,p,m in errors]})
    return {"contract":CONTRACT,"contract_version":VERSION,"verifier_version":VERSION,
            "summary":{"total":len(results),"passed":sum(r["passed"] for r in results),"failed":sum(not r["passed"] for r in results),
                       "accepted":sum(r["actual"]=="accepted" for r in results),"rejected":sum(r["actual"]=="rejected" for r in results)},"fixtures":results}

def main():
    parser=argparse.ArgumentParser(description="Verify the versioned backend-neutral Ogma Core IR contract")
    group=parser.add_mutually_exclusive_group(required=True); group.add_argument("--manifest",type=Path); group.add_argument("--input",type=Path)
    parser.add_argument("--schema",type=Path); parser.add_argument("--report",type=Path)
    args=parser.parse_args(); root=Path(__file__).resolve().parents[2]
    schema_path=args.schema or root/"contracts/core-ir/v1/schema.json"
    try:
        if args.manifest:
            report=run_corpus(root,args.manifest if args.manifest.is_absolute() else root/args.manifest,schema_path if schema_path.is_absolute() else root/schema_path)
            ok=report["summary"]["failed"]==0
        else:
            doc=load(args.input if args.input.is_absolute() else root/args.input); errors=Verifier(load(schema_path)).verify(doc)
            report={"contract":CONTRACT,"contract_version":VERSION,"verifier_version":VERSION,"accepted":not errors,"diagnostics":[{"code":c,"path":p,"message":m} for c,p,m in errors]}; ok=not errors
        output=json.dumps(report,indent=2,sort_keys=True,ensure_ascii=False)+"\n"
        if args.report:
            path=args.report if args.report.is_absolute() else root/args.report; path.parent.mkdir(parents=True,exist_ok=True); path.write_text(output,encoding="utf-8")
        else: sys.stdout.write(output)
        return 0 if ok else 1
    except (OSError,ValueError,json.JSONDecodeError) as ex:
        print(f"Core IR verifier error: {ex}",file=sys.stderr); return 2

if __name__=="__main__": raise SystemExit(main())
