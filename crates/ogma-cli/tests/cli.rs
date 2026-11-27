//! End-to-end command-line integration tests for the persisted M1 compilation unit.

use ogma_compiler::compile_to_bytecode;
use ogma_embedding::{
    Engine, EngineConfig, ExecutionRequest, ModuleResolver, ModuleResolverError, ResolvedModule,
};
use ogma_frontend::{ParseGoal, ParseMode, ParseRequestOptions, SourceFile, parse};
use oxc_allocator::Allocator;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Stdio;
use std::process::{Command, Output};
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::{Duration, Instant};

static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let id = NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("ogma-cli-test-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).expect("create temp directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ogma"))
        .args(args)
        .output()
        .expect("run ogma subprocess")
}

fn compile(source: &Path, output: &Path, mode: Option<&str>) -> Output {
    let source = source.to_string_lossy();
    let output = output.to_string_lossy();
    let mut args = vec!["compile", source.as_ref(), "-o", output.as_ref()];
    if let Some(mode) = mode {
        args.push(mode);
    }
    cli(&args)
}

#[test]
fn script_compile_inspect_and_run_use_a_persisted_ogmb_artifact() {
    let directory = TestDirectory::new();
    let source = directory.path().join("main.js");
    let artifact = directory.path().join("program.ogmb");
    fs::write(
        &source,
        "function add(value) { return value + 2; }\nlet value = add(40);\n",
    )
    .expect("write source");

    let compiled = compile(&source, &artifact, None);
    assert!(
        compiled.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    assert!(artifact.is_file());
    assert!(artifact.metadata().expect("artifact metadata").len() > 48);

    let inspected = cli(&["inspect", artifact.to_str().expect("UTF-8 path")]);
    assert!(
        inspected.status.success(),
        "inspect failed: {}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&inspected.stdout).expect("inspection JSON");
    assert_eq!(report["container"]["format"], "ogma.ogmb");
    assert_eq!(report["container"]["version"], "1.0");
    assert_eq!(report["execution_model"], "register");
    assert!(report["container"]["instructions"].as_u64().unwrap() > 0);
    assert!(report["source"]["files"].as_array().is_some());
    assert!(
        report["compilation_unit"]["function_count"]
            .as_u64()
            .unwrap()
            > 1
    );

    let executed = cli(&[
        "run",
        artifact.to_str().expect("UTF-8 path"),
        "--heap-bytes",
        "1048576",
        "--stack-bytes",
        "1048576",
        "--call-depth",
        "64",
        "--fuel",
        "500000",
        "--timeout-ms",
        "10000",
        "--job-capacity",
        "128",
    ]);
    assert!(
        executed.status.success(),
        "run failed: {}",
        String::from_utf8_lossy(&executed.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&executed.stdout).expect("run JSON");
    assert_eq!(result["completion"], "return");
}

#[test]
fn esm_entry_graph_supports_relative_imports_cycles_and_live_bindings() {
    let directory = TestDirectory::new();
    let entry = directory.path().join("entry.mjs");
    let dependency = directory.path().join("dependency.mjs");
    let artifact = directory.path().join("entry.ogmb");
    fs::write(
        &entry,
        r#"import { value, increment, readyState } from "./dependency.mjs";
export let ready = false;
increment();
ready = true;
export const result = value + 1;
export const observedReady = readyState();
"#,
    )
    .expect("write entry module");
    fs::write(
        &dependency,
        r#"import { ready } from "./entry.mjs";
export let value = 40;
export function increment() { value = value + 2; }
export function readyState() { return ready; }
"#,
    )
    .expect("write dependency");

    let compiled = compile(&entry, &artifact, None);
    assert!(
        compiled.status.success(),
        "module compile failed: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let inspected = cli(&["inspect", artifact.to_str().expect("UTF-8 path")]);
    assert!(inspected.status.success());
    let inspection: serde_json::Value =
        serde_json::from_slice(&inspected.stdout).expect("inspection JSON");
    assert_eq!(
        inspection["compilation_unit"]["module"]["imports"][0]["specifier"],
        "./dependency.mjs"
    );
    assert_eq!(inspection["compilation_unit"]["dependency_module_count"], 1);
    assert_eq!(
        inspection["compilation_unit"]["source"]["path"],
        "entry.mjs"
    );
    assert_eq!(
        inspection["compilation_unit"]["dependencies"][0]["source"]["path"],
        "dependency.mjs"
    );

    let executed = cli(&["run", artifact.to_str().expect("UTF-8 path")]);
    assert!(
        executed.status.success(),
        "ESM run failed: {}",
        String::from_utf8_lossy(&executed.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&executed.stdout).expect("run JSON");
    assert_eq!(result["completion"], "evaluated");
    assert_eq!(result["modules"].as_array().map(Vec::len), Some(2));
    assert_eq!(result["exports"]["result"].as_f64(), Some(43.0));
    assert_eq!(result["exports"]["ready"].as_bool(), Some(true));
    assert_eq!(result["exports"]["observedReady"].as_bool(), Some(true));

    let direct_result = evaluate_with_public_embedding_api(&entry, &dependency);
    assert_eq!(result["exports"]["result"].as_f64(), Some(direct_result));

    fs::remove_file(&entry).expect("remove entry source after compilation");
    fs::remove_file(&dependency).expect("remove dependency source after compilation");
    let offline_run = cli(&["run", artifact.to_str().expect("UTF-8 path")]);
    assert!(
        offline_run.status.success(),
        "persisted ESM compilation unit depended on source files: {}",
        String::from_utf8_lossy(&offline_run.stderr)
    );
    let offline_result: serde_json::Value =
        serde_json::from_slice(&offline_run.stdout).expect("offline run JSON");
    assert_eq!(offline_result["exports"]["result"].as_f64(), Some(43.0));
    assert_eq!(
        offline_result["exports"]["observedReady"].as_bool(),
        Some(true)
    );
}

#[test]
fn standalone_verified_ogmb_without_cli_sidecar_remains_runnable() {
    let directory = TestDirectory::new();
    let source = directory.path().join("main.js");
    let artifact = directory.path().join("standalone.ogmb");
    fs::write(&source, "let value = 40 + 2;\n").expect("write source");
    let compiled = compile(&source, &artifact, None);
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    fs::remove_dir_all(artifact.with_file_name("standalone.ogmb.d"))
        .expect("remove compilation-unit sidecar");

    let result = cli(&["run", artifact.to_str().expect("UTF-8 path")]);
    assert!(
        result.status.success(),
        "standalone OGMB failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn configured_heap_quota_is_enforced_by_the_production_runtime() {
    let directory = TestDirectory::new();
    let source = directory.path().join("allocate.js");
    let artifact = directory.path().join("allocate.ogmb");
    fs::write(
        &source,
        "let result = \"this string must be allocated in the Runtime heap\";\n",
    )
    .expect("write allocating source");
    let compiled = compile(&source, &artifact, None);
    assert!(
        compiled.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );

    let result = cli(&[
        "run",
        artifact.to_str().expect("UTF-8 path"),
        "--heap-bytes",
        "1",
    ]);
    assert_eq!(
        result.status.code(),
        Some(6),
        "one-byte heap quota was not enforced: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn malformed_and_unsupported_artifacts_are_rejected_before_runtime_execution() {
    let directory = TestDirectory::new();
    let source = directory.path().join("main.js");
    let artifact = directory.path().join("program.ogmb");
    let corrupt = directory.path().join("corrupt.ogmb");
    fs::write(&source, "let value = 1;\n").expect("write source");
    let compiled = compile(&source, &artifact, None);
    assert!(compiled.status.success());
    let bytes = fs::read(&artifact).expect("read artifact");
    fs::write(&corrupt, &bytes[..bytes.len().min(20)]).expect("write truncated artifact");
    let empty = directory.path().join("empty.ogmb");
    fs::write(&empty, []).expect("write empty artifact");

    let empty_run = cli(&["run", empty.to_str().expect("UTF-8 path")]);
    assert_eq!(empty_run.status.code(), Some(5));
    let empty_inspect = cli(&["inspect", empty.to_str().expect("UTF-8 path")]);
    assert_eq!(empty_inspect.status.code(), Some(5));

    let result = cli(&["run", corrupt.to_str().expect("UTF-8 path")]);
    assert_eq!(result.status.code(), Some(5));
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("container.truncated_header"),
        "unexpected diagnostic: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let mut unsupported = bytes;
    unsupported[4..6].copy_from_slice(&2_u16.to_le_bytes());
    let unsupported_path = directory.path().join("unsupported.ogmb");
    fs::write(&unsupported_path, unsupported).expect("write unsupported version");
    let result = cli(&["run", unsupported_path.to_str().expect("UTF-8 path")]);
    assert_eq!(result.status.code(), Some(5));

    let mut unsupported_features = fs::read(&artifact).expect("read valid artifact");
    let feature_flags = u64::from_le_bytes(
        unsupported_features[12..20]
            .try_into()
            .expect("feature flag header"),
    );
    unsupported_features[12..20].copy_from_slice(&(feature_flags | (1_u64 << 63)).to_le_bytes());
    let feature_path = directory.path().join("unsupported-features.ogmb");
    fs::write(&feature_path, unsupported_features).expect("write unsupported feature artifact");
    let result = cli(&["run", feature_path.to_str().expect("UTF-8 path")]);
    assert_eq!(result.status.code(), Some(5));
}

#[test]
fn compilation_unit_rejects_helper_path_traversal_before_reading_outside_files() {
    let directory = TestDirectory::new();
    let source = directory.path().join("main.js");
    let artifact = directory.path().join("program.ogmb");
    fs::write(
        &source,
        "function add(value) { return value + 2; }\nlet value = add(40);\n",
    )
    .expect("write source");
    let compiled = compile(&source, &artifact, None);
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );

    let sidecar = artifact.with_file_name("program.ogmb.d");
    let manifest_path = sidecar.join("unit.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("read unit manifest"))
            .expect("parse unit manifest");
    let functions = manifest["functions"]
        .as_array_mut()
        .expect("functions array");
    let helper = functions
        .iter_mut()
        .find(|item| item["file"].is_string())
        .expect("helper artifact");
    helper["file"] = serde_json::Value::String("../outside.ogmb".to_owned());
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("encode manifest"),
    )
    .expect("tamper manifest");

    let inspected = cli(&["inspect", artifact.to_str().expect("UTF-8 path")]);
    assert_eq!(inspected.status.code(), Some(5));
    assert!(
        String::from_utf8_lossy(&inspected.stderr).contains("unsafe helper artifact path"),
        "unexpected diagnostic: {}",
        String::from_utf8_lossy(&inspected.stderr)
    );
}

#[test]
fn module_compilation_rejects_imports_that_escape_the_entry_directory() {
    let directory = TestDirectory::new();
    let entry_directory = directory.path().join("entry");
    fs::create_dir_all(&entry_directory).expect("create entry directory");
    let entry = entry_directory.join("entry.mjs");
    let outside = directory.path().join("outside.mjs");
    let artifact = directory.path().join("escaped.ogmb");
    fs::write(&entry, "import \"../outside.mjs\";\n").expect("write entry");
    fs::write(&outside, "export default 1;\n").expect("write outside dependency");

    let result = compile(&entry, &artifact, None);
    assert_eq!(result.status.code(), Some(4));
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("outside the entry source directory"),
        "unexpected diagnostic: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!artifact.exists());
}

#[test]
fn compile_failure_keeps_source_location_and_returns_compile_exit_code() {
    let directory = TestDirectory::new();
    let source = directory.path().join("broken.js");
    let artifact = directory.path().join("broken.ogmb");
    fs::write(&source, "const = ;\n").expect("write source");

    let result = compile(&source, &artifact, None);
    assert_eq!(result.status.code(), Some(4));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("broken.js:1:"),
        "diagnostic lost source location: {stderr}"
    );
    assert!(!artifact.exists());
}

#[test]
fn runtime_throw_and_instruction_budget_have_distinct_exit_codes() {
    let directory = TestDirectory::new();
    let throwing_source = directory.path().join("throw.js");
    let throwing_artifact = directory.path().join("throw.ogmb");
    fs::write(&throwing_source, "throw 42;\n").expect("write throwing source");
    let compiled = compile(&throwing_source, &throwing_artifact, None);
    assert!(compiled.status.success());

    let thrown = cli(&["run", throwing_artifact.to_str().expect("UTF-8 path")]);
    assert_eq!(thrown.status.code(), Some(6));
    assert!(String::from_utf8_lossy(&thrown.stderr).contains("42"));

    let loop_source = directory.path().join("loop.js");
    let loop_artifact = directory.path().join("loop.ogmb");
    fs::write(&loop_source, "while (true) {}\n").expect("write loop source");
    let compiled = compile(&loop_source, &loop_artifact, None);
    assert!(compiled.status.success());

    let interrupted = cli(&[
        "run",
        loop_artifact.to_str().expect("UTF-8 path"),
        "--fuel",
        "32",
    ]);
    assert_eq!(interrupted.status.code(), Some(7));

    let deadline = cli(&[
        "run",
        loop_artifact.to_str().expect("UTF-8 path"),
        "--timeout-ms",
        "1",
    ]);
    assert_eq!(deadline.status.code(), Some(7));

    let stack_limit = cli(&[
        "run",
        loop_artifact.to_str().expect("UTF-8 path"),
        "--stack-bytes",
        "1",
    ]);
    assert_eq!(stack_limit.status.code(), Some(7));
}

#[test]
fn configured_call_depth_is_enforced_by_the_production_runtime() {
    let directory = TestDirectory::new();
    let source = directory.path().join("recursive.js");
    let artifact = directory.path().join("recursive.ogmb");
    fs::write(
        &source,
        "function recurse(value) { return recurse(value + 1); }\nrecurse(0);\n",
    )
    .expect("write recursive source");
    let compiled = compile(&source, &artifact, None);
    assert!(
        compiled.status.success(),
        "recursive compile failed: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );

    let result = cli(&[
        "run",
        artifact.to_str().expect("UTF-8 path"),
        "--call-depth",
        "4",
    ]);
    assert_eq!(result.status.code(), Some(7));
}

#[test]
fn promise_job_call_depth_limit_uses_interrupted_exit_code() {
    let directory = TestDirectory::new();
    let source = directory.path().join("promise-recursion.js");
    let artifact = directory.path().join("promise-recursion.ogmb");
    fs::write(
        &source,
        "function recurse(value) { return recurse(value + 1); }\nPromise.resolve(0).then(function () { recurse(0); });\n",
    )
    .expect("write Promise callback source");

    let compiled = compile(&source, &artifact, None);
    assert!(
        compiled.status.success(),
        "Promise callback compile failed: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );

    let result = cli(&[
        "run",
        artifact.to_str().expect("UTF-8 path"),
        "--call-depth",
        "4",
    ]);
    assert_eq!(
        result.status.code(),
        Some(7),
        "Promise callback call-depth exhaustion must use the interruption exit code: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn promise_job_capacity_is_enforced_by_the_production_runtime() {
    let directory = TestDirectory::new();
    let source = directory.path().join("promise-capacity.js");
    let artifact = directory.path().join("promise-capacity.ogmb");
    fs::write(
        &source,
        "Promise.resolve(0).then(function () {});\nPromise.resolve(1).then(function () {});\n",
    )
    .expect("write Promise queue capacity source");

    let compiled = compile(&source, &artifact, None);
    assert!(
        compiled.status.success(),
        "Promise capacity fixture compile failed: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );

    let result = cli(&[
        "run",
        artifact.to_str().expect("UTF-8 path"),
        "--job-capacity",
        "1",
    ]);
    assert_eq!(
        result.status.code(),
        Some(6),
        "a second pending PromiseJob must exceed the one-job Runtime capacity: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn zero_runtime_limits_are_rejected_before_execution() {
    let result = cli(&["run", "missing.ogmb", "--job-capacity", "0"]);
    assert_eq!(result.status.code(), Some(2));
}

#[cfg(unix)]
#[test]
fn sigint_requests_cooperative_runtime_cancellation() {
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }

    let directory = TestDirectory::new();
    let source = directory.path().join("loop.js");
    let artifact = directory.path().join("loop.ogmb");
    fs::write(&source, "while (true) {}\n").expect("write loop source");
    let compiled = compile(&source, &artifact, None);
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );

    let started = Instant::now();
    let child = Command::new(env!("CARGO_BIN_EXE_ogma"))
        .args([
            "run",
            artifact.to_str().expect("UTF-8 path"),
            "--fuel",
            "999999999999",
            "--timeout-ms",
            "10000",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ogma process");
    thread::sleep(Duration::from_millis(150));
    let sent = unsafe { kill(child.id() as i32, 2) };
    assert_eq!(sent, 0, "failed to send SIGINT to ogma process");
    let output = child.wait_with_output().expect("wait for ogma");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "SIGINT cancellation did not stop the process promptly"
    );
    assert_eq!(
        output.status.code(),
        Some(7),
        "SIGINT was not converted to Runtime cancellation: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn module_identity(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).expect("module is below graph root");
    let stable_path = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    format!(
        "source_{}",
        ogma_bytecode::sha256_hex(stable_path.as_bytes())
    )
}

fn compile_module(root: &Path, path: &Path) -> ResolvedModule {
    let canonical = fs::canonicalize(path).expect("canonical module path");
    let identity = module_identity(root, &canonical);
    let uri = format!("memory:///cli/{identity}/source.js");
    let text = fs::read_to_string(&canonical).expect("read module source");
    let source = SourceFile::new(identity, uri, None, text).expect("source metadata");
    let allocator = Allocator::default();
    let parsed = parse(
        &allocator,
        &source,
        ParseRequestOptions {
            goal: ParseGoal::Module,
            mode: ParseMode::Sloppy,
        },
    )
    .expect("parse module");
    let compiled = compile_to_bytecode(&parsed).expect("compile module");
    ResolvedModule {
        descriptor: compiled.module.expect("module descriptor"),
        artifacts: compiled
            .functions
            .into_iter()
            .map(|item| item.artifact.bytes)
            .collect(),
    }
}

struct DirectResolver {
    root: PathBuf,
    modules: HashMap<String, ResolvedModule>,
    sources: HashMap<String, PathBuf>,
}

impl ModuleResolver for DirectResolver {
    fn resolve(
        &mut self,
        referrer_module_id: &str,
        specifier: &str,
    ) -> Result<String, ModuleResolverError> {
        if !(specifier.starts_with("./") || specifier.starts_with("../")) {
            return Err(ModuleResolverError::new(
                "test resolver accepts relative paths only",
            ));
        }
        let referrer = self
            .sources
            .get(referrer_module_id)
            .ok_or_else(|| ModuleResolverError::new("unknown referrer identity"))?;
        let candidate = referrer.parent().expect("referrer parent").join(specifier);
        let canonical = fs::canonicalize(&candidate)
            .map_err(|error| ModuleResolverError::new(error.to_string()))?;
        if !canonical.starts_with(&self.root) {
            return Err(ModuleResolverError::new("module escapes test root"));
        }
        Ok(module_identity(&self.root, &canonical))
    }

    fn load(&mut self, module_id: &str) -> Result<ResolvedModule, ModuleResolverError> {
        self.modules
            .get(module_id)
            .cloned()
            .ok_or_else(|| ModuleResolverError::new(format!("unknown test module {module_id}")))
    }
}

fn evaluate_with_public_embedding_api(entry: &Path, dependency: &Path) -> f64 {
    let entry = fs::canonicalize(entry).expect("entry canonical path");
    let dependency = fs::canonicalize(dependency).expect("dependency canonical path");
    let root = entry.parent().expect("entry parent").to_path_buf();
    let entry_module = compile_module(&root, &entry);
    let dependency_module = compile_module(&root, &dependency);
    let entry_id = entry_module.descriptor.id.clone();
    let dependency_id = dependency_module.descriptor.id.clone();
    let mut resolver = DirectResolver {
        root,
        modules: HashMap::from([
            (entry_id.clone(), entry_module),
            (dependency_id.clone(), dependency_module),
        ]),
        sources: HashMap::from([
            (entry_id.clone(), entry.clone()),
            (dependency_id, dependency.clone()),
        ]),
    };
    let engine = Engine::new(EngineConfig::default()).expect("Engine");
    let mut runtime = engine.create_runtime().expect("Runtime");
    let mut context = runtime.create_context().expect("Context");
    let report = context
        .evaluate_module_graph(&entry_id, &mut resolver, ExecutionRequest::default())
        .expect("evaluate module graph through embedding API");
    assert_eq!(report.failure, None);
    let jobs = context
        .run_promise_jobs_until_idle(Some(1024))
        .expect("pump jobs");
    assert_eq!(jobs.outcome, ogma_embedding::JobPumpOutcome::Idle);
    let handle = context
        .read_module_export(&entry_id, "result")
        .expect("read result export");
    let value = context.resolve(handle).expect("resolve result export");
    let number = value.as_number().expect("numeric result export");
    context.release(handle).expect("release export root");
    number
}
