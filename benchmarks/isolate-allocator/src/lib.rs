//! OGM-24 M0 allocator and compile-unit arena proof.
//!
//! This is a standalone allocation harness, not an Ogma JavaScript heap or
//! production Runtime allocator. Allocations are raw byte blocks with explicit
//! ownership and quota accounting; no JS object/GC semantics are modeled.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, ThreadId};

const MAX_CACHED_BLOCKS_PER_BUCKET: usize = 128;
const MIN_SIZE_CLASS: usize = 16;
const MAX_SIZE_CLASS: usize = 4096;
pub const ARENA_CHUNK_BYTES: usize = 16 * 1024;
static NEXT_ALLOCATOR_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllocatorStrategy {
    System,
    PerIsolateReuse,
    SizeClassPool,
}

impl AllocatorStrategy {
    pub const ALL: [Self; 3] = [Self::System, Self::PerIsolateReuse, Self::SizeClassPool];

    pub fn name(self) -> &'static str {
        match self {
            Self::System => "system-allocator",
            Self::PerIsolateReuse => "per-isolate-exact-reuse",
            Self::SizeClassPool => "per-isolate-size-class-pool",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|strategy| strategy.name() == value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllocationError {
    InvalidLayout,
    QuotaExceeded,
    OutOfMemory,
    WrongThread,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AllocatorStats {
    pub successful_allocations: u64,
    pub deallocations: u64,
    pub allocation_failures: u64,
    pub reused_allocations: u64,
    pub cache_evictions: u64,
    pub live_requested_bytes: usize,
    pub live_backing_bytes: usize,
    pub reserved_bytes: usize,
    pub free_cached_bytes: usize,
    pub peak_live_requested_bytes: usize,
    pub peak_reserved_bytes: usize,
    pub peak_free_cached_bytes: usize,
    /// Maximum reserved minus live requested bytes observed at one instant.
    /// This includes cached blocks and size-class slack; it is not OS-level
    /// external fragmentation.
    pub peak_internal_fragmentation_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct BucketKey {
    size: usize,
    align: usize,
}

struct FreeBlock {
    pointer: NonNull<u8>,
    layout: Layout,
}

/// Unique allocation token. It is intentionally !Send/!Sync and can only be
/// created by an IsolateAllocator.
#[derive(Debug)]
pub struct Allocation {
    pointer: NonNull<u8>,
    requested_size: usize,
    backing_layout: Layout,
    bucket: Option<BucketKey>,
    allocator_id: u64,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl Allocation {
    pub fn as_non_null(&self) -> NonNull<u8> {
        self.pointer
    }

    pub fn requested_size(&self) -> usize {
        self.requested_size
    }

    pub fn backing_size(&self) -> usize {
        self.backing_layout.size()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeallocationErrorKind {
    WrongThread,
    WrongAllocator,
}

/// A failed deallocation returns ownership of the allocation to the caller.
pub struct DeallocationError {
    kind: DeallocationErrorKind,
    allocation: Allocation,
}

impl DeallocationError {
    pub fn kind(&self) -> DeallocationErrorKind {
        self.kind
    }

    pub fn into_allocation(self) -> Allocation {
        self.allocation
    }
}

/// Isolate-local allocation hooks with a strict committed-byte quota.
///
/// System is the reference allocator. The two custom strategies retain
/// deallocated blocks inside this allocator instance only. Cached capacity is
/// counted against quota; a pool can evict cached blocks to satisfy a later
/// larger request. The type is owner-thread-only.
pub struct IsolateAllocator {
    id: u64,
    owner_thread: ThreadId,
    strategy: AllocatorStrategy,
    quota_bytes: usize,
    free: BTreeMap<BucketKey, Vec<FreeBlock>>,
    stats: AllocatorStats,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl IsolateAllocator {
    pub fn new(strategy: AllocatorStrategy, quota_bytes: usize) -> Self {
        Self {
            id: NEXT_ALLOCATOR_ID.fetch_add(1, Ordering::Relaxed),
            owner_thread: thread::current().id(),
            strategy,
            quota_bytes,
            free: BTreeMap::new(),
            stats: AllocatorStats::default(),
            _not_send_or_sync: PhantomData,
        }
    }

    pub fn strategy(&self) -> AllocatorStrategy {
        self.strategy
    }

    pub fn quota_bytes(&self) -> usize {
        self.quota_bytes
    }

    pub fn stats(&self) -> AllocatorStats {
        self.stats
    }

    pub fn allocate(&mut self, requested: Layout) -> Result<Allocation, AllocationError> {
        if thread::current().id() != self.owner_thread {
            self.stats.allocation_failures += 1;
            return Err(AllocationError::WrongThread);
        }

        let bucket = self.bucket_for(requested);
        let reused = bucket.and_then(|key| self.take_cached(key));
        let (pointer, backing_layout, was_reused) = match reused {
            Some(block) => (block.pointer, block.layout, true),
            None => {
                let backing_layout = self.backing_layout(requested, bucket)?;
                if let Err(error) = self.ensure_capacity(backing_layout.size()) {
                    self.stats.allocation_failures += 1;
                    return Err(error);
                }
                // SAFETY: backing_layout is valid and non-zero; ownership is
                // transferred into the returned Allocation on success.
                let raw = unsafe { System.alloc(backing_layout) };
                let Some(pointer) = NonNull::new(raw) else {
                    self.stats.allocation_failures += 1;
                    return Err(AllocationError::OutOfMemory);
                };
                self.stats.reserved_bytes += backing_layout.size();
                (pointer, backing_layout, false)
            }
        };

        if was_reused {
            self.stats.free_cached_bytes = self
                .stats
                .free_cached_bytes
                .saturating_sub(backing_layout.size());
            self.stats.reused_allocations += 1;
        }
        self.stats.successful_allocations += 1;
        self.stats.live_requested_bytes = self
            .stats
            .live_requested_bytes
            .checked_add(requested.size())
            .ok_or(AllocationError::QuotaExceeded)?;
        self.stats.live_backing_bytes = self
            .stats
            .live_backing_bytes
            .checked_add(backing_layout.size())
            .ok_or(AllocationError::QuotaExceeded)?;
        self.update_peaks();

        Ok(Allocation {
            pointer,
            requested_size: requested.size(),
            backing_layout,
            bucket,
            allocator_id: self.id,
            _not_send_or_sync: PhantomData,
        })
    }

    pub fn deallocate(&mut self, allocation: Allocation) -> Result<(), DeallocationError> {
        let error_kind = if thread::current().id() != self.owner_thread {
            Some(DeallocationErrorKind::WrongThread)
        } else if allocation.allocator_id != self.id {
            Some(DeallocationErrorKind::WrongAllocator)
        } else {
            None
        };
        if let Some(kind) = error_kind {
            return Err(DeallocationError { kind, allocation });
        }

        self.stats.live_requested_bytes = self
            .stats
            .live_requested_bytes
            .saturating_sub(allocation.requested_size);
        self.stats.live_backing_bytes = self
            .stats
            .live_backing_bytes
            .saturating_sub(allocation.backing_layout.size());
        self.stats.deallocations += 1;

        let cache_key = allocation.bucket.filter(|key| {
            self.free.get(key).map_or(0, Vec::len) < MAX_CACHED_BLOCKS_PER_BUCKET
        });

        if let Some(key) = cache_key {
            self.stats.free_cached_bytes += allocation.backing_layout.size();
            self.free.entry(key).or_default().push(FreeBlock {
                pointer: allocation.pointer,
                layout: allocation.backing_layout,
            });
        } else {
            // SAFETY: this token owns this exact pointer/Layout pair, and the
            // allocator identity was checked above.
            unsafe { System.dealloc(allocation.pointer.as_ptr(), allocation.backing_layout) };
            self.stats.reserved_bytes = self
                .stats
                .reserved_bytes
                .saturating_sub(allocation.backing_layout.size());
        }
        self.update_peaks();
        Ok(())
    }

    fn bucket_for(&self, requested: Layout) -> Option<BucketKey> {
        let size = requested.size().max(1);
        match self.strategy {
            AllocatorStrategy::System => None,
            AllocatorStrategy::PerIsolateReuse => Some(BucketKey {
                size,
                align: requested.align(),
            }),
            AllocatorStrategy::SizeClassPool => {
                if size > MAX_SIZE_CLASS || requested.align() > MIN_SIZE_CLASS {
                    return None;
                }
                let class_size = size
                    .max(MIN_SIZE_CLASS)
                    .checked_next_power_of_two()?;
                Some(BucketKey {
                    size: class_size,
                    align: MIN_SIZE_CLASS,
                })
            }
        }
    }

    fn backing_layout(
        &self,
        requested: Layout,
        bucket: Option<BucketKey>,
    ) -> Result<Layout, AllocationError> {
        let (size, align) = bucket
            .map(|key| (key.size, key.align))
            .unwrap_or((requested.size().max(1), requested.align()));
        Layout::from_size_align(size, align).map_err(|_| AllocationError::InvalidLayout)
    }

    fn take_cached(&mut self, key: BucketKey) -> Option<FreeBlock> {
        let mut block = None;
        let mut remove_key = false;
        if let Some(blocks) = self.free.get_mut(&key) {
            block = blocks.pop();
            remove_key = blocks.is_empty();
        }
        if remove_key {
            self.free.remove(&key);
        }
        block
    }

    fn ensure_capacity(&mut self, additional: usize) -> Result<(), AllocationError> {
        let fits = |reserved: usize| {
            reserved
                .checked_add(additional)
                .is_some_and(|total| total <= self.quota_bytes)
        };
        while !fits(self.stats.reserved_bytes) {
            let Some(key) = self.free.keys().next().copied() else {
                return Err(AllocationError::QuotaExceeded);
            };
            let block = self
                .take_cached(key)
                .expect("a selected cache bucket contains a block");
            // SAFETY: the block is owned by this allocator's cache.
            unsafe { System.dealloc(block.pointer.as_ptr(), block.layout) };
            self.stats.reserved_bytes = self.stats.reserved_bytes.saturating_sub(block.layout.size());
            self.stats.free_cached_bytes = self
                .stats
                .free_cached_bytes
                .saturating_sub(block.layout.size());
            self.stats.cache_evictions += 1;
        }
        Ok(())
    }

    fn update_peaks(&mut self) {
        self.stats.peak_live_requested_bytes = self
            .stats
            .peak_live_requested_bytes
            .max(self.stats.live_requested_bytes);
        self.stats.peak_reserved_bytes = self
            .stats
            .peak_reserved_bytes
            .max(self.stats.reserved_bytes);
        self.stats.peak_free_cached_bytes = self
            .stats
            .peak_free_cached_bytes
            .max(self.stats.free_cached_bytes);
        let fragmentation = self
            .stats
            .reserved_bytes
            .saturating_sub(self.stats.live_requested_bytes);
        self.stats.peak_internal_fragmentation_bytes = self
            .stats
            .peak_internal_fragmentation_bytes
            .max(fragmentation);
    }
}

impl Drop for IsolateAllocator {
    fn drop(&mut self) {
        for blocks in self.free.values_mut() {
            for block in blocks.drain(..) {
                // SAFETY: cached blocks remain uniquely owned by this allocator.
                unsafe { System.dealloc(block.pointer.as_ptr(), block.layout) };
            }
        }
        self.free.clear();
        self.stats.free_cached_bytes = 0;
        self.stats.reserved_bytes = self.stats.live_backing_bytes;
    }
}

struct ArenaChunk {
    pointer: NonNull<u8>,
    layout: Layout,
    used: usize,
}

/// Statistics for compile-unit storage. Peak-used bytes are payload bytes;
/// peak-padding bytes are alignment padding within the bump chunks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ArenaStats {
    pub allocations: u64,
    pub resets: u64,
    pub allocated_payload_bytes: usize,
    pub reserved_bytes: usize,
    pub peak_used_bytes: usize,
    pub peak_padding_bytes: usize,
    pub chunks: usize,
}

/// Resettable bump arena for AST/semantic/Core IR data whose lifetime is one
/// compile unit. Types with destructors and JS heap objects do not belong here.
pub struct CompileUnitArena {
    owner_thread: ThreadId,
    quota_bytes: usize,
    chunks: Vec<ArenaChunk>,
    current_chunk: usize,
    unit_payload_bytes: usize,
    unit_padding_bytes: usize,
    stats: ArenaStats,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl CompileUnitArena {
    pub fn new(quota_bytes: usize) -> Self {
        Self {
            owner_thread: thread::current().id(),
            quota_bytes,
            chunks: Vec::new(),
            current_chunk: 0,
            unit_payload_bytes: 0,
            unit_padding_bytes: 0,
            stats: ArenaStats::default(),
            _not_send_or_sync: PhantomData,
        }
    }

    pub fn allocate(&mut self, requested: Layout) -> Result<NonNull<u8>, AllocationError> {
        if thread::current().id() != self.owner_thread {
            return Err(AllocationError::WrongThread);
        }
        let size = requested.size().max(1);
        let align = requested.align();

        for index in self.current_chunk..self.chunks.len() {
            let chunk = &mut self.chunks[index];
            let offset = align_up(chunk.used, align).ok_or(AllocationError::InvalidLayout)?;
            let end = offset
                .checked_add(size)
                .ok_or(AllocationError::InvalidLayout)?;
            if end <= chunk.layout.size() {
                let padding = offset - chunk.used;
                let pointer = unsafe { NonNull::new_unchecked(chunk.pointer.as_ptr().add(offset)) };
                chunk.used = end;
                self.current_chunk = index;
                self.record_allocation(size, padding);
                // SAFETY: offset/end are in bounds and the chunk base alignment
                // is at least the requested alignment.
                return Ok(pointer);
            }
        }

        let required = size
            .checked_add(align.saturating_sub(1))
            .ok_or(AllocationError::InvalidLayout)?;
        let capacity = required.max(ARENA_CHUNK_BYTES);
        let chunk_align = align.max(64);
        let chunk_layout =
            Layout::from_size_align(capacity, chunk_align).map_err(|_| AllocationError::InvalidLayout)?;
        if self
            .stats
            .reserved_bytes
            .checked_add(chunk_layout.size())
            .is_none_or(|total| total > self.quota_bytes)
        {
            return Err(AllocationError::QuotaExceeded);
        }
        // SAFETY: chunk_layout is valid and non-zero.
        let raw = unsafe { System.alloc(chunk_layout) };
        let Some(pointer) = NonNull::new(raw) else {
            return Err(AllocationError::OutOfMemory);
        };
        self.stats.reserved_bytes += chunk_layout.size();
        self.chunks.push(ArenaChunk {
            pointer,
            layout: chunk_layout,
            used: 0,
        });
        self.stats.chunks = self.chunks.len();
        self.current_chunk = self.chunks.len() - 1;

        let chunk = &mut self.chunks[self.current_chunk];
        let offset = align_up(chunk.used, align).ok_or(AllocationError::InvalidLayout)?;
        let end = offset
            .checked_add(size)
            .ok_or(AllocationError::InvalidLayout)?;
        if end > chunk.layout.size() {
            return Err(AllocationError::InvalidLayout);
        }
        let padding = offset - chunk.used;
        let result = unsafe { NonNull::new_unchecked(chunk.pointer.as_ptr().add(offset)) };
        chunk.used = end;
        self.record_allocation(size, padding);
        // SAFETY: offset/end are in bounds and the chunk base alignment is
        // at least the requested alignment.
        Ok(result)
    }

    pub fn reset(&mut self) -> Result<(), AllocationError> {
        if thread::current().id() != self.owner_thread {
            return Err(AllocationError::WrongThread);
        }
        for chunk in &mut self.chunks {
            chunk.used = 0;
        }
        self.current_chunk = 0;
        self.unit_payload_bytes = 0;
        self.unit_padding_bytes = 0;
        self.stats.allocated_payload_bytes = 0;
        self.stats.resets += 1;
        Ok(())
    }

    pub fn stats(&self) -> ArenaStats {
        self.stats
    }

    fn record_allocation(&mut self, size: usize, padding: usize) {
        self.unit_payload_bytes = self.unit_payload_bytes.saturating_add(size);
        self.unit_padding_bytes = self.unit_padding_bytes.saturating_add(padding);
        self.stats.allocations += 1;
        self.stats.allocated_payload_bytes = self.unit_payload_bytes;
        self.stats.peak_used_bytes = self
            .stats
            .peak_used_bytes
            .max(self.unit_payload_bytes);
        self.stats.peak_padding_bytes = self
            .stats
            .peak_padding_bytes
            .max(self.unit_padding_bytes);
    }
}

impl Drop for CompileUnitArena {
    fn drop(&mut self) {
        for chunk in self.chunks.drain(..) {
            // SAFETY: every chunk is uniquely owned and retains its original layout.
            unsafe { System.dealloc(chunk.pointer.as_ptr(), chunk.layout) };
        }
    }
}

fn align_up(value: usize, align: usize) -> Option<usize> {
    value
        .checked_add(align.checked_sub(1)?)
        .map(|sum| sum & !(align - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).expect("valid test layout")
    }

    #[test]
    fn system_allocator_enforces_quota_and_releases_on_deallocation() {
        let mut allocator = IsolateAllocator::new(AllocatorStrategy::System, 128);
        let first = allocator.allocate(layout(64, 8)).expect("first allocation");
        let second = allocator.allocate(layout(64, 8)).expect("second allocation");
        assert_eq!(
            allocator.allocate(layout(1, 1)).unwrap_err(),
            AllocationError::QuotaExceeded
        );
        assert_eq!(allocator.stats().reserved_bytes, 128);

        allocator.deallocate(first).expect("release first allocation");
        assert_eq!(allocator.stats().reserved_bytes, 64);
        allocator.deallocate(second).expect("release second allocation");
        assert_eq!(allocator.stats().reserved_bytes, 0);
        assert_eq!(allocator.stats().live_requested_bytes, 0);
    }

    #[test]
    fn per_isolate_allocator_reuses_exact_blocks_and_counts_cache_against_quota() {
        let mut allocator = IsolateAllocator::new(AllocatorStrategy::PerIsolateReuse, 128);
        let first = allocator.allocate(layout(64, 8)).expect("first allocation");
        let second = allocator.allocate(layout(64, 8)).expect("second allocation");
        assert_eq!(
            allocator.allocate(layout(1, 1)).unwrap_err(),
            AllocationError::QuotaExceeded
        );

        allocator.deallocate(first).expect("cache first allocation");
        let reused = allocator.allocate(layout(64, 8)).expect("reuse cached block");
        assert_eq!(allocator.stats().reserved_bytes, 128);
        assert_eq!(allocator.stats().reused_allocations, 1);

        allocator.deallocate(second).expect("cache second allocation");
        allocator.deallocate(reused).expect("cache reused allocation");
        assert_eq!(allocator.stats().live_requested_bytes, 0);
        assert_eq!(allocator.stats().free_cached_bytes, 128);
        assert!(allocator.stats().peak_reserved_bytes <= allocator.quota_bytes());
    }

    #[test]
    fn size_classes_round_up_and_preserve_alignment() {
        let mut allocator = IsolateAllocator::new(AllocatorStrategy::SizeClassPool, 256);
        let allocation = allocator.allocate(layout(17, 8)).expect("pooled allocation");
        assert_eq!(allocation.requested_size(), 17);
        assert_eq!(allocation.backing_size(), 32);
        assert_eq!(allocation.as_non_null().as_ptr() as usize % 16, 0);
        allocator.deallocate(allocation).expect("cache pooled allocation");

        let same_class = allocator.allocate(layout(31, 16)).expect("reuse same class");
        assert_eq!(same_class.backing_size(), 32);
        assert_eq!(allocator.stats().reused_allocations, 1);
        allocator
            .deallocate(same_class)
            .expect("cache reused allocation");
    }

    #[test]
    fn allocator_instances_have_independent_quotas_and_ownership() {
        let mut first = IsolateAllocator::new(AllocatorStrategy::PerIsolateReuse, 64);
        let mut second = IsolateAllocator::new(AllocatorStrategy::PerIsolateReuse, 64);
        let first_block = first.allocate(layout(64, 8)).expect("first isolate");
        let second_block = second.allocate(layout(64, 8)).expect("second isolate");
        let mismatch = second.deallocate(first_block).unwrap_err();
        assert_eq!(mismatch.kind(), DeallocationErrorKind::WrongAllocator);
        first
            .deallocate(mismatch.into_allocation())
            .expect("return block to owning isolate");
        second.deallocate(second_block).expect("release second isolate");
    }

    #[test]
    fn size_class_pool_evicts_cached_blocks_before_rejecting_a_new_class() {
        let mut allocator = IsolateAllocator::new(AllocatorStrategy::SizeClassPool, 128);
        let first = allocator.allocate(layout(64, 8)).expect("first 64-byte block");
        let second = allocator.allocate(layout(64, 8)).expect("second 64-byte block");
        allocator.deallocate(first).expect("cache first block");
        allocator.deallocate(second).expect("cache second block");

        let replacement = allocator
            .allocate(layout(96, 8))
            .expect("evict cached blocks and allocate larger class");
        assert_eq!(allocator.stats().cache_evictions, 2);
        assert_eq!(allocator.stats().reserved_bytes, 128);
        allocator.deallocate(replacement).expect("cache replacement");
        assert!(allocator.stats().peak_reserved_bytes <= allocator.quota_bytes());
    }

    #[test]
    fn compile_unit_arena_aligns_allocations_and_reuses_chunks_after_reset() {
        let mut arena = CompileUnitArena::new(ARENA_CHUNK_BYTES * 2);
        let first = arena.allocate(layout(3, 1)).expect("first node");
        let aligned = arena.allocate(layout(32, 16)).expect("aligned node");
        assert_eq!(aligned.as_ptr() as usize % 16, 0);
        let reserved = arena.stats().reserved_bytes;
        assert!(reserved >= ARENA_CHUNK_BYTES);

        arena.reset().expect("reset compile unit");
        let reused = arena.allocate(layout(3, 1)).expect("reused first node");
        assert_eq!(first, reused);
        assert_eq!(arena.stats().reserved_bytes, reserved);
        assert_eq!(arena.stats().resets, 1);
    }

    #[test]
    fn compile_unit_arena_enforces_quota_and_reset_does_not_release_capacity() {
        let mut arena = CompileUnitArena::new(ARENA_CHUNK_BYTES);
        arena
            .allocate(layout(ARENA_CHUNK_BYTES - 32, 8))
            .expect("first large allocation");
        assert_eq!(
            arena.allocate(layout(128, 8)).unwrap_err(),
            AllocationError::QuotaExceeded
        );
        assert_eq!(arena.stats().reserved_bytes, ARENA_CHUNK_BYTES);
        arena.reset().expect("reset after full compile unit");
        arena.allocate(layout(128, 8)).expect("reuse reserved chunk");
        assert_eq!(arena.stats().reserved_bytes, ARENA_CHUNK_BYTES);
    }

    #[test]
    fn zero_sized_allocations_are_backed_safely_and_accounted_as_zero() {
        let mut allocator = IsolateAllocator::new(AllocatorStrategy::System, 64);
        let zero = allocator.allocate(layout(0, 8)).expect("zero-sized layout");
        assert_eq!(zero.requested_size(), 0);
        assert_eq!(zero.backing_size(), 1);
        allocator.deallocate(zero).expect("release zero-sized allocation");
    }
}
