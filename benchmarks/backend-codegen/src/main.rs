use anyhow::{Context as _, Result, anyhow, bail};
use cranelift_codegen::{
    ir::{
        AbiParam, InstBuilder, Signature, UserFuncName, Value,
        condcodes::{FloatCC, IntCC},
        instructions::BlockArg,
        types,
    },
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Linkage, Module, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};
use inkwell::{
    FloatPredicate, IntPredicate, OptimizationLevel,
    context::Context,
    targets::{
        CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine, TargetTriple,
    },
    values::{FloatValue, IntValue},
};
use libloading::Library;
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::{
    env, ffi::CString, fs,
    hint::black_box,
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};
use target_lexicon::Triple;

type UnaryI64 = unsafe extern "C" fn(i64) -> i64;
type BinaryI64 = unsafe extern "C" fn(i64, i64) -> i64;
type BinaryF64 = unsafe extern "C" fn(f64, f64) -> f64;
type TernaryF64 = unsafe extern "C" fn(f64, f64, f64) -> f64;
type AddNumber = unsafe extern "C" fn(f64, f64) -> i64;
type ToNumber = unsafe extern "C" fn(i64) -> f64;
type CallBoundary = unsafe extern "C" fn(i64, i64, i64, i64, i64, f64) -> i64;
type CallTarget = unsafe extern "C" fn(i64, i64) -> i64;

#[derive(Clone)]
struct Options {
    iterations: u64,
    warmups: usize,
    samples: usize,
    revision: String,
    output: PathBuf,
}
fn options() -> Result<Options> {
    let mut args = env::args().skip(1);
    let mut o = Options {
        iterations: 50_000,
        warmups: 3,
        samples: 101,
        revision: env::var("GITHUB_SHA").unwrap_or_else(|_| "working-tree".into()),
        output: PathBuf::from("ogm30-report.json"),
    };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--iterations" => o.iterations = args.next().context("missing iterations")?.parse()?,
            "--warmups" => o.warmups = args.next().context("missing warmups")?.parse()?,
            "--samples" => o.samples = args.next().context("missing samples")?.parse()?,
            "--revision" => o.revision = args.next().context("missing revision")?,
            "--output" => o.output = PathBuf::from(args.next().context("missing output")?),
            _ => bail!("unknown argument: {a}"),
        }
    }
    if o.iterations == 0 || o.samples < 101 {
        bail!("iterations must be positive and samples >= 101");
    }
    Ok(o)
}

#[derive(Clone)]
struct CoreFixture {
    label: &'static str,
    id: String,
    param_representations: Vec<String>,
    return_representation: String,
    operations: Vec<String>,
    terminators: Vec<String>,
    exception_edge: bool,
    host_symbol: Option<String>,
}
struct CoreCorpus {
    fixtures: Vec<CoreFixture>,
}
impl CoreCorpus {
    fn get(&self, label: &str) -> Result<&CoreFixture> {
        self.fixtures
            .iter()
            .find(|f| f.label == label)
            .ok_or_else(|| anyhow!("missing Core IR workload {label}"))
    }
    fn symbol(&self, label: &str) -> Result<String> {
        let f = self.get(label)?;
        Ok(format!("ogma_{}_{}", f.label, f.id))
    }
}
fn parse_fixture(label: &'static str, raw: &str) -> Result<CoreFixture> {
    let doc: JsonValue = serde_json::from_str(raw)?;
    if doc["contract"] != "ogma.core-ir" || doc["version"] != "1.0.0" {
        bail!("{label}: not ogma.core-ir/1.0.0");
    }
    let f = &doc["function"];
    let id = f["id"].as_str().context("Core IR id missing")?.to_owned();
    let params = f["parameters"]
        .as_array()
        .context("Core IR parameters missing")?
        .iter()
        .map(|p| p["representation"].as_str().unwrap_or("").to_owned())
        .collect();
    let ret = f["return"]["representation"]
        .as_str()
        .context("Core IR return representation missing")?
        .to_owned();
    let mut operations = Vec::new();
    let mut terminators = Vec::new();
    let mut exception_edge = false;
    let mut host_symbol = None;
    for block in f["blocks"].as_array().context("Core IR blocks missing")? {
        for ins in block["instructions"]
            .as_array()
            .context("instructions missing")?
        {
            let op = ins["op"]
                .as_str()
                .context("instruction op missing")?
                .to_owned();
            if op == "host.call" {
                host_symbol = ins["attributes"]["symbol"].as_str().map(str::to_owned);
            }
            exception_edge |= ins["exception"]["kind"] == "edge";
            operations.push(op);
        }
        terminators.push(
            block["terminator"]["kind"]
                .as_str()
                .context("terminator missing")?
                .to_owned(),
        );
    }
    Ok(CoreFixture {
        label,
        id,
        param_representations: params,
        return_representation: ret,
        operations,
        terminators,
        exception_edge,
        host_symbol,
    })
}
fn load_corpus() -> Result<CoreCorpus> {
    let manifest: JsonValue = serde_json::from_str(include_str!("../fixtures/workloads.json"))?;
    if manifest["contract"] != "ogma.backend-codegen-workloads"
        || manifest["version"] != "1.0.0"
        || manifest["core_ir_contract"] != "ogma.core-ir/1.0.0"
    {
        bail!("unsupported workload manifest");
    }
    let entries = manifest["workloads"]
        .as_array()
        .context("workloads missing")?;
    let sources = [
        (
            "branch",
            "tests/core-ir/fixtures/positive-ssa-branch.json",
            include_str!("../../../tests/core-ir/fixtures/positive-ssa-branch.json"),
        ),
        (
            "loop",
            "tests/core-ir/fixtures/positive-loop-carried-values.json",
            include_str!("../../../tests/core-ir/fixtures/positive-loop-carried-values.json"),
        ),
        (
            "dynamic_exception",
            "tests/core-ir/fixtures/positive-dynamic-exception.json",
            include_str!("../../../tests/core-ir/fixtures/positive-dynamic-exception.json"),
        ),
        (
            "call_boundary",
            "tests/core-ir/fixtures/positive-call-boundary.json",
            include_str!("../../../tests/core-ir/fixtures/positive-call-boundary.json"),
        ),
        (
            "host_call",
            "tests/core-ir/fixtures/positive-host-call.json",
            include_str!("../../../tests/core-ir/fixtures/positive-host-call.json"),
        ),
    ];
    let mut fixtures = Vec::new();
    for (label, path, raw) in sources {
        let entry = entries
            .iter()
            .find(|e| e["id"] == label)
            .with_context(|| format!("manifest workload {label} missing"))?;
        if entry["fixture"] != path {
            bail!("{label}: fixture path mismatch");
        }
        let f = parse_fixture(label, raw)?;
        for op in entry["operations"]
            .as_array()
            .context("expected operations missing")?
        {
            let op = op.as_str().context("invalid expected operation")?;
            if !f.operations.iter().any(|actual| actual == op) {
                bail!("{label}: missing Core IR op {op}");
            }
        }
        if let Some(terms) = entry["terminators"].as_array() {
            for term in terms {
                let term = term.as_str().context("invalid expected terminator")?;
                if !f.terminators.iter().any(|actual| actual == term) {
                    bail!("{label}: missing terminator {term}");
                }
            }
        }
        if entry["exception_edge"] == true && !f.exception_edge {
            bail!("{label}: expected exception edge");
        }
        if let Some(symbol) = entry["host_symbol"].as_str()
            && f.host_symbol.as_deref() != Some(symbol)
        {
            bail!("{label}: host symbol mismatch");
        }
        fixtures.push(f);
    }
    let c = CoreCorpus { fixtures };
    expect_signature(&c, "branch", &["f64", "f64"], "f64")?;
    expect_signature(&c, "loop", &["f64", "f64", "f64"], "f64")?;
    expect_signature(&c, "dynamic_exception", &["boxed", "boxed"], "boxed")?;
    expect_signature(
        &c,
        "call_boundary",
        &[
            "function_ref",
            "boxed",
            "string_ref",
            "object_ref",
            "object_ref",
            "f64",
        ],
        "boxed",
    )?;
    expect_signature(&c, "host_call", &["boxed"], "boxed")?;
    Ok(c)
}
fn expect_signature(c: &CoreCorpus, label: &str, params: &[&str], ret: &str) -> Result<()> {
    let f = c.get(label)?;
    if f.param_representations
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        != params
        || f.return_representation != ret
    {
        bail!("{label}: unsupported Core IR signature");
    }
    Ok(())
}
fn sym(name: &str) -> Vec<u8> {
    let mut b = name.as_bytes().to_vec();
    b.push(0);
    b
}

fn define_clif<F>(m: &mut ObjectModule, id: FuncId, sig: Signature, build: F) -> Result<()>
where
    F: FnOnce(&mut ObjectModule, &mut FunctionBuilder<'_>, &[Value]) -> Result<()>,
{
    let mut ctx = m.make_context();
    ctx.func.name = UserFuncName::user(0, id.as_u32());
    ctx.func.signature = sig;
    let mut fbctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbctx);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let p = b.block_params(entry).to_vec();
        build(m, &mut b, &p)?;
        b.seal_all_blocks();
        b.finalize(m.target_config());
    }
    m.define_function(id, &mut ctx)?;
    m.clear_context(&mut ctx);
    Ok(())
}
fn unary_i64_sig(m: &mut ObjectModule) -> Signature {
    let mut s = m.make_signature();
    s.params.push(AbiParam::new(types::I64));
    s.returns.push(AbiParam::new(types::I64));
    s
}
fn binary_i64_sig(m: &mut ObjectModule) -> Signature {
    let mut s = m.make_signature();
    s.params
        .extend([AbiParam::new(types::I64), AbiParam::new(types::I64)]);
    s.returns.push(AbiParam::new(types::I64));
    s
}
fn binary_f64_sig(m: &mut ObjectModule) -> Signature {
    let mut s = m.make_signature();
    s.params
        .extend([AbiParam::new(types::F64), AbiParam::new(types::F64)]);
    s.returns.push(AbiParam::new(types::F64));
    s
}
fn ternary_f64_sig(m: &mut ObjectModule) -> Signature {
    let mut s = m.make_signature();
    s.params.extend([
        AbiParam::new(types::F64),
        AbiParam::new(types::F64),
        AbiParam::new(types::F64),
    ]);
    s.returns.push(AbiParam::new(types::F64));
    s
}
fn compile_cranelift(path: &Path, c: &CoreCorpus) -> Result<u64> {
    let start = Instant::now();
    let mut flags = settings::builder();
    flags.set("opt_level", "speed")?;
    let isa = cranelift_native::builder()
        .map_err(|e| anyhow!(e))?
        .finish(settings::Flags::new(flags))?;
    let mut m = ObjectModule::new(ObjectBuilder::new(isa, "ogma-30", default_libcall_names())?);
    let branch_sig = binary_f64_sig(&mut m);
    let loop_sig = ternary_f64_sig(&mut m);
    let exception_sig = binary_i64_sig(&mut m);
    let host_sig = unary_i64_sig(&mut m);
    let mut call_sig = m.make_signature();
    call_sig.params.extend(
        [
            types::I64,
            types::I64,
            types::I64,
            types::I64,
            types::I64,
            types::F64,
        ]
        .into_iter()
        .map(AbiParam::new),
    );
    call_sig.returns.push(AbiParam::new(types::I64));
    let branch = m.declare_function(&c.symbol("branch")?, Linkage::Export, &branch_sig)?;
    let loop_fn = m.declare_function(&c.symbol("loop")?, Linkage::Export, &loop_sig)?;
    let exception = m.declare_function(
        &c.symbol("dynamic_exception")?,
        Linkage::Export,
        &exception_sig,
    )?;
    let call = m.declare_function(&c.symbol("call_boundary")?, Linkage::Export, &call_sig)?;
    let host = m.declare_function(&c.symbol("host_call")?, Linkage::Export, &host_sig)?;
    let mut add_num_sig = m.make_signature();
    add_num_sig
        .params
        .extend([AbiParam::new(types::F64), AbiParam::new(types::F64)]);
    add_num_sig.returns.push(AbiParam::new(types::I64));
    let mut to_num_sig = m.make_signature();
    to_num_sig.params.push(AbiParam::new(types::I64));
    to_num_sig.returns.push(AbiParam::new(types::F64));
    let add_number = m.declare_function("ogma_js_add_number", Linkage::Import, &add_num_sig)?;
    let to_number = m.declare_function("ogma_js_to_number", Linkage::Import, &to_num_sig)?;
    let add_completion =
        m.declare_function("ogma_js_add_completion", Linkage::Import, &exception_sig)?;
    let js_throw = m.declare_function("ogma_js_throw", Linkage::Import, &host_sig)?;
    let mut add_mixed_sig = m.make_signature();
    add_mixed_sig
        .params
        .extend([AbiParam::new(types::I64), AbiParam::new(types::F64)]);
    add_mixed_sig.returns.push(AbiParam::new(types::I64));
    let add_mixed = m.declare_function("ogma_js_add_mixed", Linkage::Import, &add_mixed_sig)?;
    let mut js_call_sig = m.make_signature();
    js_call_sig.params.extend([
        AbiParam::new(types::I64),
        AbiParam::new(types::I64),
        AbiParam::new(types::I64),
    ]);
    js_call_sig.returns.push(AbiParam::new(types::I64));
    let js_call = m.declare_function("ogma_js_call", Linkage::Import, &js_call_sig)?;
    let host_helper = m.declare_function("ogma_host_echo", Linkage::Import, &host_sig)?;

    define_clif(&mut m, branch, branch_sig, |_, b, p| {
        let eq = b.ins().fcmp(FloatCC::Equal, p[0], p[1]);
        let yes = b.create_block();
        let no = b.create_block();
        b.ins().brif(eq, yes, &[], no, &[]);
        b.switch_to_block(yes);
        b.ins().return_(&[p[0]]);
        b.switch_to_block(no);
        b.ins().return_(&[p[1]]);
        Ok(())
    })?;
    define_clif(&mut m, loop_fn, loop_sig, |m, b, p| {
        let lp = b.create_block();
        let body = b.create_block();
        let exit = b.create_block();
        let current = b.append_block_param(lp, types::F64);
        let limit = b.append_block_param(lp, types::F64);
        let step = b.append_block_param(lp, types::F64);
        let bc = b.append_block_param(body, types::F64);
        let bl = b.append_block_param(body, types::F64);
        let bs = b.append_block_param(body, types::F64);
        let out = b.append_block_param(exit, types::F64);
        let entry_args = [
            BlockArg::Value(p[0]),
            BlockArg::Value(p[1]),
            BlockArg::Value(p[2]),
        ];
        b.ins().jump(lp, &entry_args);
        b.switch_to_block(lp);
        let eq = b.ins().fcmp(FloatCC::Equal, current, limit);
        let exit_args = [BlockArg::Value(current)];
        let body_args = [
            BlockArg::Value(current),
            BlockArg::Value(limit),
            BlockArg::Value(step),
        ];
        b.ins().brif(eq, exit, &exit_args, body, &body_args);
        b.switch_to_block(body);
        let add_ref = m.declare_func_in_func(add_number, b.func);
        let add = b.ins().call(add_ref, &[bc, bs]);
        let boxed = b.inst_results(add)[0];
        let conv_ref = m.declare_func_in_func(to_number, b.func);
        let conv = b.ins().call(conv_ref, &[boxed]);
        let next = b.inst_results(conv)[0];
        let next_args = [
            BlockArg::Value(next),
            BlockArg::Value(bl),
            BlockArg::Value(bs),
        ];
        b.ins().jump(lp, &next_args);
        b.switch_to_block(exit);
        b.ins().return_(&[out]);
        Ok(())
    })?;
    define_clif(&mut m, exception, exception_sig, |m, b, p| {
        let add_ref = m.declare_func_in_func(add_completion, b.func);
        let add = b.ins().call(add_ref, &[p[0], p[1]]);
        let result = b.inst_results(add)[0];
        let tag = b.ins().ushr_imm_u(result, 56);
        let zero = b.ins().iconst(types::I64, 0);
        let normal = b.ins().icmp(IntCC::Equal, tag, zero);
        let ok = b.create_block();
        let thrown = b.create_block();
        b.ins().brif(normal, ok, &[], thrown, &[]);
        b.switch_to_block(ok);
        b.ins().return_(&[result]);
        b.switch_to_block(thrown);
        let throw_ref = m.declare_func_in_func(js_throw, b.func);
        let rethrow = b.ins().call(throw_ref, &[result]);
        let value = b.inst_results(rethrow)[0];
        b.ins().return_(&[value]);
        Ok(())
    })?;
    define_clif(&mut m, call, call_sig, |m, b, p| {
        let _same = b.ins().icmp(IntCC::Equal, p[3], p[4]);
        let add_ref = m.declare_func_in_func(add_mixed, b.func);
        let add = b.ins().call(add_ref, &[p[2], p[5]]);
        let mixed = b.inst_results(add)[0];
        let call_ref = m.declare_func_in_func(js_call, b.func);
        let invoked = b.ins().call(call_ref, &[p[0], p[1], mixed]);
        let value = b.inst_results(invoked)[0];
        b.ins().return_(&[value]);
        Ok(())
    })?;
    define_clif(&mut m, host, host_sig, |m, b, p| {
        let f = m.declare_func_in_func(host_helper, b.func);
        let call = b.ins().call(f, &[p[0]]);
        let value = b.inst_results(call)[0];
        b.ins().return_(&[value]);
        Ok(())
    })?;
    fs::write(path, m.finish().emit()?)?;
    Ok(start.elapsed().as_nanos() as u64)
}

fn llvm_call_i64<'ctx>(
    b: &inkwell::builder::Builder<'ctx>,
    f: inkwell::values::FunctionValue<'ctx>,
    args: &[inkwell::values::BasicMetadataValueEnum<'ctx>],
    name: &str,
) -> Result<IntValue<'ctx>> {
    Ok(b.build_call(f, args, name)?
        .try_as_basic_value()
        .unwrap_basic()
        .into_int_value())
}
fn llvm_call_f64<'ctx>(
    b: &inkwell::builder::Builder<'ctx>,
    f: inkwell::values::FunctionValue<'ctx>,
    args: &[inkwell::values::BasicMetadataValueEnum<'ctx>],
    name: &str,
) -> Result<FloatValue<'ctx>> {
    Ok(b.build_call(f, args, name)?
        .try_as_basic_value()
        .unwrap_basic()
        .into_float_value())
}
fn compile_llvm(path: &Path, c: &CoreCorpus) -> Result<u64> {
    let start = Instant::now();
    Target::initialize_all(&InitializationConfig::default());
    let triple = TargetMachine::get_default_triple();
    let target = Target::from_triple(&triple).map_err(|e| anyhow!(e.to_string()))?;
    let tm = target
        .create_target_machine(
            &triple,
            "generic",
            "",
            OptimizationLevel::Default,
            RelocMode::PIC,
            CodeModel::Default,
        )
        .context("create LLVM target machine")?;
    let cx = Context::create();
    let module = cx.create_module("ogma_ogm30");
    module.set_triple(&triple);
    module.set_data_layout(&tm.get_target_data().get_data_layout());
    let b = cx.create_builder();
    let i64t = cx.i64_type();
    let f64t = cx.f64_type();
    let unary = i64t.fn_type(&[i64t.into()], false);
    let binary = i64t.fn_type(&[i64t.into(), i64t.into()], false);
    let binary_f = f64t.fn_type(&[f64t.into(), f64t.into()], false);
    let ternary_f = f64t.fn_type(&[f64t.into(), f64t.into(), f64t.into()], false);
    let branch_type = f64t.fn_type(&[f64t.into(), f64t.into()], false);
    let call_type = i64t.fn_type(
        &[
            i64t.into(),
            i64t.into(),
            i64t.into(),
            i64t.into(),
            i64t.into(),
            f64t.into(),
        ],
        false,
    );

    let branch = module.add_function(&c.symbol("branch")?, branch_type, None);
    let entry = cx.append_basic_block(branch, "entry");
    let yes = cx.append_basic_block(branch, "equal");
    let no = cx.append_basic_block(branch, "not_equal");
    b.position_at_end(entry);
    let left = branch.get_nth_param(0).unwrap().into_float_value();
    let right = branch.get_nth_param(1).unwrap().into_float_value();
    let eq = b.build_float_compare(FloatPredicate::OEQ, left, right, "strict_equal")?;
    b.build_conditional_branch(eq, yes, no)?;
    b.position_at_end(yes);
    b.build_return(Some(&left))?;
    b.position_at_end(no);
    b.build_return(Some(&right))?;

    let add_number = module.add_function("ogma_js_add_number", binary_f, None);
    let to_number = module.add_function(
        "ogma_js_to_number",
        i64t.fn_type(&[i64t.into()], false),
        None,
    );
    let loop_fn = module.add_function(&c.symbol("loop")?, ternary_f, None);
    let loop_entry = cx.append_basic_block(loop_fn, "entry");
    let loop_bb = cx.append_basic_block(loop_fn, "loop");
    let body = cx.append_basic_block(loop_fn, "body");
    let exit = cx.append_basic_block(loop_fn, "exit");
    b.position_at_end(loop_entry);
    b.build_unconditional_branch(loop_bb)?;
    b.position_at_end(loop_bb);
    let current = b.build_phi(f64t, "current")?;
    let limit = b.build_phi(f64t, "limit")?;
    let step = b.build_phi(f64t, "step")?;
    let current_v = current.as_basic_value().into_float_value();
    let limit_v = limit.as_basic_value().into_float_value();
    let eq = b.build_float_compare(FloatPredicate::OEQ, current_v, limit_v, "at_limit")?;
    b.build_conditional_branch(eq, exit, body)?;
    b.position_at_end(body);
    let step_v = step.as_basic_value().into_float_value();
    let boxed = llvm_call_i64(&b, add_number, &[current_v.into(), step_v.into()], "js_add")?;
    let next = llvm_call_f64(&b, to_number, &[boxed.into()], "to_number")?;
    b.build_unconditional_branch(loop_bb)?;
    let body_end = b.get_insert_block().context("loop body missing")?;
    let start_value = loop_fn.get_nth_param(0).unwrap().into_float_value();
    let max = loop_fn.get_nth_param(1).unwrap().into_float_value();
    let increment = loop_fn.get_nth_param(2).unwrap().into_float_value();
    current.add_incoming(&[(&start_value, loop_entry), (&next, body_end)]);
    limit.add_incoming(&[(&max, loop_entry), (&limit_v, body_end)]);
    step.add_incoming(&[(&increment, loop_entry), (&step_v, body_end)]);
    b.position_at_end(exit);
    b.build_return(Some(&current_v))?;

    let add_completion = module.add_function("ogma_js_add_completion", binary, None);
    let js_throw = module.add_function("ogma_js_throw", unary, None);
    let exception = module.add_function(&c.symbol("dynamic_exception")?, binary, None);
    let ex_entry = cx.append_basic_block(exception, "entry");
    let ok = cx.append_basic_block(exception, "normal");
    let thrown = cx.append_basic_block(exception, "throw");
    b.position_at_end(ex_entry);
    let lhs = exception.get_nth_param(0).unwrap().into_int_value();
    let rhs = exception.get_nth_param(1).unwrap().into_int_value();
    let result = llvm_call_i64(&b, add_completion, &[lhs.into(), rhs.into()], "js_add")?;
    let tag = b.build_right_shift(result, i64t.const_int(56, false), false, "completion_tag")?;
    let normal = b.build_int_compare(IntPredicate::EQ, tag, i64t.const_zero(), "is_normal")?;
    b.build_conditional_branch(normal, ok, thrown)?;
    b.position_at_end(ok);
    b.build_return(Some(&result))?;
    b.position_at_end(thrown);
    let rethrow = llvm_call_i64(&b, js_throw, &[result.into()], "js_throw")?;
    b.build_return(Some(&rethrow))?;

    let add_mixed = module.add_function(
        "ogma_js_add_mixed",
        i64t.fn_type(&[i64t.into(), f64t.into()], false),
        None,
    );
    let js_call = module.add_function(
        "ogma_js_call",
        i64t.fn_type(&[i64t.into(), i64t.into(), i64t.into()], false),
        None,
    );
    let call = module.add_function(&c.symbol("call_boundary")?, call_type, None);
    b.position_at_end(cx.append_basic_block(call, "entry"));
    let callee = call.get_nth_param(0).unwrap().into_int_value();
    let receiver = call.get_nth_param(1).unwrap().into_int_value();
    let text = call.get_nth_param(2).unwrap().into_int_value();
    let left = call.get_nth_param(3).unwrap().into_int_value();
    let right = call.get_nth_param(4).unwrap().into_int_value();
    let numeric = call.get_nth_param(5).unwrap().into_float_value();
    let _same = b.build_int_compare(IntPredicate::EQ, left, right, "strict_equal")?;
    let mixed = llvm_call_i64(&b, add_mixed, &[text.into(), numeric.into()], "js_add")?;
    let invoked = llvm_call_i64(
        &b,
        js_call,
        &[callee.into(), receiver.into(), mixed.into()],
        "js_call",
    )?;
    b.build_return(Some(&invoked))?;

    let host_helper = module.add_function("ogma_host_echo", unary, None);
    let host = module.add_function(&c.symbol("host_call")?, unary, None);
    b.position_at_end(cx.append_basic_block(host, "entry"));
    let arg = host.get_nth_param(0).unwrap().into_int_value();
    let value = llvm_call_i64(&b, host_helper, &[arg.into()], "host_call")?;
    b.build_return(Some(&value))?;
    module
        .verify()
        .map_err(|e| anyhow!("LLVM verification: {e}"))?;
    tm.write_to_file(&module, FileType::Object, path)
        .map_err(|e| anyhow!("LLVM object emission: {e}"))?;
    Ok(start.elapsed().as_nanos() as u64)
}

const HELPERS: &str = r#"
#include <stdint.h>
#include <stdio.h>
#include <string.h>
static const uint64_t PAYLOAD_MASK=UINT64_C(0x00ffffffffffffff);
static char dynamic_string[128];
int64_t ogma_js_add_number(double a,double b){union{double d;uint64_t u;}v;v.d=a+b;return (int64_t)v.u;}
double ogma_js_to_number(int64_t boxed){union{double d;uint64_t u;}v;v.u=(uint64_t)boxed;return v.d;}
int64_t ogma_js_add_completion(int64_t a,int64_t b){
 uint64_t tag=(a==INT64_MIN)?1:0;uint64_t payload=(a==INT64_MIN)?(uint64_t)b:(uint64_t)a+(uint64_t)b;
 return (int64_t)((tag<<56)|(payload&PAYLOAD_MASK));
}
int64_t ogma_js_throw(int64_t completion){return completion;}
int64_t ogma_js_add_mixed(int64_t text,double number){const char *left=(const char *)(uintptr_t)text;(void)snprintf(dynamic_string,sizeof(dynamic_string),"%s%.0f",left,number);return (int64_t)(uintptr_t)dynamic_string;}
typedef int64_t (*OgmaCallTarget)(int64_t,int64_t);
int64_t ogma_js_call(int64_t callee,int64_t receiver,int64_t arg){OgmaCallTarget target=(OgmaCallTarget)(uintptr_t)callee;return target(receiver,arg);}
int64_t ogma_js_call_target(int64_t receiver,int64_t arg){const char *value=(const char *)(uintptr_t)arg;return receiver+(int64_t)strlen(value);}
int64_t ogma_host_echo(int64_t arg){return arg+11;}
"#;
fn link(object: &Path, shared: &Path, helper: &Path) -> Result<u64> {
    fs::write(helper, HELPERS)?;
    let start = Instant::now();
    let out = Command::new("cc")
        .args(["-shared", "-fPIC"])
        .arg(object)
        .arg("-x")
        .arg("c")
        .arg(helper)
        .arg("-o")
        .arg(shared)
        .output()?;
    if !out.status.success() {
        bail!(
            "native link failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(start.elapsed().as_nanos() as u64)
}
#[derive(Serialize)]
struct Distribution {
    p50_ns_per_call: f64,
    p95_ns_per_call: f64,
    p99_ns_per_call: f64,
    throughput_calls_per_second: f64,
    raw_ns_per_call_samples: Vec<f64>,
}
#[derive(Serialize)]
struct LatencyDistribution {
    p50_ns: f64,
    p95_ns: f64,
    p99_ns: f64,
    raw_samples_ns: Vec<f64>,
}
fn distribution(samples: Vec<f64>) -> Distribution {
    let mut s = samples.clone();
    s.sort_by(f64::total_cmp);
    let p = |q: f64| s[((s.len() - 1) as f64 * q).ceil() as usize];
    let median = p(0.5);
    Distribution {
        p50_ns_per_call: median,
        p95_ns_per_call: p(0.95),
        p99_ns_per_call: p(0.99),
        throughput_calls_per_second: 1e9 / median,
        raw_ns_per_call_samples: samples,
    }
}
fn latency_distribution(samples: Vec<f64>) -> LatencyDistribution {
    let mut s = samples.clone();
    s.sort_by(f64::total_cmp);
    let p = |q: f64| s[((s.len() - 1) as f64 * q).ceil() as usize];
    LatencyDistribution {
        p50_ns: p(0.5),
        p95_ns: p(0.95),
        p99_ns: p(0.99),
        raw_samples_ns: samples,
    }
}
fn bench<F: FnMut(u64)>(o: &Options, mut f: F) -> Distribution {
    for _ in 0..o.warmups {
        for n in 0..o.iterations {
            f(black_box(n));
        }
    }
    let mut samples = Vec::with_capacity(o.samples);
    for _ in 0..o.samples {
        let start = Instant::now();
        for n in 0..o.iterations {
            f(black_box(n));
        }
        samples.push(start.elapsed().as_nanos() as f64 / o.iterations as f64);
    }
    distribution(samples)
}
#[derive(Serialize)]
struct BackendReport {
    compile_time_ns: LatencyDistribution,
    link_time_ns: LatencyDistribution,
    object_bytes: u64,
    shared_library_bytes: u64,
    workloads: JsonValue,
}
fn compile_rounds(backend: &str, root: &Path, c: &CoreCorpus) -> Result<(Vec<f64>, PathBuf)> {
    let mut samples = Vec::new();
    let mut last = PathBuf::new();
    for n in 0..31 {
        last = root.join(format!("{backend}-{n}.o"));
        let t = if backend == "cranelift" {
            compile_cranelift(&last, c)?
        } else {
            compile_llvm(&last, c)?
        };
        samples.push(t as f64);
    }
    Ok((samples, last))
}
fn run_backend(name: &str, o: &Options, root: &Path, c: &CoreCorpus) -> Result<BackendReport> {
    let (compile, object) = compile_rounds(name, root, c)?;
    let shared = root.join(format!("{name}.so"));
    let helper = root.join(format!("{name}.c"));
    let mut link_samples = Vec::new();
    for _ in 0..5 {
        link_samples.push(link(&object, &shared, &helper)? as f64);
    }
    let lib = unsafe { Library::new(&shared)? };
    let branch: BinaryF64 = unsafe { *lib.get(sym(&c.symbol("branch")?).as_slice())? };
    let loop_fn: TernaryF64 = unsafe { *lib.get(sym(&c.symbol("loop")?).as_slice())? };
    let exception: BinaryI64 =
        unsafe { *lib.get(sym(&c.symbol("dynamic_exception")?).as_slice())? };
    let call: CallBoundary = unsafe { *lib.get(sym(&c.symbol("call_boundary")?).as_slice())? };
    let call_target: CallTarget = unsafe { *lib.get(b"ogma_js_call_target\0")? };
    let callee = call_target as usize as i64;
    let text = CString::new("item=").unwrap();
    let text_handle = text.as_ptr() as i64;
    let host: UnaryI64 = unsafe { *lib.get(sym(&c.symbol("host_call")?).as_slice())? };
    if unsafe { branch(7.0, 7.0) } != 7.0 || unsafe { branch(7.0, 8.0) } != 8.0 {
        bail!("{name}: branch mismatch");
    }
    if unsafe { loop_fn(0.0, 5.0, 1.0) } != 5.0 {
        bail!("{name}: loop mismatch");
    }
    if unsafe { exception(7, 8) } != 15 {
        bail!("{name}: normal completion mismatch");
    }
    if ((unsafe { exception(i64::MIN, 9) } as u64) >> 56) != 1 {
        bail!("{name}: Throw completion missing");
    }
    if unsafe { call(callee, 2, text_handle, 4, 4, 5.0) } != 8 {
        bail!("{name}: call-boundary result mismatch");
    }
    if unsafe { host(31) } != 42 {
        bail!("{name}: host.call mismatch");
    }
    let add_number: AddNumber = unsafe { *lib.get(b"ogma_js_add_number\0")? };
    let to_number: ToNumber = unsafe { *lib.get(b"ogma_js_to_number\0")? };
    let add_completion: BinaryI64 = unsafe { *lib.get(b"ogma_js_add_completion\0")? };
    let host_direct: UnaryI64 = unsafe { *lib.get(b"ogma_host_echo\0")? };
    let workloads = serde_json::json!({
      "core_ir_ssa_branch":bench(o,|n|{black_box(unsafe{branch((n%17)as f64,(n%13)as f64)});}),
      "core_ir_loop_js_add_to_number":bench(o,|_|{black_box(unsafe{loop_fn(0.0,5.0,1.0)});}),
      "core_ir_dynamic_add_normal_completion":bench(o,|n|{black_box(unsafe{exception((n%100)as i64,7)});}),
      "core_ir_dynamic_add_throw_completion":bench(o,|_|{black_box(unsafe{exception(i64::MIN,7)});}),
      "core_ir_js_add_js_call_boundary":bench(o,|_|{black_box(unsafe{call(callee,2,text_handle,4,4,5.0)});}),
      "core_ir_host_call":bench(o,|n|{black_box(unsafe{host(n as i64)});}),
      "direct_js_add_number_helper":bench(o,|n|{black_box(unsafe{to_number(add_number(n as f64,1.0))});}),
      "direct_dynamic_completion_helper":bench(o,|n|{black_box(unsafe{add_completion(n as i64,7)});}),
      "direct_host_helper":bench(o,|n|{black_box(unsafe{host_direct(n as i64)});})
    });
    Ok(BackendReport {
        compile_time_ns: latency_distribution(compile),
        link_time_ns: latency_distribution(link_samples),
        object_bytes: fs::metadata(&object)?.len(),
        shared_library_bytes: fs::metadata(&shared)?.len(),
        workloads,
    })
}

#[derive(Serialize)]
struct Report {
    schema_version: u32,
    task: &'static str,
    source_revision: String,
    tested_revision: String,
    rustc: String,
    llvm: String,
    clang: String,
    os: String,
    host_arch: &'static str,
    cpu_model: String,
    build_configuration: &'static str,
    iterations_per_sample: u64,
    warmup_rounds: usize,
    measurement_samples: usize,
    target_lookup: JsonValue,
    llvm_package_footprint_kib: Option<u64>,
    cranelift_crate_source_bytes: u64,
    llvm_binding_source_bytes: u64,
    cranelift_build_artifact_bytes: u64,
    llvm_binding_build_artifact_bytes: u64,
    integration_notes: JsonValue,
    cranelift: BackendReport,
    llvm_backend: BackendReport,
}
fn command(p: &str, a: &[&str]) -> String {
    Command::new(p)
        .args(a)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|| "unavailable".into())
}
fn target_lookup() -> JsonValue {
    Target::initialize_all(&InitializationConfig::default());
    let triples = [
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "x86_64-pc-windows-msvc",
        "aarch64-apple-darwin",
    ];
    JsonValue::Array(triples.iter().map(|name|{let t:Triple=name.parse().unwrap();serde_json::json!({"triple":name,
 "cranelift":cranelift_codegen::isa::lookup(t).is_ok(),"llvm":Target::from_triple(&TargetTriple::create(name)).is_ok(),
 "scope":"target lookup only; no cross-link or ABI claim"})}).collect())
}
fn dir_size(p: &Path) -> u64 {
    let Ok(e) = fs::read_dir(p) else {
        return 0;
    };
    e.flatten()
        .map(|x| {
            if x.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                dir_size(&x.path())
            } else {
                x.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}
fn cargo_source_size(prefixes: &[&str]) -> u64 {
    let home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")));
    let Some(home) = home else {
        return 0;
    };
    let Ok(indices) = fs::read_dir(home.join("registry").join("src")) else {
        return 0;
    };
    indices
        .flatten()
        .map(|i| {
            let Ok(pkgs) = fs::read_dir(i.path()) else {
                return 0;
            };
            pkgs.flatten()
                .filter_map(|p| {
                    let n = p.file_name().to_string_lossy().into_owned();
                    prefixes
                        .iter()
                        .any(|x| n.starts_with(x))
                        .then(|| dir_size(&p.path()))
                })
                .sum::<u64>()
        })
        .sum()
}
fn cargo_artifact_size(prefix: &str) -> u64 {
    let root = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"));
    let Ok(e) = fs::read_dir(root.join("release").join("deps")) else {
        return 0;
    };
    e.flatten()
        .filter_map(|x| {
            x.file_name()
                .to_string_lossy()
                .starts_with(prefix)
                .then(|| x.metadata().map(|m| m.len()).unwrap_or(0))
        })
        .sum()
}
fn main() -> Result<()> {
    let o = options()?;
    let c = load_corpus()?;
    let root = env::temp_dir().join(format!("ogm30-{}", std::process::id()));
    fs::create_dir_all(&root)?;
    let cranelift = run_backend("cranelift", &o, &root, &c)?;
    let llvm_backend = run_backend("llvm", &o, &root, &c)?;
    let footprint = Command::new("dpkg-query")
        .args([
            "-W",
            concat!("-f=$", "{Installed-Size}\\n"),
            "llvm-18-dev",
            "llvm-18",
            "clang-18",
        ])
        .output()
        .ok()
        .filter(|x| x.status.success())
        .map(|x| {
            String::from_utf8_lossy(&x.stdout)
                .lines()
                .filter_map(|v| v.parse::<u64>().ok())
                .sum()
        });
    let report = Report {
        schema_version: 1,
        task: "OGM-30",
        source_revision: o.revision.clone(),
        tested_revision: env::var("GITHUB_SHA").unwrap_or_else(|_| o.revision.clone()),
        rustc: command("rustc", &["--version"]),
        llvm: command("llvm-config-18", &["--version"]),
        clang: command("clang-18", &["--version"]),
        os: command("uname", &["-sr"]),
        host_arch: env::consts::ARCH,
        cpu_model: command("lscpu", &["--noheadings", "--parse=MODELNAME"]),
        build_configuration: "release; Cranelift opt_level=speed; LLVM OptimizationLevel::Default; native target; PIC",
        iterations_per_sample: o.iterations,
        warmup_rounds: o.warmups,
        measurement_samples: o.samples,
        target_lookup: target_lookup(),
        llvm_package_footprint_kib: footprint,
        cranelift_crate_source_bytes: cargo_source_size(&["cranelift-"]),
        llvm_binding_source_bytes: cargo_source_size(&["inkwell-", "llvm-sys-181"]),
        cranelift_build_artifact_bytes: cargo_artifact_size("libcranelift_"),
        llvm_binding_build_artifact_bytes: cargo_artifact_size("libinkwell-")
            + cargo_artifact_size("libllvm_sys_181-"),
        integration_notes: serde_json::json!({"core_ir":"both backends lower the same canonical ogma.core-ir/1.0.0 fixtures",
 "cranelift":"Rust crates and in-process ObjectModule; no external LLVM toolchain",
 "llvm":"Inkwell/llvm-sys and system LLVM 18 development toolchain; in-process target-machine emission",
 "runtime_helpers":"C helper stubs exercise explicit JS semantic and host-call boundaries; not production Ogma Runtime or OGM-27 root maps",
 "status":"M0 benchmark only; no production compiler/Runtime integration"}),
        cranelift,
        llvm_backend,
    };
    if let Some(parent) = o.output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&o.output, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    let _ = fs::remove_dir_all(root);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn benchmark_uses_canonical_core_ir_v1_fixtures() {
        let c = load_corpus().unwrap();
        assert_eq!(c.fixtures.len(), 5);
        assert!(c.get("dynamic_exception").unwrap().exception_edge);
        assert_eq!(
            c.get("host_call").unwrap().host_symbol.as_deref(),
            Some("benchmark.host.echo")
        );
        assert!(
            c.get("call_boundary")
                .unwrap()
                .operations
                .contains(&"js.call".into())
        );
    }
}
