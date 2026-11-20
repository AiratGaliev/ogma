//! OGM-24 allocator measurements for the OGM-38 candidate heap workload.
//!
//! The OGM-26 collector proof owns graph topology and reachability. This module
//! supplies real per-isolate backing allocations with the same candidate Value
//! storage size and per-object payload sizes, so allocator quota/reuse/fragmentation
//! are measured rather than inferred from Vec capacities or a synthetic model.
//! The backing blocks are deliberately a sidecar to the OGM-26 proof heap; this
//! does not claim that the production Runtime's object cells use this allocator.

use anyhow::{Context, Result, bail};
use ogma_isolate_allocator_proof::{AllocatorStrategy, IsolateAllocator};
use serde::Serialize;
use std::alloc::Layout;
use std::hint::black_box;
use std::time::Instant;

#[derive(Clone, Debug, Serialize)]
pub struct Distribution {
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub samples: Vec<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AllocatorSample {
    pub allocation_ns: u128,
    pub deallocation_ns: u128,
    pub peak_reserved_bytes: usize,
    pub peak_live_requested_bytes: usize,
    pub peak_internal_fragmentation_bytes: usize,
    pub reused_allocations: u64,
    pub cache_evictions: u64,
    pub allocation_failures: u64,
    pub live_requested_bytes_after_release: usize,
    pub reserved_bytes_after_release: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct AllocatorStrategyEvidence {
    pub strategy: &'static str,
    pub object_count: usize,
    pub payload_bytes_per_object: usize,
    pub value_storage_bytes: usize,
    pub allocation_rounds_per_sample: usize,
    pub allocation_ns_per_sample: Distribution,
    pub deallocation_ns_per_sample: Distribution,
    pub peak_reserved_bytes_max: usize,
    pub peak_live_requested_bytes_max: usize,
    pub peak_internal_fragmentation_bytes_max: usize,
    pub reused_allocations_total: u64,
    pub cache_evictions_total: u64,
    pub allocation_failures_total: u64,
    pub raw_samples: Vec<AllocatorSample>,
}

fn distribution(mut samples: Vec<f64>) -> Distribution {
    samples.sort_by(f64::total_cmp);
    let percentile = |p: f64| {
        if samples.is_empty() {
            return 0.0;
        }
        let index = ((samples.len() - 1) as f64 * p).round() as usize;
        samples[index]
    };
    Distribution {
        p50: percentile(0.50),
        p95: percentile(0.95),
        p99: percentile(0.99),
        samples,
    }
}

fn layout(size: usize) -> Result<Layout> {
    Layout::from_size_align(size.max(1), 8).context("invalid OGM-38 allocator workload layout")
}

fn allocate_round(
    allocator: &mut IsolateAllocator,
    payload_layout: Layout,
    value_layout: Layout,
    object_count: usize,
) -> Result<Vec<ogma_isolate_allocator_proof::Allocation>> {
    let mut allocations = Vec::with_capacity(object_count + 1);
    let value_storage = allocator
        .allocate(value_layout)
        .map_err(|error| anyhow::anyhow!("OGM-24 Value backing allocation failed: {error:?}"))?;
    // Touch the full buffer: reserved-memory measurements must not count only
    // untouched virtual allocations.
    unsafe {
        value_storage
            .as_non_null()
            .as_ptr()
            .write_bytes(0xA5, value_layout.size());
    }
    allocations.push(value_storage);

    for index in 0..object_count {
        let allocation = allocator.allocate(payload_layout).map_err(|error| {
            anyhow::anyhow!("OGM-24 object payload allocation {index} failed: {error:?}")
        })?;
        unsafe {
            allocation
                .as_non_null()
                .as_ptr()
                .write_bytes((index as u8).wrapping_add(1), payload_layout.size());
        }
        allocations.push(allocation);
    }
    black_box(&allocations);
    Ok(allocations)
}

fn deallocate_round(
    allocator: &mut IsolateAllocator,
    allocations: Vec<ogma_isolate_allocator_proof::Allocation>,
) -> Result<()> {
    for allocation in allocations.into_iter().rev() {
        allocator.deallocate(allocation).map_err(|error| {
            anyhow::anyhow!("OGM-24 backing deallocation failed: {:?}", error.kind())
        })?;
    }
    Ok(())
}

fn sample(
    strategy: AllocatorStrategy,
    object_count: usize,
    payload_bytes_per_object: usize,
    value_storage_bytes: usize,
    quota_bytes: usize,
) -> Result<AllocatorSample> {
    let payload_layout = layout(payload_bytes_per_object)?;
    let value_layout = layout(value_storage_bytes)?;
    let mut allocator = IsolateAllocator::new(strategy, quota_bytes);

    let allocation_start = Instant::now();
    let first = allocate_round(&mut allocator, payload_layout, value_layout, object_count)?;
    let first_allocation_ns = allocation_start.elapsed().as_nanos();
    let first_stats = allocator.stats();

    let deallocation_start = Instant::now();
    deallocate_round(&mut allocator, first)?;
    let first_deallocation_ns = deallocation_start.elapsed().as_nanos();

    // The second identical allocation round makes reuse observable for
    // PerIsolateReuse and SizeClassPool, while System remains the baseline.
    let allocation_start = Instant::now();
    let second = allocate_round(&mut allocator, payload_layout, value_layout, object_count)?;
    let second_allocation_ns = allocation_start.elapsed().as_nanos();
    let second_stats = allocator.stats();

    let deallocation_start = Instant::now();
    deallocate_round(&mut allocator, second)?;
    let second_deallocation_ns = deallocation_start.elapsed().as_nanos();
    let allocation_ns = first_allocation_ns + second_allocation_ns;
    let deallocation_ns = first_deallocation_ns + second_deallocation_ns;
    let final_stats = allocator.stats();

    if final_stats.live_requested_bytes != 0 || final_stats.live_backing_bytes != 0 {
        bail!(
            "OGM-24 {} left live backing allocations after release: requested={} backing={}",
            strategy.name(),
            final_stats.live_requested_bytes,
            final_stats.live_backing_bytes
        );
    }
    if final_stats.allocation_failures != 0 {
        bail!(
            "OGM-24 {} unexpectedly failed {} allocations",
            strategy.name(),
            final_stats.allocation_failures
        );
    }

    Ok(AllocatorSample {
        allocation_ns,
        deallocation_ns,
        peak_reserved_bytes: first_stats
            .peak_reserved_bytes
            .max(second_stats.peak_reserved_bytes)
            .max(final_stats.peak_reserved_bytes),
        peak_live_requested_bytes: first_stats
            .peak_live_requested_bytes
            .max(second_stats.peak_live_requested_bytes)
            .max(final_stats.peak_live_requested_bytes),
        peak_internal_fragmentation_bytes: first_stats
            .peak_internal_fragmentation_bytes
            .max(second_stats.peak_internal_fragmentation_bytes)
            .max(final_stats.peak_internal_fragmentation_bytes),
        reused_allocations: final_stats.reused_allocations,
        cache_evictions: final_stats.cache_evictions,
        allocation_failures: final_stats.allocation_failures,
        live_requested_bytes_after_release: final_stats.live_requested_bytes,
        reserved_bytes_after_release: final_stats.reserved_bytes,
    })
}

pub fn run_allocator_evidence(
    object_count: usize,
    value_bytes_per_slot: usize,
    value_slots_per_object: usize,
    warmups: usize,
    measurements: usize,
) -> Result<Vec<AllocatorStrategyEvidence>> {
    if object_count == 0 || value_bytes_per_slot == 0 || value_slots_per_object == 0 {
        bail!("OGM-24 allocator workload dimensions must be non-zero");
    }
    if measurements < 1 {
        bail!("OGM-24 allocator workload requires at least one measurement");
    }

    let value_storage_bytes = object_count
        .checked_mul(value_slots_per_object)
        .and_then(|slots| slots.checked_mul(value_bytes_per_slot))
        .context("OGM-24 candidate Value backing size overflow")?;
    let payload_bytes_per_object = value_slots_per_object
        .checked_mul(value_bytes_per_slot)
        .context("OGM-24 per-object payload size overflow")?;
    let one_round_bytes = value_storage_bytes
        .checked_add(
            object_count
                .checked_mul(payload_bytes_per_object)
                .context("OGM-24 object payload total overflow")?,
        )
        .context("OGM-24 total allocation size overflow")?;
    // Allow two simultaneously live rounds only transiently; quota tracks the
    // first round after it has been returned to the cache.
    let quota_bytes = one_round_bytes
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(1024 * 1024))
        .context("OGM-24 allocator quota overflow")?;

    let mut evidence = Vec::with_capacity(AllocatorStrategy::ALL.len());
    for strategy in AllocatorStrategy::ALL {
        for _ in 0..warmups {
            black_box(sample(
                strategy,
                object_count,
                payload_bytes_per_object,
                value_storage_bytes,
                quota_bytes,
            )?);
        }
        let mut raw_samples = Vec::with_capacity(measurements);
        for _ in 0..measurements {
            raw_samples.push(sample(
                strategy,
                object_count,
                payload_bytes_per_object,
                value_storage_bytes,
                quota_bytes,
            )?);
        }
        let peak_reserved_bytes_max = raw_samples
            .iter()
            .map(|item| item.peak_reserved_bytes)
            .max()
            .unwrap_or_default();
        let peak_live_requested_bytes_max = raw_samples
            .iter()
            .map(|item| item.peak_live_requested_bytes)
            .max()
            .unwrap_or_default();
        let peak_internal_fragmentation_bytes_max = raw_samples
            .iter()
            .map(|item| item.peak_internal_fragmentation_bytes)
            .max()
            .unwrap_or_default();
        let reused_allocations_total = raw_samples.iter().map(|item| item.reused_allocations).sum();
        let cache_evictions_total = raw_samples.iter().map(|item| item.cache_evictions).sum();
        let allocation_failures_total = raw_samples
            .iter()
            .map(|item| item.allocation_failures)
            .sum();

        evidence.push(AllocatorStrategyEvidence {
            strategy: strategy.name(),
            object_count,
            payload_bytes_per_object,
            value_storage_bytes,
            allocation_rounds_per_sample: 2,
            allocation_ns_per_sample: distribution(
                raw_samples
                    .iter()
                    .map(|item| item.allocation_ns as f64)
                    .collect(),
            ),
            deallocation_ns_per_sample: distribution(
                raw_samples
                    .iter()
                    .map(|item| item.deallocation_ns as f64)
                    .collect(),
            ),
            peak_reserved_bytes_max,
            peak_live_requested_bytes_max,
            peak_internal_fragmentation_bytes_max,
            reused_allocations_total,
            cache_evictions_total,
            allocation_failures_total,
            raw_samples,
        });
    }
    Ok(evidence)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_candidate_heap_shape_is_measured_with_all_ogm29_strategies() {
        let evidence = run_allocator_evidence(64, 16, 16, 1, 3).expect("allocator evidence");
        assert_eq!(evidence.len(), AllocatorStrategy::ALL.len());
        for item in &evidence {
            assert_eq!(item.object_count, 64);
            assert_eq!(item.payload_bytes_per_object, 256);
            assert_eq!(item.value_storage_bytes, 16 * 64 * 16);
            assert_eq!(item.allocation_rounds_per_sample, 2);
            assert_eq!(item.allocation_failures_total, 0);
            assert!(item.peak_reserved_bytes_max >= item.peak_live_requested_bytes_max);
            assert!(item.raw_samples.iter().all(|sample| {
                sample.live_requested_bytes_after_release == 0 && sample.allocation_failures == 0
            }));
        }
        let system = evidence
            .iter()
            .find(|item| item.strategy == "system-allocator")
            .unwrap();
        let reuse = evidence
            .iter()
            .find(|item| item.strategy == "per-isolate-exact-reuse")
            .unwrap();
        let size_class = evidence
            .iter()
            .find(|item| item.strategy == "per-isolate-size-class-pool")
            .unwrap();
        assert_eq!(system.reused_allocations_total, 0);
        assert!(reuse.reused_allocations_total > 0);
        assert!(size_class.reused_allocations_total > 0);
    }
}
