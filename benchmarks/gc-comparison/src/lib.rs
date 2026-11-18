#![forbid(unsafe_code)]

//! OGM-26 M0 comparison harness. This is a model heap, not a JS Runtime.

use std::fmt;
use std::mem::size_of;
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectorKind {
    RcCycleRemoval,
    MarkSweep,
}

impl CollectorKind {
    pub const ALL: [Self; 2] = [Self::RcCycleRemoval, Self::MarkSweep];

    pub const fn name(self) -> &'static str {
        match self {
            Self::RcCycleRemoval => "rc-cycle-removal",
            Self::MarkSweep => "non-moving-mark-sweep",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GcRef {
    index: usize,
    generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostHandle {
    index: usize,
    generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind {
    Interpreter,
    NativePrototype,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameToken(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScopeToken(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GcError {
    StaleObjectRef,
    StaleHostHandle,
    RootCountOverflow,
    RefCountUnderflow,
    RefCountInvariant,
    InvalidFrame,
    InvalidScope,
    RootStackOrder,
    TokenSpaceExhausted,
}

impl fmt::Display for GcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for GcError {}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeapStats {
    pub allocations: u64,
    pub allocated_bytes: u64,
    pub reclaimed_objects: u64,
    pub reclaimed_bytes: u64,
    pub live_objects: u64,
    pub live_bytes: u64,
    pub peak_live_objects: u64,
    pub peak_heap_bytes: u64,
    pub refcount_operations: u64,
    pub root_operations: u64,
    pub edge_operations: u64,
    pub collections: u64,
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct RootCounts {
    pub interpreter_frame_roots: usize,
    pub native_frame_roots: usize,
    pub persistent_host_handles: usize,
    pub temporary_scope_roots: usize,
}

impl RootCounts {
    pub fn total(self) -> usize {
        self.interpreter_frame_roots
            + self.native_frame_roots
            + self.persistent_host_handles
            + self.temporary_scope_roots
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphShape {
    pub objects: usize,
    pub edges: usize,
    pub roots: RootCounts,
    pub fingerprint: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CollectionReport {
    pub collection_ns: u128,
    pub root_scan_ns: u128,
    pub roots_scanned: usize,
    pub edges_examined: u64,
    pub marked_objects: usize,
    pub reclaimed_objects: usize,
    pub reclaimed_bytes: u64,
    pub live_objects_after: u64,
    pub live_bytes_after: u64,
}

#[derive(Clone, Debug)]
struct ObjectCell {
    edges: Vec<GcRef>,
    ref_count: u64,
    payload_bytes: usize,
    logical_bytes: u64,
}

impl ObjectCell {
    fn new(payload_bytes: usize) -> Self {
        let logical_bytes = (size_of::<Self>() + payload_bytes) as u64;
        Self {
            edges: Vec::new(),
            ref_count: 0,
            payload_bytes,
            logical_bytes,
        }
    }
}

#[derive(Clone, Debug)]
struct ObjectSlot {
    generation: u64,
    cell: Option<ObjectCell>,
}

#[derive(Clone, Debug)]
struct RootFrame {
    token: FrameToken,
    kind: FrameKind,
    references: Vec<GcRef>,
}

#[derive(Clone, Debug)]
struct RootScope {
    token: ScopeToken,
    references: Vec<GcRef>,
}

#[derive(Clone, Copy, Debug)]
struct HostSlot {
    generation: u64,
    target: Option<GcRef>,
}

#[derive(Default)]
struct RootInventory {
    counts: RootCounts,
    references: Vec<GcRef>,
}

/// Shared object-slot layout with two alternative collection policies.
///
/// GcRef addresses internal heap slots. HostHandle is a separate external,
/// generation-checked persistent token whose registry entry roots its target.
pub struct GcHeap {
    collector: CollectorKind,
    slots: Vec<ObjectSlot>,
    free_slots: Vec<usize>,
    frames: Vec<RootFrame>,
    scopes: Vec<RootScope>,
    host_slots: Vec<HostSlot>,
    free_host_slots: Vec<usize>,
    next_token: u64,
    stats: HeapStats,
}

impl GcHeap {
    pub fn new(collector: CollectorKind) -> Self {
        Self {
            collector,
            slots: Vec::new(),
            free_slots: Vec::new(),
            frames: Vec::new(),
            scopes: Vec::new(),
            host_slots: Vec::new(),
            free_host_slots: Vec::new(),
            next_token: 1,
            stats: HeapStats::default(),
        }
    }

    pub fn collector(&self) -> CollectorKind {
        self.collector
    }

    pub fn stats(&self) -> HeapStats {
        self.stats
    }

    pub fn allocate(&mut self, payload_bytes: usize) -> GcRef {
        let cell = ObjectCell::new(payload_bytes);
        let logical_bytes = cell.logical_bytes;

        let index = if let Some(index) = self.free_slots.pop() {
            let slot = &mut self.slots[index];
            debug_assert!(slot.cell.is_none());
            debug_assert_ne!(slot.generation, u64::MAX);
            slot.cell = Some(cell);
            index
        } else {
            let index = self.slots.len();
            self.slots.push(ObjectSlot {
                generation: 1,
                cell: Some(cell),
            });
            index
        };

        let generation = self.slots[index].generation;
        self.stats.allocations += 1;
        self.stats.allocated_bytes += logical_bytes;
        self.stats.live_objects += 1;
        self.stats.live_bytes += logical_bytes;
        self.stats.peak_live_objects = self.stats.peak_live_objects.max(self.stats.live_objects);
        self.stats.peak_heap_bytes = self.stats.peak_heap_bytes.max(self.stats.live_bytes);

        GcRef { index, generation }
    }

    pub fn add_edge(&mut self, from: GcRef, to: GcRef) -> Result<(), GcError> {
        self.validate(from)?;
        self.validate(to)?;

        // Retain first so a checked refcount overflow cannot leave an edge
        // installed without the matching inbound reference count.
        if self.collector == CollectorKind::RcCycleRemoval {
            self.increment_ref(to)?;
        }

        let edge_bytes = size_of::<GcRef>() as u64;
        {
            let parent = self.cell_mut(from)?;
            parent.edges.push(to);
            parent.logical_bytes += edge_bytes;
        }
        self.stats.live_bytes += edge_bytes;
        self.stats.peak_heap_bytes = self.stats.peak_heap_bytes.max(self.stats.live_bytes);
        self.stats.edge_operations += 1;
        Ok(())
    }

    pub fn remove_edge(&mut self, from: GcRef, to: GcRef) -> Result<(), GcError> {
        let parent = self.cell_mut(from)?;
        let position = parent
            .edges
            .iter()
            .position(|candidate| *candidate == to)
            .ok_or(GcError::StaleObjectRef)?;
        parent.edges.remove(position);
        parent.logical_bytes -= size_of::<GcRef>() as u64;
        self.stats.live_bytes -= size_of::<GcRef>() as u64;
        self.stats.edge_operations += 1;

        if self.collector == CollectorKind::RcCycleRemoval {
            self.release_ref(to)?;
        }
        Ok(())
    }

    pub fn begin_frame(&mut self, kind: FrameKind) -> Result<FrameToken, GcError> {
        let token = FrameToken(self.next_token()?);
        self.frames.push(RootFrame {
            token,
            kind,
            references: Vec::new(),
        });
        Ok(token)
    }

    pub fn add_frame_root(&mut self, frame: FrameToken, reference: GcRef) -> Result<(), GcError> {
        self.validate(reference)?;
        if self.frames.last().ok_or(GcError::InvalidFrame)?.token != frame {
            return Err(GcError::RootStackOrder);
        }
        self.retain_ref(reference)?;
        self.frames
            .last_mut()
            .ok_or(GcError::InvalidFrame)?
            .references
            .push(reference);
        Ok(())
    }

    pub fn end_frame(&mut self, frame: FrameToken) -> Result<(), GcError> {
        let current = self.frames.last().ok_or(GcError::InvalidFrame)?;
        if current.token != frame {
            return Err(GcError::RootStackOrder);
        }
        let removed = self.frames.pop().ok_or(GcError::InvalidFrame)?;
        for reference in removed.references {
            self.release_ref(reference)?;
        }
        Ok(())
    }

    pub fn open_scope(&mut self) -> Result<ScopeToken, GcError> {
        let token = ScopeToken(self.next_token()?);
        self.scopes.push(RootScope {
            token,
            references: Vec::new(),
        });
        Ok(token)
    }

    pub fn scope_add(&mut self, scope: ScopeToken, reference: GcRef) -> Result<(), GcError> {
        self.validate(reference)?;
        if self.scopes.last().ok_or(GcError::InvalidScope)?.token != scope {
            return Err(GcError::RootStackOrder);
        }
        self.retain_ref(reference)?;
        self.scopes
            .last_mut()
            .ok_or(GcError::InvalidScope)?
            .references
            .push(reference);
        Ok(())
    }

    pub fn close_scope(&mut self, scope: ScopeToken) -> Result<(), GcError> {
        let current = self.scopes.last().ok_or(GcError::InvalidScope)?;
        if current.token != scope {
            return Err(GcError::RootStackOrder);
        }
        let removed = self.scopes.pop().ok_or(GcError::InvalidScope)?;
        for reference in removed.references {
            self.release_ref(reference)?;
        }
        Ok(())
    }

    pub fn retain_host_handle(&mut self, reference: GcRef) -> Result<HostHandle, GcError> {
        self.validate(reference)?;
        // A failed retain must not publish a host slot without returning a token.
        self.retain_ref(reference)?;
        let index = if let Some(index) = self.free_host_slots.pop() {
            let slot = &mut self.host_slots[index];
            debug_assert!(slot.target.is_none());
            debug_assert_ne!(slot.generation, u64::MAX);
            slot.target = Some(reference);
            index
        } else {
            let index = self.host_slots.len();
            self.host_slots.push(HostSlot {
                generation: 1,
                target: Some(reference),
            });
            index
        };
        Ok(HostHandle {
            index,
            generation: self.host_slots[index].generation,
        })
    }

    pub fn resolve_host_handle(&self, handle: HostHandle) -> Result<GcRef, GcError> {
        let slot = self
            .host_slots
            .get(handle.index)
            .ok_or(GcError::StaleHostHandle)?;
        if slot.generation != handle.generation {
            return Err(GcError::StaleHostHandle);
        }
        let reference = slot.target.ok_or(GcError::StaleHostHandle)?;
        self.validate(reference)?;
        Ok(reference)
    }

    pub fn release_host_handle(&mut self, handle: HostHandle) -> Result<(), GcError> {
        let slot = self
            .host_slots
            .get_mut(handle.index)
            .ok_or(GcError::StaleHostHandle)?;
        if slot.generation != handle.generation {
            return Err(GcError::StaleHostHandle);
        }
        let reference = slot.target.take().ok_or(GcError::StaleHostHandle)?;
        slot.generation = slot.generation.saturating_add(1);
        let reusable = slot.generation != u64::MAX;
        if reusable {
            self.free_host_slots.push(handle.index);
        }
        self.release_ref(reference)
    }

    pub fn is_alive(&self, reference: GcRef) -> bool {
        self.validate(reference).is_ok()
    }

    pub fn live_objects(&self) -> u64 {
        self.stats.live_objects
    }

    pub fn graph_shape(&self) -> Result<GraphShape, GcError> {
        let roots = self.root_inventory()?;
        let mut edges = 0_usize;
        let mut hash = 0xcbf29ce484222325_u64;

        for (index, slot) in self.slots.iter().enumerate() {
            if let Some(cell) = &slot.cell {
                edges += cell.edges.len();
                hash_u64(&mut hash, index as u64);
                hash_u64(&mut hash, slot.generation);
                hash_u64(&mut hash, cell.payload_bytes as u64);
                for edge in &cell.edges {
                    hash_u64(&mut hash, edge.index as u64);
                    hash_u64(&mut hash, edge.generation);
                }
            }
        }

        for frame in &self.frames {
            let tag = match frame.kind {
                FrameKind::Interpreter => 1,
                FrameKind::NativePrototype => 2,
            };
            hash_u64(&mut hash, tag);
            for reference in &frame.references {
                hash_u64(&mut hash, reference.index as u64);
                hash_u64(&mut hash, reference.generation);
            }
        }
        hash_u64(&mut hash, 3);
        for slot in &self.host_slots {
            if let Some(reference) = slot.target {
                hash_u64(&mut hash, reference.index as u64);
                hash_u64(&mut hash, reference.generation);
            }
        }
        hash_u64(&mut hash, 4);
        for scope in &self.scopes {
            for reference in &scope.references {
                hash_u64(&mut hash, reference.index as u64);
                hash_u64(&mut hash, reference.generation);
            }
        }

        Ok(GraphShape {
            objects: self.stats.live_objects as usize,
            edges,
            roots: roots.counts,
            fingerprint: format!("{hash:016x}"),
        })
    }

    pub fn collect(&mut self) -> Result<CollectionReport, GcError> {
        let collection_start = Instant::now();
        let root_scan_start = Instant::now();
        let inventory = self.root_inventory()?;
        let root_scan_ns = root_scan_start.elapsed().as_nanos();

        // Per-slot root multiplicities are only needed by trial deletion. Keep
        // this RC-specific bookkeeping out of the tracing collector's hot path
        // and out of the shared root-inventory timing.
        let root_counts = if self.collector == CollectorKind::RcCycleRemoval {
            let mut counts = vec![0_u64; self.slots.len()];
            for reference in &inventory.references {
                self.validate(*reference)?;
                counts[reference.index] = counts[reference.index]
                    .checked_add(1)
                    .ok_or(GcError::RootCountOverflow)?;
            }
            Some(counts)
        } else {
            None
        };

        let (marked, marked_objects, edges_examined) = match self.collector {
            CollectorKind::RcCycleRemoval => {
                let root_counts = root_counts.as_deref().ok_or(GcError::RefCountInvariant)?;
                let mut trial_counts = vec![0_u64; self.slots.len()];
                for (index, slot) in self.slots.iter().enumerate() {
                    if let Some(cell) = &slot.cell {
                        trial_counts[index] = cell.ref_count;
                    }
                }

                let mut trial_edges = 0_u64;
                for index in 0..self.slots.len() {
                    let edge_count = self.slots[index]
                        .cell
                        .as_ref()
                        .map_or(0, |cell| cell.edges.len());
                    for edge_index in 0..edge_count {
                        let target = self.slots[index]
                            .cell
                            .as_ref()
                            .ok_or(GcError::RefCountInvariant)?
                            .edges[edge_index];
                        self.validate(target)?;
                        trial_counts[target.index] = trial_counts[target.index]
                            .checked_sub(1)
                            .ok_or(GcError::RefCountInvariant)?;
                        trial_edges += 1;
                    }
                }

                self.stats.refcount_operations += trial_edges;
                for (index, slot) in self.slots.iter().enumerate() {
                    if slot.cell.is_some() && trial_counts[index] != root_counts[index] {
                        return Err(GcError::RefCountInvariant);
                    }
                }

                let seeds = self.slots.iter().enumerate().filter_map(|(index, slot)| {
                    if slot.cell.is_some() && trial_counts[index] > 0 {
                        Some(GcRef {
                            index,
                            generation: slot.generation,
                        })
                    } else {
                        None
                    }
                });
                let (marked, reachable, traversal_edges) = self.mark_reachable(seeds)?;
                (marked, reachable, trial_edges + traversal_edges)
            }
            CollectorKind::MarkSweep => {
                let (marked, reachable, edges) =
                    self.mark_reachable(inventory.references.iter().copied())?;
                (marked, reachable, edges)
            }
        };

        let mut dead = vec![false; self.slots.len()];
        for (index, slot) in self.slots.iter().enumerate() {
            dead[index] = slot.cell.is_some() && !marked[index];
        }

        let (reclaimed_objects, reclaimed_bytes) = self.sweep(&dead);
        if let Some(root_counts) = root_counts.as_deref() {
            self.recompute_ref_counts(root_counts)?;
        }

        self.stats.collections += 1;
        Ok(CollectionReport {
            collection_ns: collection_start.elapsed().as_nanos(),
            root_scan_ns,
            roots_scanned: inventory.references.len(),
            edges_examined,
            marked_objects,
            reclaimed_objects,
            reclaimed_bytes,
            live_objects_after: self.stats.live_objects,
            live_bytes_after: self.stats.live_bytes,
        })
    }

    fn next_token(&mut self) -> Result<u64, GcError> {
        let token = self.next_token;
        self.next_token = self
            .next_token
            .checked_add(1)
            .ok_or(GcError::TokenSpaceExhausted)?;
        Ok(token)
    }

    fn validate(&self, reference: GcRef) -> Result<(), GcError> {
        let slot = self
            .slots
            .get(reference.index)
            .ok_or(GcError::StaleObjectRef)?;
        if slot.generation != reference.generation || slot.cell.is_none() {
            return Err(GcError::StaleObjectRef);
        }
        Ok(())
    }

    fn cell(&self, reference: GcRef) -> Result<&ObjectCell, GcError> {
        self.validate(reference)?;
        self.slots[reference.index]
            .cell
            .as_ref()
            .ok_or(GcError::StaleObjectRef)
    }

    fn cell_mut(&mut self, reference: GcRef) -> Result<&mut ObjectCell, GcError> {
        self.validate(reference)?;
        self.slots[reference.index]
            .cell
            .as_mut()
            .ok_or(GcError::StaleObjectRef)
    }

    fn retain_ref(&mut self, reference: GcRef) -> Result<(), GcError> {
        self.validate(reference)?;
        if self.collector == CollectorKind::RcCycleRemoval {
            self.increment_ref(reference)?;
        }
        self.stats.root_operations += 1;
        Ok(())
    }

    fn increment_ref(&mut self, reference: GcRef) -> Result<(), GcError> {
        let cell = self.cell_mut(reference)?;
        cell.ref_count = cell
            .ref_count
            .checked_add(1)
            .ok_or(GcError::RootCountOverflow)?;
        self.stats.refcount_operations += 1;
        Ok(())
    }

    fn release_ref(&mut self, reference: GcRef) -> Result<(), GcError> {
        self.validate(reference)?;
        if self.collector != CollectorKind::RcCycleRemoval {
            self.stats.root_operations += 1;
            return Ok(());
        }

        let mut pending = vec![reference];
        while let Some(current) = pending.pop() {
            let reclaimed_cell = {
                let slot = self
                    .slots
                    .get_mut(current.index)
                    .ok_or(GcError::StaleObjectRef)?;
                if slot.generation != current.generation {
                    return Err(GcError::StaleObjectRef);
                }
                let cell = slot.cell.as_mut().ok_or(GcError::StaleObjectRef)?;
                cell.ref_count = cell
                    .ref_count
                    .checked_sub(1)
                    .ok_or(GcError::RefCountUnderflow)?;
                self.stats.refcount_operations += 1;

                if cell.ref_count == 0 {
                    let reclaimed = slot.cell.take().ok_or(GcError::StaleObjectRef)?;
                    slot.generation = slot.generation.saturating_add(1);
                    let reusable = slot.generation != u64::MAX;
                    Some((reclaimed, reusable))
                } else {
                    None
                }
            };

            if let Some((reclaimed, reusable)) = reclaimed_cell {
                self.stats.live_objects -= 1;
                self.stats.live_bytes -= reclaimed.logical_bytes;
                self.stats.reclaimed_objects += 1;
                self.stats.reclaimed_bytes += reclaimed.logical_bytes;
                if reusable {
                    self.free_slots.push(current.index);
                }
                pending.extend(reclaimed.edges);
            }
        }
        self.stats.root_operations += 1;
        Ok(())
    }

    fn root_inventory(&self) -> Result<RootInventory, GcError> {
        let mut inventory = RootInventory::default();

        for frame in &self.frames {
            match frame.kind {
                FrameKind::Interpreter => {
                    inventory.counts.interpreter_frame_roots += frame.references.len()
                }
                FrameKind::NativePrototype => {
                    inventory.counts.native_frame_roots += frame.references.len()
                }
            }
            inventory
                .references
                .extend(frame.references.iter().copied());
        }

        for slot in &self.host_slots {
            if let Some(reference) = slot.target {
                inventory.counts.persistent_host_handles += 1;
                inventory.references.push(reference);
            }
        }

        for scope in &self.scopes {
            inventory.counts.temporary_scope_roots += scope.references.len();
            inventory
                .references
                .extend(scope.references.iter().copied());
        }

        for reference in &inventory.references {
            self.validate(*reference)?;
        }
        Ok(inventory)
    }

    fn mark_reachable<I>(&self, seeds: I) -> Result<(Vec<bool>, usize, u64), GcError>
    where
        I: IntoIterator<Item = GcRef>,
    {
        let mut marked = vec![false; self.slots.len()];
        let mut stack = Vec::new();

        for reference in seeds {
            self.validate(reference)?;
            if !marked[reference.index] {
                marked[reference.index] = true;
                stack.push(reference);
            }
        }

        let mut edges_examined = 0_u64;
        while let Some(reference) = stack.pop() {
            let edge_count = self.cell(reference)?.edges.len();
            for edge_index in 0..edge_count {
                let target = self.cell(reference)?.edges[edge_index];
                self.validate(target)?;
                edges_examined += 1;
                if !marked[target.index] {
                    marked[target.index] = true;
                    stack.push(target);
                }
            }
        }

        let marked_objects = marked.iter().filter(|value| **value).count();
        Ok((marked, marked_objects, edges_examined))
    }

    fn sweep(&mut self, dead: &[bool]) -> (usize, u64) {
        let mut reclaimed_objects = 0_usize;
        let mut reclaimed_bytes = 0_u64;

        for (index, is_dead) in dead.iter().copied().enumerate() {
            if !is_dead {
                continue;
            }
            let slot = &mut self.slots[index];
            if let Some(cell) = slot.cell.take() {
                reclaimed_objects += 1;
                reclaimed_bytes += cell.logical_bytes;
                slot.generation = slot.generation.saturating_add(1);
                if slot.generation != u64::MAX {
                    self.free_slots.push(index);
                }
            }
        }

        self.stats.live_objects -= reclaimed_objects as u64;
        self.stats.live_bytes -= reclaimed_bytes;
        self.stats.reclaimed_objects += reclaimed_objects as u64;
        self.stats.reclaimed_bytes += reclaimed_bytes;

        (reclaimed_objects, reclaimed_bytes)
    }

    fn recompute_ref_counts(&mut self, root_counts: &[u64]) -> Result<(), GcError> {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if let Some(cell) = &mut slot.cell {
                cell.ref_count = *root_counts.get(index).ok_or(GcError::RefCountInvariant)?;
            }
        }

        for source_index in 0..self.slots.len() {
            let edge_count = self.slots[source_index]
                .cell
                .as_ref()
                .map_or(0, |cell| cell.edges.len());
            for edge_index in 0..edge_count {
                let target = self.slots[source_index]
                    .cell
                    .as_ref()
                    .ok_or(GcError::RefCountInvariant)?
                    .edges[edge_index];
                self.validate(target)?;
                let target_cell = self.cell_mut(target)?;
                target_cell.ref_count = target_cell
                    .ref_count
                    .checked_add(1)
                    .ok_or(GcError::RootCountOverflow)?;
                self.stats.refcount_operations += 1;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn verify_ref_counts(&self) -> Result<(), GcError> {
        if self.collector != CollectorKind::RcCycleRemoval {
            return Ok(());
        }
        let inventory = self.root_inventory()?;
        let mut counts = vec![0_u64; self.slots.len()];
        for reference in inventory.references {
            counts[reference.index] += 1;
        }
        for slot in &self.slots {
            if let Some(cell) = &slot.cell {
                for edge in &cell.edges {
                    self.validate(*edge)?;
                    counts[edge.index] += 1;
                }
            }
        }
        for (index, slot) in self.slots.iter().enumerate() {
            if let Some(cell) = &slot.cell {
                if cell.ref_count != counts[index] {
                    return Err(GcError::RefCountInvariant);
                }
            }
        }
        Ok(())
    }
}

fn hash_u64(hash: &mut u64, value: u64) {
    for byte in value.to_le_bytes() {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(0x100000001b3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidates() -> [CollectorKind; 2] {
        CollectorKind::ALL
    }

    #[test]
    fn failed_reference_retains_do_not_partially_mutate_edges_or_root_registries() {
        let mut heap = GcHeap::new(CollectorKind::RcCycleRemoval);
        let source = heap.allocate(8);
        let target = heap.allocate(8);
        heap.cell_mut(target).unwrap().ref_count = u64::MAX;

        assert_eq!(
            heap.add_edge(source, target),
            Err(GcError::RootCountOverflow)
        );
        assert!(heap.cell(source).unwrap().edges.is_empty());

        let frame = heap.begin_frame(FrameKind::Interpreter).unwrap();
        assert_eq!(
            heap.add_frame_root(frame, target),
            Err(GcError::RootCountOverflow)
        );
        assert!(heap.frames.last().unwrap().references.is_empty());

        let scope = heap.open_scope().unwrap();
        assert_eq!(
            heap.scope_add(scope, target),
            Err(GcError::RootCountOverflow)
        );
        assert!(heap.scopes.last().unwrap().references.is_empty());

        assert_eq!(
            heap.retain_host_handle(target),
            Err(GcError::RootCountOverflow)
        );
        assert!(heap.host_slots.is_empty());
    }

    #[test]
    fn both_collectors_preserve_reachable_cycles_and_reclaim_unreachable_cycles() {
        for collector in candidates() {
            let mut heap = GcHeap::new(collector);
            let a = heap.allocate(32);
            let b = heap.allocate(32);
            let c = heap.allocate(32);
            let d = heap.allocate(32);
            heap.add_edge(a, b).unwrap();
            heap.add_edge(b, a).unwrap();
            heap.add_edge(c, d).unwrap();
            heap.add_edge(d, c).unwrap();

            let frame = heap.begin_frame(FrameKind::Interpreter).unwrap();
            heap.add_frame_root(frame, a).unwrap();

            let report = heap.collect().unwrap();
            assert_eq!(report.live_objects_after, 2);
            assert_eq!(report.reclaimed_objects, 2);
            assert!(heap.is_alive(a));
            assert!(heap.is_alive(b));
            assert!(!heap.is_alive(c));
            assert!(!heap.is_alive(d));

            heap.end_frame(frame).unwrap();
            heap.collect().unwrap();
            assert_eq!(heap.live_objects(), 0);
        }
    }

    #[test]
    fn all_four_root_categories_retain_and_then_release_objects() {
        for collector in candidates() {
            let mut heap = GcHeap::new(collector);
            let interpreter = heap.allocate(8);
            let native = heap.allocate(8);
            let persistent = heap.allocate(8);
            let temporary = heap.allocate(8);

            let interpreter_frame = heap.begin_frame(FrameKind::Interpreter).unwrap();
            heap.add_frame_root(interpreter_frame, interpreter).unwrap();
            let native_frame = heap.begin_frame(FrameKind::NativePrototype).unwrap();
            heap.add_frame_root(native_frame, native).unwrap();
            let host_handle = heap.retain_host_handle(persistent).unwrap();
            let scope = heap.open_scope().unwrap();
            heap.scope_add(scope, temporary).unwrap();

            let roots = heap.graph_shape().unwrap().roots;
            assert_eq!(roots.interpreter_frame_roots, 1);
            assert_eq!(roots.native_frame_roots, 1);
            assert_eq!(roots.persistent_host_handles, 1);
            assert_eq!(roots.temporary_scope_roots, 1);
            assert_eq!(heap.collect().unwrap().live_objects_after, 4);

            heap.close_scope(scope).unwrap();
            heap.release_host_handle(host_handle).unwrap();
            heap.end_frame(native_frame).unwrap();
            heap.end_frame(interpreter_frame).unwrap();
            heap.collect().unwrap();
            assert_eq!(heap.live_objects(), 0);
            assert_eq!(
                heap.resolve_host_handle(host_handle),
                Err(GcError::StaleHostHandle)
            );
            assert!(!heap.is_alive(interpreter));
            assert!(!heap.is_alive(native));
            assert!(!heap.is_alive(persistent));
            assert!(!heap.is_alive(temporary));
        }
    }

    #[test]
    fn temporary_scopes_and_frames_are_strictly_lifo() {
        for collector in candidates() {
            let mut heap = GcHeap::new(collector);
            let value = heap.allocate(8);
            let outer = heap.open_scope().unwrap();
            let inner = heap.open_scope().unwrap();
            assert_eq!(heap.close_scope(outer), Err(GcError::RootStackOrder));
            heap.close_scope(inner).unwrap();
            heap.close_scope(outer).unwrap();

            let outer_frame = heap.begin_frame(FrameKind::Interpreter).unwrap();
            heap.add_frame_root(outer_frame, value).unwrap();
            let inner_frame = heap.begin_frame(FrameKind::NativePrototype).unwrap();
            assert_eq!(heap.end_frame(outer_frame), Err(GcError::RootStackOrder));
            heap.end_frame(inner_frame).unwrap();
            heap.end_frame(outer_frame).unwrap();
        }
    }

    #[test]
    fn released_host_handles_and_reused_heap_slots_reject_stale_generations() {
        for collector in candidates() {
            let mut heap = GcHeap::new(collector);
            let original = heap.allocate(16);
            let host = heap.retain_host_handle(original).unwrap();
            heap.release_host_handle(host).unwrap();
            assert_eq!(
                heap.resolve_host_handle(host),
                Err(GcError::StaleHostHandle)
            );
            heap.collect().unwrap();
            let replacement = heap.allocate(16);
            assert_eq!(original.index, replacement.index);
            assert_ne!(original.generation, replacement.generation);
            assert!(!heap.is_alive(original));
            assert!(heap.is_alive(replacement));

            let replacement_host = heap.retain_host_handle(replacement).unwrap();
            assert_eq!(host.index, replacement_host.index);
            assert_ne!(host.generation, replacement_host.generation);
            assert_eq!(
                heap.resolve_host_handle(host),
                Err(GcError::StaleHostHandle)
            );
            assert_eq!(heap.resolve_host_handle(replacement_host), Ok(replacement));
        }
    }

    #[test]
    fn trial_deletion_keeps_objects_reachable_from_any_root_provider() {
        let mut heap = GcHeap::new(CollectorKind::RcCycleRemoval);
        let root = heap.allocate(8);
        let child = heap.allocate(8);
        let orphan_a = heap.allocate(8);
        let orphan_b = heap.allocate(8);
        heap.add_edge(root, child).unwrap();
        heap.add_edge(child, root).unwrap();
        heap.add_edge(orphan_a, orphan_b).unwrap();
        heap.add_edge(orphan_b, orphan_a).unwrap();

        let scope = heap.open_scope().unwrap();
        heap.scope_add(scope, root).unwrap();
        heap.verify_ref_counts().unwrap();

        let report = heap.collect().unwrap();
        assert_eq!(report.live_objects_after, 2);
        assert!(heap.is_alive(root));
        assert!(heap.is_alive(child));
        assert!(!heap.is_alive(orphan_a));
        assert!(!heap.is_alive(orphan_b));
        heap.verify_ref_counts().unwrap();

        heap.close_scope(scope).unwrap();
        heap.collect().unwrap();
        assert_eq!(heap.live_objects(), 0);
    }

    #[test]
    fn rootless_reference_counted_objects_are_reclaimed_by_cycle_pass() {
        let mut heap = GcHeap::new(CollectorKind::RcCycleRemoval);
        let a = heap.allocate(8);
        let b = heap.allocate(8);
        heap.add_edge(a, b).unwrap();
        heap.add_edge(b, a).unwrap();
        heap.verify_ref_counts().unwrap();

        let report = heap.collect().unwrap();
        assert_eq!(report.reclaimed_objects, 2);
        assert_eq!(heap.live_objects(), 0);
        assert!(!heap.is_alive(a));
        assert!(!heap.is_alive(b));
    }

    #[test]
    fn removing_the_last_external_reference_reclaims_acyclic_rc_chains_immediately() {
        let mut heap = GcHeap::new(CollectorKind::RcCycleRemoval);
        let parent = heap.allocate(8);
        let child = heap.allocate(8);
        heap.add_edge(parent, child).unwrap();
        let frame = heap.begin_frame(FrameKind::Interpreter).unwrap();
        heap.add_frame_root(frame, parent).unwrap();
        heap.verify_ref_counts().unwrap();

        heap.end_frame(frame).unwrap();
        assert_eq!(heap.live_objects(), 0);
        assert!(!heap.is_alive(parent));
        assert!(!heap.is_alive(child));
    }
}
