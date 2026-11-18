use ogma_buffer_interop_proof::{ArrayBuffer, BufferError, OwnedMemory, Runtime, TypedArray};
use std::hint::black_box;
use std::time::Instant;
fn arg(a: &[String], k: &str, d: &str) -> String {
    a.windows(2)
        .find(|w| w[0] == k)
        .map(|w| w[1].clone())
        .unwrap_or_else(|| d.to_owned())
}
fn checksum(b: &[u8]) -> u64 {
    b.iter().fold(0_u64, |s, x| s.wrapping_add(u64::from(*x)))
}
fn pct(v: &[f64], p: f64) -> f64 {
    v[((p * v.len() as f64).ceil() as usize).max(1).min(v.len()) - 1]
}
fn measure<F>(n: usize, w: usize, samples: usize, mut f: F) -> Result<Vec<f64>, BufferError>
where
    F: FnMut() -> Result<u64, BufferError>,
{
    for _ in 0..w {
        for _ in 0..n {
            black_box(f()?);
        }
    }
    let mut times = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start = Instant::now();
        let mut sink = 0_u64;
        for _ in 0..n {
            sink = sink.wrapping_add(black_box(f()?));
        }
        black_box(sink);
        times.push(start.elapsed().as_nanos() as f64 / n as f64);
    }
    times.sort_by(f64::total_cmp);
    Ok(times)
}
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let iterations = arg(&a, "--iterations", "2000")
        .parse::<usize>()
        .unwrap_or(2000)
        .max(1);
    let warmups = arg(&a, "--warmups", "3").parse::<usize>().unwrap_or(3);
    let samples = arg(&a, "--samples", "101")
        .parse::<usize>()
        .unwrap_or(101)
        .max(3);
    let revision = arg(&a, "--revision", "local");
    let tested = arg(&a, "--tested-revision", "local");
    let cpu = arg(&a, "--cpu", std::env::consts::ARCH);
    let output = arg(&a, "--output", "ogm-22-buffer-interop-report.json");
    let mut cases = Vec::new();
    for size in [64_usize, 4096, 1_048_576] {
        let mut m = OwnedMemory::zeroed(size, 64)?;
        let mut bytes = vec![0_u8; size];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        m.write(0, &bytes)?;
        let (buffer, _) = ArrayBuffer::take_ownership(m, size)?;
        let view = TypedArray::new(buffer, 0, size, 1, 1)?;
        let runtime = Runtime::new();
        let per_sample = (iterations.saturating_mul(64) / size).max(1);
        let copy = measure(per_sample, warmups, samples, || {
            let copied = view.copy_to_vec()?;
            Ok(checksum(black_box(&copied)))
        })?;
        let zero = measure(per_sample, warmups, samples, || {
            runtime.with_scoped_read(&view, |slice| checksum(black_box(slice)))
        })?;
        for (mode, v) in [("copy", copy), ("scoped_zero_copy", zero)] {
            let median = pct(&v, 0.50);
            let mib = (size as f64 / median) * 1_000_000_000.0 / (1024.0 * 1024.0);
            cases.push(format!(
                r#"{{"size_bytes":{size},"mode":"{mode}","iterations_per_sample":{per_sample},"samples":{},"min_ns_per_op":{:.3},"median_ns_per_op":{median:.3},"p95_ns_per_op":{:.3},"p99_ns_per_op":{:.3},"max_ns_per_op":{:.3},"throughput_mib_s":{mib:.3}}}"#,
                v.len(),v[0],pct(&v,0.95),pct(&v,0.99),v[v.len()-1]));
        }
    }
    let report = format!(
        r#"{{"task":"OGM-22","revision":"{}","tested_revision":"{}","rustc_version":"{}","os":"{}","arch":"{}","cpu":"{}","profile":"release","warmups":{},"measurement_samples":{},"base_iterations":{},"method":"nearest-rank percentiles over per-sample mean latency; equivalent checksums","cases":[{}]}}"#,
        esc(&revision),
        esc(&tested),
        esc(&std::env::var("RUSTC_VERSION").unwrap_or_else(|_| "unknown".into())),
        esc(std::env::consts::OS),
        esc(std::env::consts::ARCH),
        esc(&cpu),
        warmups,
        samples,
        iterations,
        cases.join(",")
    );
    std::fs::write(&output, report)?;
    println!("Wrote {output}");
    Ok(())
}
