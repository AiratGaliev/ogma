use ogma_rust_abi_proof::*;
use std::env;
use std::ffi::c_void;
use std::fmt::Write as _;
use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use std::ptr;
use std::time::Instant;

struct Config {
    iterations: usize,
    warmups: usize,
    samples: usize,
    revision: String,
    tested_revision: String,
    output: Option<PathBuf>,
}
struct Measurement {
    name: &'static str,
    items_per_call: usize,
    min_ns: f64,
    median_ns: f64,
    p95_ns: f64,
    p99_ns: f64,
    max_ns: f64,
}
fn parse_config() -> Result<Config, String> {
    let mut cfg = Config {
        iterations: 10_000,
        warmups: 3,
        samples: 101,
        revision: "unknown".into(),
        tested_revision: "unknown".into(),
        output: None,
    };
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {arg}"))?;
        match arg.as_str() {
            "--iterations" => {
                cfg.iterations = value
                    .parse()
                    .map_err(|_| "invalid --iterations".to_string())?
            }
            "--warmups" => {
                cfg.warmups = value.parse().map_err(|_| "invalid --warmups".to_string())?
            }
            "--samples" => {
                cfg.samples = value.parse().map_err(|_| "invalid --samples".to_string())?
            }
            "--revision" => cfg.revision = value,
            "--tested-revision" => cfg.tested_revision = value,
            "--output" => cfg.output = Some(PathBuf::from(value)),
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }
    if cfg.iterations == 0 || cfg.samples < 101 || cfg.warmups == 0 {
        return Err(
            "iterations and warmups must be > 0; samples must be >= 101 for p95/p99".into(),
        );
    }
    Ok(cfg)
}
fn percentile(sorted: &[f64], p: f64) -> f64 {
    let rank = ((sorted.len() as f64 * p).ceil() as usize).max(1);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}
fn measure<F: FnMut()>(
    name: &'static str,
    items_per_call: usize,
    cfg: &Config,
    mut f: F,
) -> Measurement {
    for _ in 0..cfg.warmups {
        for _ in 0..cfg.iterations {
            f();
        }
    }
    let mut per_call = Vec::with_capacity(cfg.samples);
    for _ in 0..cfg.samples {
        let start = Instant::now();
        for _ in 0..cfg.iterations {
            f();
        }
        per_call.push(start.elapsed().as_nanos() as f64 / cfg.iterations as f64);
    }
    per_call.sort_by(f64::total_cmp);
    Measurement {
        name,
        items_per_call,
        min_ns: per_call[0],
        median_ns: percentile(&per_call, 0.50),
        p95_ns: percentile(&per_call, 0.95),
        p99_ns: percentile(&per_call, 0.99),
        max_ns: *per_call.last().expect("non-empty sample set"),
    }
}
fn expect(status: OgmaStatus) {
    assert_eq!(
        status,
        OgmaStatus::OK,
        "ABI benchmark operation failed: {status:?}"
    );
}
unsafe extern "C-unwind" fn callback_identity(
    _runtime: *const OgmaRuntime,
    _context: OgmaContextHandle,
    args: *const OgmaHandle,
    args_len: usize,
    completion: *mut OgmaCompletion,
    _user_data: *mut c_void,
) -> OgmaStatus {
    if args_len != 1 || args.is_null() || completion.is_null() {
        return OgmaStatus::INVALID_ARGUMENT;
    }
    unsafe {
        completion.write(OgmaCompletion {
            kind: COMPLETION_NORMAL,
            reserved: 0,
            value: *args,
        });
    }
    OgmaStatus::OK
}
fn json_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 8);
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}
fn report(cfg: &Config, measurements: &[Measurement]) -> String {
    let version = ogma_abi_version();
    let mut out = String::new();
    let rustc = env::var("RUSTC_VERSION").unwrap_or_else(|_| "not supplied".into());
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let _ = writeln!(out, "{{");
    let _ = writeln!(out, "  \"schema_version\": 1,");
    let _ = writeln!(out, "  \"benchmark\": \"ogma-rust-abi-m0\",");
    let _ = writeln!(
        out,
        "  \"source_revision\": \"{}\",",
        json_escape(&cfg.revision)
    );
    let _ = writeln!(
        out,
        "  \"tested_revision\": \"{}\",",
        json_escape(&cfg.tested_revision)
    );
    let _ = writeln!(out, "  \"environment\": {{");
    let _ = writeln!(out, "    \"rustc\": \"{}\",", json_escape(&rustc));
    let _ = writeln!(out, "    \"os\": \"{}\",", env::consts::OS);
    let _ = writeln!(out, "    \"arch\": \"{}\",", env::consts::ARCH);
    let _ = writeln!(out, "    \"profile\": \"{}\",", profile);
    let _ = writeln!(out, "    \"iterations_per_sample\": {},", cfg.iterations);
    let _ = writeln!(out, "    \"warmup_rounds\": {},", cfg.warmups);
    let _ = writeln!(out, "    \"measurement_samples\": {}", cfg.samples);
    let _ = writeln!(out, "  }},");
    let _ = writeln!(
        out,
        "  \"abi\": {{\"major\": {}, \"minor\": {}, \"patch\": {}, \"features\": {}}},",
        version.major, version.minor, version.patch, version.features
    );
    let _ = writeln!(out, "  \"measurements\": [");
    for (index, m) in measurements.iter().enumerate() {
        let comma = if index + 1 == measurements.len() {
            ""
        } else {
            ","
        };
        let _ = writeln!(out,
            "    {{\"name\": \"{}\", \"unit\": \"ns/call\", \"items_per_call\": {}, \"min\": {:.3}, \"median\": {:.3}, \"p95\": {:.3}, \"p99\": {:.3}, \"max\": {:.3}, \"calls_per_second_at_median\": {:.3}}}{}",
            m.name, m.items_per_call, m.min_ns, m.median_ns, m.p95_ns, m.p99_ns, m.max_ns,
            1_000_000_000.0 / m.median_ns.max(f64::MIN_POSITIVE), comma);
    }
    let _ = writeln!(out, "  ],");
    let _ = writeln!(out, "  \"limitations\": [");
    let _ = writeln!(
        out,
        "    \"mock value store and callback dispatch only; no ECMAScript execution\","
    );
    let _ = writeln!(
        out,
        "    \"measurements are scoped to this M0 proof harness and host ABI boundary\","
    );
    let _ = writeln!(
        out,
        "    \"no production runtime throughput, allocator, GC or native-code claim\""
    );
    let _ = writeln!(out, "  ]");
    let _ = writeln!(out, "}}");
    out
}
fn run() -> Result<(), String> {
    let cfg = parse_config()?;
    let mut runtime = ptr::null_mut();
    expect(unsafe { ogma_runtime_create(&mut runtime) });
    let mut context = OgmaContextHandle::default();
    expect(unsafe { ogma_context_create(runtime, &mut context) });
    let mut scope = OgmaRootScopeHandle::default();
    expect(unsafe { ogma_root_scope_open(runtime, context, &mut scope) });

    let handles: Vec<OgmaHandle> = (0..64)
        .map(|index| {
            let mut handle = OgmaHandle::default();
            expect(unsafe {
                ogma_context_create_number(runtime, context, index as f64 + 1.0, &mut handle)
            });
            expect(unsafe { ogma_root_scope_add(runtime, context, scope, handle) });
            handle
        })
        .collect();

    let mut callback = OgmaCallbackHandle::default();
    expect(unsafe {
        ogma_callback_register(
            runtime,
            context,
            Some(callback_identity),
            ptr::null_mut(),
            &mut callback,
        )
    });
    let mut function = OgmaHandle::default();
    expect(unsafe { ogma_mock_function_create(runtime, context, callback, &mut function) });
    expect(unsafe { ogma_root_scope_add(runtime, context, scope, function) });

    let primitive = measure("primitive-f64-add", 1, &cfg, || {
        let mut value = 0.0;
        expect(unsafe { ogma_primitive_add_f64(black_box(19.0), black_box(23.0), &mut value) });
        black_box(value);
        assert_eq!(value, 42.0);
    });
    let checked_handle = measure("checked-handle-to-f64", 1, &cfg, || {
        let mut value = 0.0;
        expect(unsafe { ogma_context_get_number(runtime, context, handles[41], &mut value) });
        black_box(value);
        assert_eq!(value, 42.0);
    });
    let bulk = measure("bulk-64-checked-handles", handles.len(), &cfg, || {
        let mut sum = 0.0;
        expect(unsafe {
            ogma_context_sum_numbers(runtime, context, handles.as_ptr(), handles.len(), &mut sum)
        });
        black_box(sum);
        assert_eq!(sum, 2080.0);
    });
    let roundtrip = measure(
        "host-call-mock-function-callback-roundtrip",
        1,
        &cfg,
        || {
            let mut completion = OgmaCompletion::default();
            expect(unsafe {
                ogma_context_call(runtime, context, function, &handles[0], 1, &mut completion)
            });
            assert_eq!(completion.kind, COMPLETION_NORMAL);
            black_box(completion.value);
        },
    );

    expect(unsafe { ogma_root_scope_close(runtime, context, scope) });
    let mut runtime_slot = runtime;
    expect(unsafe { ogma_runtime_destroy(&mut runtime_slot) });
    let result = report(&cfg, &[primitive, checked_handle, bulk, roundtrip]);
    match cfg.output {
        Some(path) => {
            fs::write(&path, result).map_err(|e| format!("write {}: {e}", path.display()))?
        }
        None => print!("{result}"),
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("ogma-abi-bench: {error}");
        std::process::exit(2);
    }
}
