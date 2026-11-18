use anyhow::{anyhow, bail, Context as _, Result};
use cranelift_codegen::{
    ir::{types, AbiParam, InstBuilder, IntCC, Signature, UserFuncName, Value},
    settings, Context as ClifContext,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{default_libcall_names, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use inkwell::{
    context::Context,
    targets::{CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine, TargetTriple},
    values::IntValue,
    IntPredicate, OptimizationLevel,
};
use libloading::Library;
use serde::{Deserialize, Serialize};
use std::{env, fs, hint::black_box, path::{Path, PathBuf}, process::Command, time::Instant};
use target_lexicon::Triple;

type Unary = unsafe extern "C" fn(i64) -> i64;
type Binary = unsafe extern "C" fn(i64, i64) -> i64;

#[derive(Clone)]
struct Options { iterations: u64, warmups: usize, samples: usize, revision: String, output: PathBuf }

fn options() -> Result<Options> {
    let mut args = env::args().skip(1);
    let mut o = Options { iterations: 50_000, warmups: 3, samples: 101,
        revision: env::var("GITHUB_SHA").unwrap_or_else(|_| "working-tree".into()),
        output: PathBuf::from("ogm30-report.json") };
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
    if o.iterations == 0 || o.samples < 5 { bail!("iterations must be positive and samples >= 5"); }
    Ok(o)
}

#[derive(Deserialize)]
struct Manifest {
    contract: String,
    version: String,
    core_ir_reference: String,
    functions: Vec<FunctionSpec>,
}
#[derive(Deserialize)]
struct FunctionSpec {
    name: String,
    kind: String,
    params: usize,
    effects: Vec<String>,
}
fn load_manifest() -> Result<Manifest> {
    let manifest: Manifest = serde_json::from_str(include_str!("../fixtures/workloads.json"))?;
    if manifest.contract != "ogma.backend-benchmark-core-ir" || manifest.version != "1.0.0"
        || manifest.core_ir_reference != "ogma.core-ir/1.0.0" {
        bail!("unsupported benchmark Core IR manifest contract/version");
    }
    let required = ["arithmetic","control_flow","internal_callee","internal_call","dynamic_add","completion","host_call"];
    for kind in required {
        let matches: Vec<_> = manifest.functions.iter().filter(|f| f.kind == kind).collect();
        if matches.len() != 1 { bail!("manifest must contain exactly one {kind} workload"); }
        let expected_params = if kind == "dynamic_add" { 2 } else { 1 };
        if matches[0].params != expected_params { bail!("invalid parameter count for {kind}"); }
        let required_effects: &[&str] = match kind {
            "dynamic_add" => &["may_call_host", "may_throw"],
            "completion" => &["may_call_host", "may_throw"],
            "host_call" => &["may_call_host", "may_reenter"],
            "internal_call" => &["may_call_host"],
            _ => &[],
        };
        for effect in required_effects {
            if !matches[0].effects.iter().any(|actual| actual == effect) {
                bail!("missing {effect} effect for {kind}");
            }
        }
    }
    Ok(manifest)
}
fn workload_name<'a>(manifest: &'a Manifest, kind: &str) -> Result<&'a str> {
    manifest.functions.iter().find(|f| f.kind == kind).map(|f| f.name.as_str())
        .ok_or_else(|| anyhow!("missing workload kind {kind}"))
}

fn define_clif<F>(m: &mut ObjectModule, id: FuncId, sig: Signature, build: F) -> Result<()>
where F: FnOnce(&mut ObjectModule, &mut FunctionBuilder<'_>, &[Value]) -> Result<Vec<Value>> {
    let mut ctx = m.make_context();
    ctx.func = ClifContext::for_function(cranelift_codegen::ir::Function::with_name_signature(
        UserFuncName::user(0, id.as_u32()), sig));
    let mut fbctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbctx);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let params = b.block_params(entry).to_vec();
        let returns = build(m, &mut b, &params)?;
        b.ins().return_(&returns);
        b.seal_all_blocks();
        b.finalize();
    }
    m.define_function(id, &mut ctx)?;
    m.clear_context(&mut ctx);
    Ok(())
}

fn compile_cranelift(path: &Path, manifest: &Manifest) -> Result<u64> {
    let start = Instant::now();
    let mut flags = settings::builder();
    flags.set("opt_level", "speed")?;
    let isa = cranelift_native::builder().map_err(|e| anyhow!(e))?.finish(settings::Flags::new(flags))?;
    let mut m = ObjectModule::new(ObjectBuilder::new(isa, "ogma-30", default_libcall_names())?);
    let mut u = m.make_signature();
    u.params.push(AbiParam::new(types::I64)); u.returns.push(AbiParam::new(types::I64));
    let mut b = m.make_signature();
    b.params.push(AbiParam::new(types::I64)); b.params.push(AbiParam::new(types::I64));
    b.returns.push(AbiParam::new(types::I64));
    let arithmetic = m.declare_function(workload_name(manifest, "arithmetic")?, Linkage::Export, &u)?;
    let branch = m.declare_function(workload_name(manifest, "control_flow")?, Linkage::Export, &u)?;
    let internal = m.declare_function(workload_name(manifest, "internal_callee")?, Linkage::Export, &u)?;
    let internal_call = m.declare_function(workload_name(manifest, "internal_call")?, Linkage::Export, &u)?;
    let dynamic = m.declare_function(workload_name(manifest, "dynamic_add")?, Linkage::Export, &b)?;
    let completion = m.declare_function(workload_name(manifest, "completion")?, Linkage::Export, &u)?;
    let host = m.declare_function(workload_name(manifest, "host_call")?, Linkage::Export, &u)?;
    let dyn_helper = m.declare_function("ogma_dynamic_add_helper", Linkage::Import, &b)?;
    let completion_helper = m.declare_function("ogma_completion_helper", Linkage::Import, &u)?;
    let host_helper = m.declare_function("ogma_host_call_helper", Linkage::Import, &u)?;

    define_clif(&mut m, arithmetic, u.clone(), |_, b, p| {
        let three = b.ins().iconst(types::I64, 3);
        let seven = b.ins().iconst(types::I64, 7);
        let v = b.ins().imul(p[0], three);
        Ok(vec![b.ins().iadd(v, seven)])
    })?;
    define_clif(&mut m, branch, u.clone(), |_, b, p| {
        let zero = b.ins().iconst(types::I64, 0);
        let cond = b.ins().icmp(IntCC::SignedLessThan, p[0], zero);
        let yes = b.create_block(); let no = b.create_block(); let merge = b.create_block();
        let result = b.append_block_param(merge, types::I64);
        b.ins().brif(cond, yes, &[], no, &[]);
        b.switch_to_block(yes);
        let neg = b.ins().ineg(p[0]); b.ins().jump(merge, &[neg]);
        b.switch_to_block(no);
        let two = b.ins().iconst(types::I64, 2);
        let dbl = b.ins().imul(p[0], two); b.ins().jump(merge, &[dbl]);
        b.switch_to_block(merge);
        Ok(vec![result])
    })?;
    define_clif(&mut m, internal, u.clone(), |_, b, p| {
        let two = b.ins().iconst(types::I64, 2);
        let one = b.ins().iconst(types::I64, 1);
        let doubled = b.ins().imul(p[0], two);
        Ok(vec![b.ins().iadd(doubled, one)])
    })?;
    define_clif(&mut m, internal_call, u.clone(), |m, b, p| {
        let f = m.declare_func_in_func(internal, b.func);
        let call = b.ins().call(f, &[p[0]]);
        Ok(vec![b.inst_results(call)[0]])
    })?;
    define_clif(&mut m, dynamic, b.clone(), |m, b, p| {
        let f = m.declare_func_in_func(dyn_helper, b.func);
        let call = b.ins().call(f, &[p[0], p[1]]);
        Ok(vec![b.inst_results(call)[0]])
    })?;
    define_clif(&mut m, completion, u.clone(), |m, b, p| {
        let f = m.declare_func_in_func(completion_helper, b.func);
        let call = b.ins().call(f, &[p[0]]);
        Ok(vec![b.inst_results(call)[0]])
    })?;
    define_clif(&mut m, host, u, |m, b, p| {
        let f = m.declare_func_in_func(host_helper, b.func);
        let call = b.ins().call(f, &[p[0]]);
        Ok(vec![b.inst_results(call)[0]])
    })?;
    fs::write(path, m.finish().emit()?)?;
    Ok(start.elapsed().as_nanos() as u64)
}

fn llvm_call<'ctx>(b: &inkwell::builder::Builder<'ctx>, f: inkwell::values::FunctionValue<'ctx>,
    args: &[inkwell::values::BasicMetadataValueEnum<'ctx>], name: &str) -> Result<IntValue<'ctx>> {
    Ok(b.build_call(f, args, name)?.try_as_basic_value().left()
        .context("call did not return a value")?.into_int_value())
}

fn compile_llvm(path: &Path, manifest: &Manifest) -> Result<u64> {
    let start = Instant::now();
    Target::initialize_native(&InitializationConfig::default())
        .map_err(|e| anyhow!("LLVM target init: {e}"))?;
    let triple = TargetMachine::get_default_triple();
    let target = Target::from_triple(&triple).map_err(|e| anyhow!(e.to_string()))?;
    let tm = target.create_target_machine(&triple, "generic", "", OptimizationLevel::Default,
        RelocMode::PIC, CodeModel::Default).context("create LLVM target machine")?;
    let cx = Context::create();
    let module = cx.create_module("ogma_ogm30");
    module.set_triple(&triple);
    module.set_data_layout(&tm.get_target_data().get_data_layout());
    let b = cx.create_builder();
    let i64t = cx.i64_type();
    let u = i64t.fn_type(&[i64t.into()], false);
    let pair = i64t.fn_type(&[i64t.into(), i64t.into()], false);

    let f = module.add_function(workload_name(manifest, "arithmetic")?, u, None);
    b.position_at_end(cx.append_basic_block(f, "entry"));
    let x = f.get_nth_param(0).unwrap().into_int_value();
    let v = b.build_int_mul(x, i64t.const_int(3, false), "mul")?;
    let v = b.build_int_add(v, i64t.const_int(7, false), "add")?;
    b.build_return(Some(&v))?;

    let f = module.add_function(workload_name(manifest, "control_flow")?, u, None);
    let entry = cx.append_basic_block(f, "entry");
    let yes = cx.append_basic_block(f, "negative");
    let no = cx.append_basic_block(f, "non_negative");
    let merge = cx.append_basic_block(f, "merge");
    b.position_at_end(entry);
    let x = f.get_nth_param(0).unwrap().into_int_value();
    let cond = b.build_int_compare(IntPredicate::SLT, x, i64t.const_zero(), "negative")?;
    b.build_conditional_branch(cond, yes, no)?;
    b.position_at_end(yes);
    let neg = b.build_int_neg(x, "neg")?;
    b.build_unconditional_branch(merge)?;
    let yes_end = b.get_insert_block().context("negative block missing")?;
    b.position_at_end(no);
    let dbl = b.build_int_mul(x, i64t.const_int(2, false), "double")?;
    b.build_unconditional_branch(merge)?;
    let no_end = b.get_insert_block().context("non-negative block missing")?;
    b.position_at_end(merge);
    let phi = b.build_phi(i64t, "result")?;
    phi.add_incoming(&[(&neg, yes_end), (&dbl, no_end)]);
    b.build_return(Some(&phi.as_basic_value().into_int_value()))?;

    let internal = module.add_function(workload_name(manifest, "internal_callee")?, u, None);
    b.position_at_end(cx.append_basic_block(internal, "entry"));
    let x = internal.get_nth_param(0).unwrap().into_int_value();
    let v = b.build_int_mul(x, i64t.const_int(2, false), "mul")?;
    let v = b.build_int_add(v, i64t.const_int(1, false), "add")?;
    b.build_return(Some(&v))?;
    let caller = module.add_function(workload_name(manifest, "internal_call")?, u, None);
    b.position_at_end(cx.append_basic_block(caller, "entry"));
    let x = caller.get_nth_param(0).unwrap().into_int_value();
    let v = llvm_call(&b, internal, &[x.into()], "call")?;
    b.build_return(Some(&v))?;

    let dh = module.add_function("ogma_dynamic_add_helper", pair, None);
    let caller = module.add_function(workload_name(manifest, "dynamic_add")?, pair, None);
    b.position_at_end(cx.append_basic_block(caller, "entry"));
    let a = caller.get_nth_param(0).unwrap().into_int_value();
    let c = caller.get_nth_param(1).unwrap().into_int_value();
    let v = llvm_call(&b, dh, &[a.into(), c.into()], "dynamic")?;
    b.build_return(Some(&v))?;
    let ch = module.add_function("ogma_completion_helper", u, None);
    let caller = module.add_function(workload_name(manifest, "completion")?, u, None);
    b.position_at_end(cx.append_basic_block(caller, "entry"));
    let x = caller.get_nth_param(0).unwrap().into_int_value();
    let v = llvm_call(&b, ch, &[x.into()], "completion")?;
    b.build_return(Some(&v))?;
    let hh = module.add_function("ogma_host_call_helper", u, None);
    let caller = module.add_function(workload_name(manifest, "host_call")?, u, None);
    b.position_at_end(cx.append_basic_block(caller, "entry"));
    let x = caller.get_nth_param(0).unwrap().into_int_value();
    let v = llvm_call(&b, hh, &[x.into()], "host")?;
    b.build_return(Some(&v))?;

    module.verify().map_err(|e| anyhow!("LLVM module verification: {e}"))?;
    tm.write_to_file(&module, FileType::Object, path)
        .map_err(|e| anyhow!("LLVM object emission: {e}"))?;
    Ok(start.elapsed().as_nanos() as u64)
}

const HELPERS: &str = r#"
#include <stdint.h>
int64_t ogma_dynamic_add_helper(int64_t a, int64_t b) { return a + b; }
int64_t ogma_completion_helper(int64_t x) { return x < 0 ? -1 : x; }
int64_t ogma_host_call_helper(int64_t x) { return x * 5 + 11; }
"#;

fn link(object: &Path, shared: &Path, helper: &Path) -> Result<u64> {
    fs::write(helper, HELPERS)?;
    let start = Instant::now();
    let out = Command::new("cc").args(["-shared", "-fPIC"]).arg(object)
        .arg("-x").arg("c").arg(helper).arg("-o").arg(shared).output()?;
    if !out.status.success() { bail!("native link failed: {}", String::from_utf8_lossy(&out.stderr)); }
    Ok(start.elapsed().as_nanos() as u64)
}

#[derive(Serialize)]
struct Distribution {
    p50_ns_per_call: f64, p95_ns_per_call: f64, p99_ns_per_call: f64,
    throughput_calls_per_second: f64, raw_ns_per_call_samples: Vec<f64>,
}
fn distribution(mut s: Vec<f64>) -> Distribution {
    s.sort_by(f64::total_cmp);
    let p = |q: f64| s[((s.len() - 1) as f64 * q).ceil() as usize];
    let median = p(0.5);
    Distribution { p50_ns_per_call: median, p95_ns_per_call: p(0.95), p99_ns_per_call: p(0.99),
        throughput_calls_per_second: 1e9 / median, raw_ns_per_call_samples: s }
}
fn symbol_name(name: &str) -> Vec<u8> {
    let mut bytes = name.as_bytes().to_vec();
    bytes.push(0);
    bytes
}
fn bench_unary(lib: &Library, name: &[u8], x: i64, o: &Options) -> Result<Distribution> {
    let f: Unary = unsafe { *lib.get(name)? };
    for _ in 0..o.warmups { for n in 0..o.iterations { black_box(unsafe { f(black_box(x + n as i64 % 31)) }); } }
    let mut samples = Vec::new();
    for _ in 0..o.samples {
        let start = Instant::now();
        for n in 0..o.iterations { black_box(unsafe { f(black_box(x + n as i64 % 31)) }); }
        samples.push(start.elapsed().as_nanos() as f64 / o.iterations as f64);
    }
    Ok(distribution(samples))
}
fn bench_binary(lib: &Library, name: &[u8], o: &Options) -> Result<Distribution> {
    let f: Binary = unsafe { *lib.get(name)? };
    for _ in 0..o.warmups { for n in 0..o.iterations { black_box(unsafe { f(black_box(n as i64), black_box(7)) }); } }
    let mut samples = Vec::new();
    for _ in 0..o.samples {
        let start = Instant::now();
        for n in 0..o.iterations { black_box(unsafe { f(black_box(n as i64), black_box(7)) }); }
        samples.push(start.elapsed().as_nanos() as f64 / o.iterations as f64);
    }
    Ok(distribution(samples))
}

#[derive(Serialize)]
struct BackendReport {
    compile_time_ns: Distribution, link_time_ns: u64, object_bytes: u64, shared_library_bytes: u64,
    workloads: serde_json::Value,
}
fn compile_rounds(backend: &str, root: &Path, manifest: &Manifest) -> Result<(Vec<f64>, PathBuf)> {
    let mut samples = Vec::new(); let mut last = PathBuf::new();
    for n in 0..5 {
        last = root.join(format!("{backend}-{n}.o"));
        let elapsed = if backend == "cranelift" { compile_cranelift(&last, manifest)? } else { compile_llvm(&last, manifest)? };
        samples.push(elapsed as f64);
    }
    Ok((samples, last))
}
fn run_backend(name: &str, o: &Options, root: &Path, manifest: &Manifest) -> Result<BackendReport> {
    let (compile, object) = compile_rounds(name, root, manifest)?;
    let shared = root.join(format!("{name}.so"));
    let helper = root.join(format!("{name}-helpers.c"));
    let link_time_ns = link(&object, &shared, &helper)?;
    let lib = unsafe { Library::new(&shared)? };
    let workloads = serde_json::json!({
        "arithmetic": bench_unary(&lib,&symbol_name(workload_name(manifest, "arithmetic")?),13,o)?,
        "control_flow": bench_unary(&lib,&symbol_name(workload_name(manifest, "control_flow")?),-13,o)?,
        "internal_call": bench_unary(&lib,&symbol_name(workload_name(manifest, "internal_call")?),13,o)?,
        "dynamic_operation_helper": bench_binary(&lib,&symbol_name(workload_name(manifest, "dynamic_add")?),o)?,
        "dynamic_helper_direct": bench_binary(&lib,b"ogma_dynamic_add_helper\0",o)?,
        "exception_completion_helper": bench_unary(&lib,&symbol_name(workload_name(manifest, "completion")?),-13,o)?,
        "host_call_helper": bench_unary(&lib,&symbol_name(workload_name(manifest, "host_call")?),13,o)?,
        "host_helper_direct": bench_unary(&lib,b"ogma_host_call_helper\0",13,o)?
    });
    let arithmetic: Unary = unsafe { *lib.get(workload_name(manifest, "arithmetic")?.as_bytes())? };
    let branch: Unary = unsafe { *lib.get(workload_name(manifest, "control_flow")?.as_bytes())? };
    let internal: Unary = unsafe { *lib.get(workload_name(manifest, "internal_call")?.as_bytes())? };
    let completion: Unary = unsafe { *lib.get(workload_name(manifest, "completion")?.as_bytes())? };
    let host: Unary = unsafe { *lib.get(workload_name(manifest, "host_call")?.as_bytes())? };
    let dynamic: Binary = unsafe { *lib.get(workload_name(manifest, "dynamic_add")?.as_bytes())? };
    for x in [-9_i64,-1,0,1,17,1024] {
        if unsafe { arithmetic(x) } != x.wrapping_mul(3).wrapping_add(7) { bail!("{name}: arithmetic mismatch"); }
        if unsafe { branch(x) } != if x < 0 { -x } else { x * 2 } { bail!("{name}: branch mismatch"); }
        if unsafe { internal(x) } != x.wrapping_mul(2).wrapping_add(1) { bail!("{name}: internal call mismatch"); }
        if unsafe { completion(x) } != if x < 0 { -1 } else { x } { bail!("{name}: completion mismatch"); }
        if unsafe { host(x) } != x.wrapping_mul(5).wrapping_add(11) { bail!("{name}: host helper mismatch"); }
    }
    if unsafe { dynamic(12,30) } != 42 { bail!("{name}: dynamic helper mismatch"); }
    Ok(BackendReport { compile_time_ns: distribution(compile), link_time_ns,
        object_bytes: fs::metadata(&object)?.len(), shared_library_bytes: fs::metadata(&shared)?.len(), workloads })
}

#[derive(Serialize)]
struct Report {
    schema_version: u32, task: &'static str, source_revision: String, tested_revision: String,
    rustc: String, llvm: String, clang: String, host_arch: &'static str, host_os: &'static str,
    iterations_per_sample: u64, warmup_rounds: usize, measurement_samples: usize,
    target_lookup: serde_json::Value, llvm_package_footprint_kib: Option<u64>,
    cranelift_crate_source_bytes: u64, llvm_binding_source_bytes: u64, cranelift_build_artifact_bytes: u64, llvm_binding_build_artifact_bytes: u64,
    integration_notes: serde_json::Value, cranelift: BackendReport, llvm_backend: BackendReport,
}
fn cmd(program: &str, args: &[&str]) -> String {
    Command::new(program).args(args).output().ok().filter(|x| x.status.success())
        .map(|x| String::from_utf8_lossy(&x.stdout).trim().into())
        .unwrap_or_else(|| "unavailable".into())
}
fn target_lookup() -> serde_json::Value {
    Target::initialize_all(&InitializationConfig::default());
    let triples = ["x86_64-unknown-linux-gnu","aarch64-unknown-linux-gnu",
        "x86_64-pc-windows-msvc","aarch64-apple-darwin"];
    serde_json::Value::Array(triples.iter().map(|name| {
        let t: Triple = name.parse().unwrap();
        serde_json::json!({"triple":name,
            "cranelift":cranelift_codegen::isa::lookup(t).is_ok(),
            "llvm":Target::from_triple(&TargetTriple::create(name)).is_ok(),
            "scope":"target lookup only; no cross-link/ABI compatibility claim"})
    }).collect())
}

fn directory_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else { return 0; };
    entries.flatten().map(|entry| {
        let path = entry.path();
        if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            directory_size(&path)
        } else {
            entry.metadata().map(|metadata| metadata.len()).unwrap_or(0)
        }
    }).sum()
}
fn cargo_source_size(prefixes: &[&str]) -> u64 {
    let home = env::var_os("CARGO_HOME").map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")));
    let Some(home) = home else { return 0; };
    let root = home.join("registry").join("src");
    let Ok(indices) = fs::read_dir(root) else { return 0; };
    indices.flatten().map(|index| {
        let Ok(crates) = fs::read_dir(index.path()) else { return 0; };
        crates.flatten().filter_map(|package| {
            let name = package.file_name().to_string_lossy().into_owned();
            prefixes.iter().any(|prefix| name.starts_with(prefix))
                .then(|| directory_size(&package.path()))
        }).sum::<u64>()
    }).sum()
}
fn cargo_artifact_size(prefix: &str) -> u64 {
    let root = env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("target"));
    let Ok(entries) = fs::read_dir(root.join("release").join("deps")) else { return 0; };
    entries.flatten().filter_map(|entry| {
        let name = entry.file_name().to_string_lossy().into_owned();
        name.starts_with(prefix).then(|| entry.metadata().map(|m| m.len()).unwrap_or(0))
    }).sum()
}

fn main() -> Result<()> {
    let o = options()?;
    let root = env::temp_dir().join(format!("ogm30-{}", std::process::id()));
    fs::create_dir_all(&root)?;
    let manifest = load_manifest()?;
    let cranelift = run_backend("cranelift",&o,&root,&manifest)?;
    let llvm_backend = run_backend("llvm",&o,&root,&manifest)?;
    let footprint = Command::new("dpkg-query")
        .args(["-W",r"-f=${Installed-Size}\n","llvm-18-dev","llvm-18","clang-18"])
        .output().ok().filter(|x|x.status.success())
        .map(|x|String::from_utf8_lossy(&x.stdout).lines().filter_map(|v|v.parse::<u64>().ok()).sum());
    let report = Report {
        schema_version:1, task:"OGM-30", source_revision:o.revision.clone(),
        tested_revision:env::var("GITHUB_SHA").unwrap_or_else(|_|o.revision.clone()),
        rustc:cmd("rustc",&["--version"]), llvm:cmd("llvm-config-18",&["--version"]), clang:cmd("clang-18",&["--version"]),
        host_arch:env::consts::ARCH, host_os:env::consts::OS,
        iterations_per_sample:o.iterations, warmup_rounds:o.warmups, measurement_samples:o.samples,
        target_lookup:target_lookup(), llvm_package_footprint_kib:footprint,
        cranelift_crate_source_bytes:cargo_source_size(&["cranelift-"]),
        llvm_binding_source_bytes:cargo_source_size(&["inkwell-","llvm-sys-181"]),
        cranelift_build_artifact_bytes:cargo_artifact_size("libcranelift_"),
        llvm_binding_build_artifact_bytes:cargo_artifact_size("libinkwell-") + cargo_artifact_size("libllvm_sys_181-"),
        integration_notes:serde_json::json!({
            "cranelift":"Rust crates; in-process ObjectModule lowering; no LLVM installation",
            "llvm":"Inkwell/llvm-sys and system LLVM 18 development toolchain; in-process target-machine emission",
            "runtime_boundary":"both backends call the same explicit C helper symbols; OGM-27 shadow-root maps are not emitted",
            "status":"M0 proof only; no production Ogma compiler or Runtime integration"
        }),
        cranelift, llvm_backend
    };
    if let Some(parent) = o.output.parent() { fs::create_dir_all(parent)?; }
    fs::write(&o.output,serde_json::to_vec_pretty(&report)?)?;
    println!("{}",serde_json::to_string_pretty(&report)?);
    let _ = fs::remove_dir_all(root);
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn benchmark_manifest_is_versioned_and_covers_required_workloads() {
        let manifest = load_manifest().unwrap();
        assert_eq!(manifest.functions.len(), 7);
        assert!(manifest.functions.iter().all(|f| !f.name.is_empty()));
    }
}
