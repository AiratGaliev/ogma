use ogma_frame_snapshot_proof::{
    CommandBuffer, FrameSnapshot, HostCommand, HostEcsSync, HostFrame, PhysicsPoseCache,
    PositionChange, ScriptHarness, UpdateMode, CGE_REFERENCE_REVISION,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

static TRACK_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOCATION_COUNT: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

struct CountingAllocator;

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwards the original layout unchanged to the system allocator.
        let pointer = unsafe { System.alloc(layout) };
        record_allocation(layout.size(), !pointer.is_null());
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwards the original layout unchanged to the system allocator.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        record_allocation(layout.size(), !pointer.is_null());
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: pointer and layout originate from the system allocator.
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwards the original pointer, layout and requested size.
        let resized = unsafe { System.realloc(pointer, layout, new_size) };
        record_allocation(new_size, !resized.is_null());
        resized
    }
}

fn record_allocation(bytes: usize, success: bool) {
    if success && TRACK_ALLOCATIONS.load(Ordering::Relaxed) {
        ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
    }
}

#[derive(Clone, Copy, Debug)]
struct Measurement {
    p50: f64,
    p95: f64,
    p99: f64,
    min: f64,
    max: f64,
    allocations_per_op: f64,
    bytes_per_op: f64,
}

#[derive(Clone, Debug)]
struct Record {
    phase: String,
    mode: String,
    entity_count: usize,
    changed_entity_count: Option<usize>,
    item_count: usize,
    host_callbacks_per_update: usize,
    iterations_per_sample: usize,
    samples: usize,
    metrics: Measurement,
}

impl Record {
    fn to_json(&self) -> String {
        let changed = self
            .changed_entity_count
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_owned());
        format!(
            "{{\"phase\":\"{}\",\"mode\":\"{}\",\"entity_count\":{},\"changed_entity_count\":{},\"item_count\":{},\"host_callbacks_per_update\":{},\"iterations_per_sample\":{},\"samples\":{},\"p50_ns_per_op\":{:.3},\"p95_ns_per_op\":{:.3},\"p99_ns_per_op\":{:.3},\"min_ns_per_op\":{:.3},\"max_ns_per_op\":{:.3},\"rust_allocations_per_op\":{:.6},\"rust_allocated_bytes_per_op\":{:.3}}}",
            escape_json(&self.phase),
            escape_json(&self.mode),
            self.entity_count,
            changed,
            self.item_count,
            self.host_callbacks_per_update,
            self.iterations_per_sample,
            self.samples,
            self.metrics.p50,
            self.metrics.p95,
            self.metrics.p99,
            self.metrics.min,
            self.metrics.max,
            self.metrics.allocations_per_op,
            self.metrics.bytes_per_op,
        )
    }
}

fn measure<F>(
    iterations: usize,
    warmups: usize,
    samples: usize,
    mut operation: F,
) -> Result<Measurement, String>
where
    F: FnMut() -> Result<u64, String>,
{
    for _ in 0..warmups {
        let mut checksum = 0_u64;
        for _ in 0..iterations {
            checksum = checksum.wrapping_add(black_box(operation()?));
        }
        black_box(checksum);
    }

    let mut latencies = Vec::with_capacity(samples);
    let mut total_allocations = 0_u64;
    let mut total_bytes = 0_u64;
    for _ in 0..samples {
        ALLOCATION_COUNT.store(0, Ordering::Relaxed);
        ALLOCATED_BYTES.store(0, Ordering::Relaxed);
        TRACK_ALLOCATIONS.store(true, Ordering::Relaxed);

        let started = Instant::now();
        let mut checksum = 0_u64;
        for _ in 0..iterations {
            match operation() {
                Ok(value) => checksum = checksum.wrapping_add(black_box(value)),
                Err(error) => {
                    TRACK_ALLOCATIONS.store(false, Ordering::Relaxed);
                    return Err(error);
                }
            }
        }
        let elapsed = started.elapsed();
        TRACK_ALLOCATIONS.store(false, Ordering::Relaxed);
        black_box(checksum);

        latencies.push(elapsed.as_nanos() as f64 / iterations as f64);
        total_allocations += ALLOCATION_COUNT.load(Ordering::Relaxed);
        total_bytes += ALLOCATED_BYTES.load(Ordering::Relaxed);
    }

    latencies.sort_by(f64::total_cmp);
    let percentile = |p: f64| {
        let rank = ((p * latencies.len() as f64).ceil() as usize).max(1);
        latencies[rank.min(latencies.len()) - 1]
    };
    let total_ops = (iterations * samples) as f64;
    Ok(Measurement {
        p50: percentile(0.50),
        p95: percentile(0.95),
        p99: percentile(0.99),
        min: latencies[0],
        max: latencies[latencies.len() - 1],
        allocations_per_op: total_allocations as f64 / total_ops,
        bytes_per_op: total_bytes as f64 / total_ops,
    })
}

fn iterations_for(base: usize, entity_count: usize) -> usize {
    base.saturating_mul(40)
        .checked_div(entity_count.max(1))
        .unwrap_or(1)
        .max(1)
}

fn command_drain_records(
    records: &mut Vec<Record>,
    command_counts: &[usize],
    base_iterations: usize,
    warmups: usize,
    samples: usize,
) -> Result<(), String> {
    // Each pre-filled command buffer is drained exactly once. Command payloads
    // include owned strings, matching the destruction cost of CGE RuntimeCommand.
    let iterations = base_iterations.max(10);
    for command_count in command_counts {
        let total_frames = iterations
            .checked_mul(warmups + samples)
            .ok_or_else(|| "command workload size overflow".to_owned())?;
        let mut frames = (0..total_frames)
            .map(|frame| CommandBuffer::with_commands(*command_count, frame as u32))
            .collect::<Vec<_>>();
        let mut output: Vec<HostCommand> = Vec::with_capacity(*command_count);
        let mut next_frame = 0_usize;
        let metrics = measure(iterations, warmups, samples, || {
            let frame = frames
                .get_mut(next_frame)
                .ok_or_else(|| "command frame pool exhausted".to_owned())?;
            next_frame += 1;
            frame.drain_into(&mut output);
            Ok(black_box(output.len() as u64))
        })?;
        records.push(Record {
            phase: "command_drain_reuse".to_owned(),
            mode: "clear_and_swap_prebuilt_commands".to_owned(),
            entity_count: 0,
            changed_entity_count: None,
            item_count: *command_count,
            host_callbacks_per_update: 0,
            iterations_per_sample: iterations,
            samples,
            metrics,
        });
    }
    Ok(())
}

fn main() -> Result<(), String> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let iterations = parse_argument(&arguments, "--iterations", 1000)?;
    let warmups = parse_argument(&arguments, "--warmups", 3)?;
    let samples = parse_argument(&arguments, "--samples", 101)?;
    if iterations == 0 || samples < 3 {
        return Err("--iterations must be positive and --samples must be at least 3".to_owned());
    }
    let revision = string_argument(&arguments, "--revision", "local");
    let tested_revision = string_argument(&arguments, "--tested-revision", "local");
    let cpu = string_argument(&arguments, "--cpu", std::env::consts::ARCH);
    let output = string_argument(&arguments, "--output", "ogm-23-frame-snapshot-report.json");

    let mut records = Vec::new();
    for entity_count in [40_usize, 256, 1024] {
        let collision_count = (entity_count / 8).max(1);
        let operation_iterations = iterations_for(iterations, entity_count);
        let host_frame = HostFrame::fixture(1, entity_count, collision_count);
        let initial_snapshot = FrameSnapshot::from_host_frame(&host_frame);

        // Snapshot construction: fresh clone versus in-place refresh with reused maps/vectors.
        let copy_metrics = measure(operation_iterations, warmups, samples, || {
            let snapshot = FrameSnapshot::from_host_frame(&host_frame);
            Ok(black_box(snapshot.positions().len() as u64))
        })?;
        records.push(Record {
            phase: "frame_snapshot_construction".to_owned(),
            mode: "fresh_owned_copy".to_owned(),
            entity_count,
            changed_entity_count: None,
            item_count: entity_count + collision_count,
            host_callbacks_per_update: 0,
            iterations_per_sample: operation_iterations,
            samples,
            metrics: copy_metrics,
        });

        let mut reusable_snapshot = initial_snapshot.clone();
        let mut source_frame = host_frame.clone();
        let mut frame_id = 1_u64;
        let reuse_metrics = measure(operation_iterations, warmups, samples, || {
            frame_id = frame_id.wrapping_add(1);
            source_frame.frame_id = frame_id;
            reusable_snapshot.refresh_from(&source_frame);
            Ok(black_box(reusable_snapshot.frame_id()))
        })?;
        records.push(Record {
            phase: "frame_snapshot_construction".to_owned(),
            mode: "reused_host_buffers".to_owned(),
            entity_count,
            changed_entity_count: None,
            item_count: entity_count + collision_count,
            host_callbacks_per_update: 0,
            iterations_per_sample: operation_iterations,
            samples,
            metrics: reuse_metrics,
        });

        // Snapshot handoff is measured separately from construction and JS execution.
        // Both source snapshots are prepared before timing; the measured operation
        // is one synchronous publish into the Runtime-owned frame snapshot.
        let mut alternate_host_frame = host_frame.clone();
        alternate_host_frame.frame_id = host_frame.frame_id.wrapping_add(1);
        alternate_host_frame.delta_seconds = 1.0 / 30.0;
        alternate_host_frame.positions.insert(1, [99.0, -99.0]);
        let alternate_snapshot = FrameSnapshot::from_host_frame(&alternate_host_frame);
        let handoff_harness =
            ScriptHarness::new(initial_snapshot.clone(), entity_count, collision_count)?;
        let mut handoff_alternate = false;
        let handoff_metrics = measure(operation_iterations, warmups, samples, || {
            let snapshot = if handoff_alternate {
                &alternate_snapshot
            } else {
                &initial_snapshot
            };
            handoff_harness.publish_snapshot(snapshot);
            handoff_alternate = !handoff_alternate;
            Ok(black_box(snapshot.frame_id()))
        })?;
        records.push(Record {
            phase: "frame_snapshot_handoff".to_owned(),
            mode: "publish_snapshot_mutex_clone_from".to_owned(),
            entity_count,
            changed_entity_count: None,
            item_count: entity_count + collision_count,
            host_callbacks_per_update: 0,
            iterations_per_sample: operation_iterations,
            samples,
            metrics: handoff_metrics,
        });

        // Active-pose/deactivation cache is an explicitly synthetic host phase.
        for moving_count in [2_usize.min(entity_count), entity_count] {
            let mut cache = PhysicsPoseCache::new(entity_count);
            let mut active_poses = (1..=moving_count as u32)
                .map(|id| (id, [id as f32 * 0.5, id as f32 * -0.25]))
                .collect::<Vec<_>>();
            let deactivated_id = if moving_count < entity_count {
                entity_count as u32
            } else {
                1
            };
            let mut deactivated_poses = vec![(deactivated_id, [0.0, 0.0])];
            let mut frame = 0_u64;
            let metrics = measure(operation_iterations, warmups, samples, || {
                frame = frame.wrapping_add(1);
                let offset = frame as f32 * 0.0001;
                for (entity_id, position) in &mut active_poses {
                    position[0] = *entity_id as f32 * 0.5 + offset;
                    position[1] = *entity_id as f32 * -0.25 - offset;
                }
                deactivated_poses[0].1 = [offset * 2.0, -offset * 2.0];
                cache.apply_active_poses(&active_poses, &deactivated_poses);
                Ok(black_box(cache.changes().len() as u64))
            })?;
            let actual_change_count = if moving_count < entity_count {
                moving_count + 1
            } else {
                moving_count
            };
            records.push(Record {
                phase: "host_physics_active_pose_cache_model".to_owned(),
                mode: "active_poses_plus_final_deactivation".to_owned(),
                entity_count,
                changed_entity_count: Some(actual_change_count),
                item_count: actual_change_count,
                host_callbacks_per_update: 0,
                iterations_per_sample: operation_iterations,
                samples,
                metrics,
            });
        }

        // Sparse and dense host ECS workloads use the same full transform cache but
        // different changed-entity cardinalities; all paths remain outside JS dispatch.
        for changed_count in [2_usize.min(entity_count), entity_count] {
            let mut positions = host_frame.positions.clone();
            let changes = (1..=changed_count as u32)
                .map(|entity_id| PositionChange {
                    entity_id,
                    position: [entity_id as f32 + 10.0, entity_id as f32 - 10.0],
                })
                .collect::<Vec<_>>();
            for change in &changes {
                positions.insert(change.entity_id, change.position);
            }

            let mut sparse = HostEcsSync::new(entity_count);
            let sparse_metrics = measure(operation_iterations, warmups, samples, || {
                sparse.sync_sparse(&changes);
                Ok(black_box(sparse.transforms().len() as u64))
            })?;
            records.push(Record {
                phase: "host_ecs_sync_model".to_owned(),
                mode: "sparse_changed_entity_updates".to_owned(),
                entity_count,
                changed_entity_count: Some(changed_count),
                item_count: changes.len(),
                host_callbacks_per_update: 0,
                iterations_per_sample: operation_iterations,
                samples,
                metrics: sparse_metrics,
            });

            let mut dense = HostEcsSync::new(entity_count);
            let dense_metrics = measure(operation_iterations, warmups, samples, || {
                dense.sync_dense(&positions);
                Ok(black_box(dense.transforms().len() as u64))
            })?;
            records.push(Record {
                phase: "host_ecs_sync_model".to_owned(),
                mode: "dense_full_transform_query".to_owned(),
                entity_count,
                changed_entity_count: Some(changed_count),
                item_count: entity_count,
                host_callbacks_per_update: 0,
                iterations_per_sample: operation_iterations,
                samples,
                metrics: dense_metrics,
            });

            let mut adaptive = HostEcsSync::new(entity_count);
            let adaptive_metrics = measure(operation_iterations, warmups, samples, || {
                let mode = adaptive
                    .sync_adaptive(&changes, &positions)
                    .ok_or_else(|| "adaptive ECS sync unexpectedly skipped".to_owned())?;
                Ok(black_box(match mode {
                    ogma_frame_snapshot_proof::EcsSyncMode::Sparse => 1,
                    ogma_frame_snapshot_proof::EcsSyncMode::Dense => 2,
                }))
            })?;
            records.push(Record {
                phase: "host_ecs_sync_model".to_owned(),
                mode: "consumer_50_percent_adaptive_selector".to_owned(),
                entity_count,
                changed_entity_count: Some(changed_count),
                item_count: changes.len(),
                host_callbacks_per_update: 0,
                iterations_per_sample: operation_iterations,
                samples,
                metrics: adaptive_metrics,
            });
        }

        // Reference consumer script uses cached JS functions and a stable FrameSnapshot.
        let harness = ScriptHarness::new(initial_snapshot, entity_count, collision_count)?;
        let scalar_result = harness.run(UpdateMode::Scalar)?;
        let set_result = harness.run(UpdateMode::BatchSet)?;
        let bulk_result = harness.run(UpdateMode::BatchBulk)?;
        let pure_result = harness.run(UpdateMode::PureJavaScript)?;
        for (name, result) in [
            ("batch TypedArray.set", set_result),
            ("batch bulk write", bulk_result),
            ("pure JavaScript baseline", pure_result),
        ] {
            if (scalar_result - result).abs() > 0.000_001 {
                return Err(format!(
                    "consumer correctness mismatch for {name} at {entity_count} entities: scalar={scalar_result}, result={result}"
                ));
            }
        }

        let scalar_callbacks = 10 + 6 + 2 + (entity_count * 2) + 1 + (collision_count * 2) + 1 + 2;
        let batch_callbacks = 10 + 6 + 2 + 1 + 1 + 1 + 1 + 2;
        for (mode, update_mode, callback_count) in [
            (
                "string_ids_scalar_callbacks",
                UpdateMode::Scalar,
                scalar_callbacks,
            ),
            (
                "numeric_bitsets_batch_set",
                UpdateMode::BatchSet,
                batch_callbacks,
            ),
            (
                "numeric_bitsets_batch_bulk",
                UpdateMode::BatchBulk,
                batch_callbacks,
            ),
            (
                "javascript_only_preloaded_arrays",
                UpdateMode::PureJavaScript,
                0,
            ),
        ] {
            let metrics = measure(operation_iterations, warmups, samples, || {
                let result = harness.run(update_mode)?;
                Ok(black_box(result.to_bits()))
            })?;
            records.push(Record {
                phase: if callback_count == 0 {
                    "javascript_update_only".to_owned()
                } else {
                    "reference_consumer_js_update_and_dispatch".to_owned()
                },
                mode: mode.to_owned(),
                entity_count,
                changed_entity_count: None,
                item_count: entity_count + collision_count,
                host_callbacks_per_update: callback_count,
                iterations_per_sample: operation_iterations,
                samples,
                metrics,
            });
        }
    }

    // Reproduce the pinned CGE Criterion host-API update at its original sizes
    // (16/64/256). The additional modes isolate scalar transforms, string/bitset
    // input, per-element TypedArray.set, and validated contiguous bulk transfer.
    for entity_count in [16_usize, 64, 256] {
        let collision_count = 0;
        let operation_iterations = iterations_for(iterations, entity_count);
        let host_frame = HostFrame::fixture(1, entity_count, collision_count);
        let snapshot = FrameSnapshot::from_host_frame(&host_frame);
        let harness = ScriptHarness::new(snapshot, entity_count, collision_count)?;

        let scalar_result = harness.run(UpdateMode::CgeScalar)?;
        let array_set_result = harness.run(UpdateMode::CgeReadPositions)?;
        let numeric_set_result = harness.run(UpdateMode::CgeNumericReadPositions)?;
        let bulk_result = harness.run(UpdateMode::CgeBulk)?;
        let pure_result = harness.run(UpdateMode::CgePureJavaScript)?;
        for (name, result) in [
            ("CGE Array readPositions", array_set_result),
            ("CGE numeric-input Array readPositions", numeric_set_result),
            ("CGE typed-ID bulk read", bulk_result),
            ("CGE JavaScript-only update", pure_result),
        ] {
            if (scalar_result - result).abs() > 0.000_001 {
                return Err(format!(
                    "CGE reference workload correctness mismatch for {name} at {entity_count} entities: scalar={scalar_result}, result={result}"
                ));
            }
        }

        let scalar_callbacks = entity_count * 2 + 8 + 1;
        let array_set_callbacks = 1 + 8 + 1;
        for (mode, update_mode, callback_count) in [
            (
                "cge_scalar_callbacks",
                UpdateMode::CgeScalar,
                scalar_callbacks,
            ),
            (
                "cge_array_read_positions_set",
                UpdateMode::CgeReadPositions,
                array_set_callbacks,
            ),
            (
                "cge_array_read_positions_numeric_set",
                UpdateMode::CgeNumericReadPositions,
                array_set_callbacks,
            ),
            (
                "cge_typed_id_bulk",
                UpdateMode::CgeBulk,
                array_set_callbacks,
            ),
            (
                "cge_javascript_only_preloaded_output",
                UpdateMode::CgePureJavaScript,
                0,
            ),
        ] {
            let metrics = measure(operation_iterations, warmups, samples, || {
                Ok(black_box(harness.run(update_mode)?.to_bits()))
            })?;
            records.push(Record {
                phase: if callback_count == 0 {
                    "cge_javascript_update_only".to_owned()
                } else {
                    "cge_reference_consumer_js_update_and_dispatch".to_owned()
                },
                mode: mode.to_owned(),
                entity_count,
                changed_entity_count: None,
                item_count: entity_count,
                host_callbacks_per_update: callback_count,
                iterations_per_sample: operation_iterations,
                samples,
                metrics,
            });
        }
    }

    command_drain_records(&mut records, &[0, 8, 64, 256], 32, warmups, samples)?;

    let cases = records
        .iter()
        .map(Record::to_json)
        .collect::<Vec<_>>()
        .join(",");
    let report = format!(
        "{{\"task\":\"OGM-23\",\"revision\":\"{}\",\"tested_revision\":\"{}\",\"rustc_version\":\"{}\",\"os\":\"{}\",\"arch\":\"{}\",\"cpu\":\"{}\",\"profile\":\"release\",\"warmups\":{},\"measurement_samples\":{},\"base_iterations\":{},\"cge_reference_revision\":\"{}\",\"reference_runtime\":\"rquickjs 0.14.0 (benchmark-only; not an Ogma Runtime)\",\"cge_reference_workload\":\"quickjs_host_api/frame_submit_and_update from pinned CGE benches/runtime.rs; entity counts 16,64,256\",\"method\":\"nearest-rank p50/p95/p99 of per-sample mean latency; each mode validates an equivalent checksum before timing\",\"allocation_counter\":\"Rust global allocator alloc/alloc_zeroed/realloc calls only; QuickJS C allocator allocations are not included\",\"limitations\":{{\"ogma_runtime_dispatch\":\"not_measured: this repository revision has no executable Ogma Runtime or production JS host adapter\",\"host_physics\":\"synthetic active-pose/deactivation cache model; Jolt step and Jolt active_body_poses_into are excluded\",\"host_ecs\":\"synthetic sparse/dense transform synchronization model; Flecs query execution is excluded\",\"reference_dispatch\":\"measures the pinned CGE-style JavaScript workload through rquickjs 0.14.0; do not interpret as Ogma Runtime performance\"}},\"cases\":[{}]}}",
        escape_json(&revision),
        escape_json(&tested_revision),
        escape_json(&std::env::var("RUSTC_VERSION").unwrap_or_else(|_| "unknown".to_owned())),
        escape_json(std::env::consts::OS),
        escape_json(std::env::consts::ARCH),
        escape_json(&cpu),
        warmups,
        samples,
        iterations,
        CGE_REFERENCE_REVISION,
        cases,
    );

    std::fs::write(&output, report).map_err(|error| error.to_string())?;
    println!("Wrote {output} with {} benchmark cases", records.len());
    Ok(())
}

fn parse_argument(arguments: &[String], name: &str, default: usize) -> Result<usize, String> {
    let Some(value) = arguments
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].as_str())
    else {
        return Ok(default);
    };
    value
        .parse::<usize>()
        .map_err(|error| format!("invalid value for {name}: {error}"))
}

fn string_argument(arguments: &[String], name: &str, default: &str) -> String {
    arguments
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
        .unwrap_or_else(|| default.to_owned())
}

fn escape_json(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\u{0008}' => escaped.push_str("\\b"),
            '\u{000c}' => escaped.push_str("\\f"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            control if control <= '\u{001f}' => {
                use std::fmt::Write as _;
                let _ = write!(escaped, "\\u{:04x}", control as u32);
            }
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::escape_json;

    #[test]
    fn json_string_escapes_quotes_backslashes_and_control_characters() {
        assert_eq!(
            escape_json("\"\\\u{0000}\u{0008}\u{0009}\n\r\u{001f}"),
            "\\\"\\\\\\u0000\\b\\t\\n\\r\\u001f"
        );
    }

    #[test]
    fn iteration_budget_scales_with_entity_count_and_never_becomes_zero() {
        assert_eq!(super::iterations_for(100, 40), 100);
        assert_eq!(super::iterations_for(100, 256), 15);
        assert_eq!(super::iterations_for(100, 1024), 3);
        assert_eq!(super::iterations_for(1, usize::MAX), 1);
    }

    #[test]
    fn cli_argument_parsing_preserves_defaults_and_rejects_invalid_values() {
        let explicit = vec![String::from("--samples"), String::from("5")];
        assert_eq!(
            super::parse_argument(&explicit, "--samples", 101).unwrap(),
            5
        );
        assert_eq!(
            super::parse_argument(&explicit, "--iterations", 1000).unwrap(),
            1000
        );

        let invalid = vec![String::from("--samples"), String::from("many")];
        assert!(super::parse_argument(&invalid, "--samples", 101).is_err());
    }
}
