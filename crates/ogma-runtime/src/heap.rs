//! Non-moving, generation-checked Runtime heap and explicit root providers.

use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::value::{Value, ValueRepr};

static NEXT_HEAP_ID: AtomicU64 = AtomicU64::new(1);

/// Per-Runtime allocation policy.
///
/// The quota is charged for heap slot-table capacity, UTF-16 payload capacity,
/// object-edge capacity, root/handle registry capacity and explicitly charged
/// external memory. The maximum limits one backing allocation request; it is
/// not a substitute for the aggregate quota.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeapConfig {
    /// Maximum aggregate charged bytes owned by this Runtime.
    pub heap_quota_bytes: usize,
    /// Maximum size of one allocation or external-memory charge.
    pub max_allocation_bytes: usize,
}

impl HeapConfig {
    /// Creates a heap configuration.
    #[must_use]
    pub const fn new(heap_quota_bytes: usize, max_allocation_bytes: usize) -> Self {
        Self {
            heap_quota_bytes,
            max_allocation_bytes,
        }
    }
}

impl Default for HeapConfig {
    fn default() -> Self {
        Self::new(64 * 1024 * 1024, 16 * 1024 * 1024)
    }
}

/// Fail-closed Runtime heap errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeapError {
    /// A single allocation or external charge exceeds the configured limit.
    AllocationTooLarge {
        /// Requested bytes.
        requested_bytes: usize,
        /// Configured maximum allocation.
        maximum_bytes: usize,
    },
    /// The aggregate Runtime allocation quota would be exceeded.
    QuotaExceeded {
        /// Requested additional bytes.
        requested_bytes: usize,
        /// Bytes still available.
        available_bytes: usize,
    },
    /// The platform allocator could not satisfy a reservation.
    AllocationFailed,
    /// A size or capacity calculation overflowed.
    SizeOverflow,
    /// The requested slot index cannot be represented by the handle format.
    HeapCapacityExceeded,
    /// A unique Runtime identity could not be allocated.
    HeapIdentityExhausted,
    /// A Runtime value belongs to another heap.
    WrongHeap,
    /// An internal object reference is stale or invalid.
    StaleReference,
    /// An external persistent handle is stale or invalid.
    StaleHostHandle,
    /// An external-memory charge is stale or invalid.
    StaleExternalCharge,
    /// An operation requires a managed object/function reference.
    ReferenceRequired,
    /// An operation is not valid for this managed cell kind.
    InvalidObjectKind,
    /// A requested object edge does not exist.
    EdgeNotFound,
    /// A root frame token is invalid.
    InvalidFrame,
    /// A temporary scope token is invalid.
    InvalidScope,
    /// A root frame or scope was not closed in strict LIFO order.
    RootStackOrder,
    /// Root token space is exhausted.
    RootTokenExhausted,
    /// A generation counter reached its terminal value.
    GenerationExhausted,
    /// Runtime heap accounting invariants were violated.
    AccountingInvariant,
    /// The requested ECMAScript coercion is outside the current M1 subset.
    UnsupportedCoercion,
}

impl std::fmt::Display for HeapError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for HeapError {}

/// Internal object identity. It is never exposed through the host API.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GcRef {
    pub(crate) heap_id: u64,
    pub(crate) index: u32,
    pub(crate) generation: u32,
}

/// Opaque persistent host identity.
///
/// The fields and representation are private. A handle is meaningful only to
/// the heap that created it and never contains an object pointer or Value bits.
/// It is also !Send and !Sync: callers must keep it on the owning Runtime thread.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct HostHandle {
    heap_id: u64,
    index: u32,
    generation: u32,
    _thread_affinity: PhantomData<Rc<()>>,
}

/// Opaque token for an active interpreter root frame. It is !Send and !Sync.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameToken {
    heap_id: u64,
    serial: u64,
    _thread_affinity: PhantomData<Rc<()>>,
}

/// Opaque token for an active temporary root scope. It is !Send and !Sync.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScopeToken {
    heap_id: u64,
    serial: u64,
    _thread_affinity: PhantomData<Rc<()>>,
}

/// Runtime-owned token for externally allocated memory charged to the heap.
/// It is !Send and !Sync and must be released on the owning Runtime thread.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExternalMemoryCharge {
    heap_id: u64,
    index: u32,
    generation: u32,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl fmt::Debug for HostHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HostHandle(<opaque>)")
    }
}

impl fmt::Debug for FrameToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FrameToken(<opaque>)")
    }
}

impl fmt::Debug for ScopeToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ScopeToken(<opaque>)")
    }
}

impl fmt::Debug for ExternalMemoryCharge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ExternalMemoryCharge(<opaque>)")
    }
}

/// Explicitly enumerable root-provider counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RootSnapshot {
    /// Number of active interpreter frames.
    pub interpreter_frames: usize,
    /// Number of managed references held by interpreter frames.
    pub interpreter_frame_roots: usize,
    /// Number of active temporary root scopes.
    pub temporary_scopes: usize,
    /// Number of managed references held by temporary scopes.
    pub temporary_scope_roots: usize,
    /// Number of live persistent host handles that refer to managed values.
    pub persistent_host_roots: usize,
}

/// Aggregate heap accounting and collection counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeapStats {
    /// Currently charged bytes, including external charges.
    pub used_bytes: usize,
    /// Configured aggregate byte quota.
    pub quota_bytes: usize,
    /// Currently charged external-memory bytes.
    pub external_memory_bytes: usize,
    /// Peak charged bytes observed.
    pub peak_used_bytes: usize,
    /// Number of successfully published managed cells.
    pub allocations: u64,
    /// Number of reclaimed managed cells.
    pub reclaimed_objects: u64,
    /// Current live managed cell count.
    pub live_objects: u64,
    /// Number of completed tracing collections.
    pub collections: u64,
}

/// Result of one non-moving mark-sweep collection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CollectionReport {
    /// Number of managed roots visited across all root providers.
    pub roots_scanned: usize,
    /// Number of cells reachable from those roots.
    pub marked_objects: usize,
    /// Number of unreachable cells reclaimed.
    pub reclaimed_objects: usize,
    /// Charged cell, payload and edge bytes released by the collection.
    pub reclaimed_bytes: usize,
    /// Charged bytes remaining after sweeping.
    pub used_bytes_after: usize,
}

#[derive(Debug)]
enum CellKind {
    String(Vec<u16>),
    Object,
    Function,
}

#[derive(Debug)]
struct HeapCell {
    kind: CellKind,
    edges: Vec<GcRef>,
    marked: bool,
    mark_next: Option<u32>,
}

#[derive(Debug)]
struct CellStorage(Box<[HeapCell]>);

impl std::ops::Deref for CellStorage {
    type Target = HeapCell;

    fn deref(&self) -> &Self::Target {
        &self.0[0]
    }
}

impl std::ops::DerefMut for CellStorage {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0[0]
    }
}

#[derive(Debug)]
struct HeapSlot {
    generation: u32,
    cell: Option<CellStorage>,
    free_next: Option<u32>,
}

#[derive(Debug)]
struct RootFrame {
    serial: u64,
    roots: Vec<GcRef>,
}

#[derive(Debug)]
struct RootScope {
    serial: u64,
    roots: Vec<GcRef>,
}

#[derive(Clone, Copy, Debug)]
struct HostSlot {
    generation: u32,
    value: Option<Value>,
    free_next: Option<u32>,
}

#[derive(Clone, Copy, Debug)]
struct ExternalSlot {
    generation: u32,
    bytes: usize,
    active: bool,
    free_next: Option<u32>,
}

#[derive(Debug)]
struct HeapBudget {
    quota_bytes: usize,
    max_allocation_bytes: usize,
    used_bytes: usize,
    external_memory_bytes: usize,
    peak_used_bytes: usize,
}

impl HeapBudget {
    fn new(config: HeapConfig) -> Self {
        Self {
            quota_bytes: config.heap_quota_bytes,
            max_allocation_bytes: config.max_allocation_bytes,
            used_bytes: 0,
            external_memory_bytes: 0,
            peak_used_bytes: 0,
        }
    }

    fn charge(&mut self, requested_bytes: usize) -> Result<(), HeapError> {
        if requested_bytes > self.max_allocation_bytes {
            return Err(HeapError::AllocationTooLarge {
                requested_bytes,
                maximum_bytes: self.max_allocation_bytes,
            });
        }
        let available_bytes = self.quota_bytes.saturating_sub(self.used_bytes);
        if requested_bytes > available_bytes {
            return Err(HeapError::QuotaExceeded {
                requested_bytes,
                available_bytes,
            });
        }
        self.used_bytes = self
            .used_bytes
            .checked_add(requested_bytes)
            .ok_or(HeapError::SizeOverflow)?;
        self.peak_used_bytes = self.peak_used_bytes.max(self.used_bytes);
        Ok(())
    }

    fn release(&mut self, bytes: usize) -> Result<(), HeapError> {
        self.used_bytes = self
            .used_bytes
            .checked_sub(bytes)
            .ok_or(HeapError::AccountingInvariant)?;
        Ok(())
    }

    fn charge_external(&mut self, bytes: usize) -> Result<(), HeapError> {
        self.charge(bytes)?;
        self.external_memory_bytes = self
            .external_memory_bytes
            .checked_add(bytes)
            .ok_or(HeapError::SizeOverflow)?;
        Ok(())
    }

    fn release_external(&mut self, bytes: usize) -> Result<(), HeapError> {
        self.external_memory_bytes = self
            .external_memory_bytes
            .checked_sub(bytes)
            .ok_or(HeapError::AccountingInvariant)?;
        self.release(bytes)
    }
}

/// A thread-affine Runtime heap with explicit, inspectable root providers.
///
/// The collector is non-moving tracing mark-sweep. Internal object references
/// address generation-checked slots directly; persistent HostHandle resolution
/// is a separate external registry lookup. The heap is deliberately !Send and
/// !Sync so mutable Runtime state remains on its creating thread.
pub struct Heap {
    heap_id: u64,
    budget: HeapBudget,
    slots: Vec<HeapSlot>,
    free_slot_head: Option<u32>,
    mark_head: Option<u32>,
    frames: Vec<RootFrame>,
    scopes: Vec<RootScope>,
    host_slots: Vec<HostSlot>,
    free_host_head: Option<u32>,
    external_slots: Vec<ExternalSlot>,
    free_external_head: Option<u32>,
    next_root_serial: u64,
    allocations: u64,
    reclaimed_objects: u64,
    live_objects: u64,
    collections: u64,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl Heap {
    /// Creates an empty Runtime heap with the supplied hard allocation limits.
    pub fn new(config: HeapConfig) -> Result<Self, HeapError> {
        let heap_id = NEXT_HEAP_ID
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| HeapError::HeapIdentityExhausted)?;
        Ok(Self {
            heap_id,
            budget: HeapBudget::new(config),
            slots: Vec::new(),
            free_slot_head: None,
            mark_head: None,
            frames: Vec::new(),
            scopes: Vec::new(),
            host_slots: Vec::new(),
            free_host_head: None,
            external_slots: Vec::new(),
            free_external_head: None,
            next_root_serial: 1,
            allocations: 0,
            reclaimed_objects: 0,
            live_objects: 0,
            collections: 0,
            _not_send_or_sync: PhantomData,
        })
    }

    /// Returns a snapshot of current accounting and lifetime counters.
    #[must_use]
    pub fn stats(&self) -> HeapStats {
        HeapStats {
            used_bytes: self.budget.used_bytes,
            quota_bytes: self.budget.quota_bytes,
            external_memory_bytes: self.budget.external_memory_bytes,
            peak_used_bytes: self.budget.peak_used_bytes,
            allocations: self.allocations,
            reclaimed_objects: self.reclaimed_objects,
            live_objects: self.live_objects,
            collections: self.collections,
        }
    }

    /// Returns the current root-provider inventory without scanning Rust stacks.
    #[must_use]
    pub fn root_snapshot(&self) -> RootSnapshot {
        RootSnapshot {
            interpreter_frames: self.frames.len(),
            interpreter_frame_roots: self.frames.iter().map(|frame| frame.roots.len()).sum(),
            temporary_scopes: self.scopes.len(),
            temporary_scope_roots: self.scopes.iter().map(|scope| scope.roots.len()).sum(),
            persistent_host_roots: self
                .host_slots
                .iter()
                .filter(|slot| slot.value.is_some_and(|value| value.gc_ref().is_some()))
                .count(),
        }
    }

    /// Allocates a UTF-16 string while preserving unpaired surrogate code units.
    ///
    /// If quota pressure prevents the allocation, one collection is attempted
    /// before retrying. An oversized allocation is rejected without collecting.
    pub fn allocate_string_utf16(&mut self, units: &[u16]) -> Result<Value, HeapError> {
        match self.try_allocate_string_from_iter(units.len(), units.iter().copied()) {
            Err(HeapError::QuotaExceeded { .. }) => {
                self.collect()?;
                self.try_allocate_string_from_iter(units.len(), units.iter().copied())
            }
            result => result,
        }
    }

    /// Allocates a string from valid Rust Unicode text without an uncharged
    /// intermediate UTF-16 buffer.
    pub fn allocate_string(&mut self, text: &str) -> Result<Value, HeapError> {
        let unit_count = text.encode_utf16().count();
        match self.try_allocate_string_from_iter(unit_count, text.encode_utf16()) {
            Err(HeapError::QuotaExceeded { .. }) => {
                self.collect()?;
                self.try_allocate_string_from_iter(unit_count, text.encode_utf16())
            }
            result => result,
        }
    }

    /// Allocates an ordinary object cell.
    pub fn allocate_object(&mut self) -> Result<Value, HeapError> {
        match self.try_allocate_cell(CellKind::Object) {
            Err(HeapError::QuotaExceeded { .. }) => {
                self.collect()?;
                self.try_allocate_cell(CellKind::Object)
                    .map(Value::from_object)
            }
            result => result.map(Value::from_object),
        }
    }

    /// Allocates a callable object cell.
    pub fn allocate_function(&mut self) -> Result<Value, HeapError> {
        match self.try_allocate_cell(CellKind::Function) {
            Err(HeapError::QuotaExceeded { .. }) => {
                self.collect()?;
                self.try_allocate_cell(CellKind::Function)
                    .map(Value::from_function)
            }
            result => result.map(Value::from_function),
        }
    }

    /// Returns the exact UTF-16 code units for a managed string.
    pub fn string_utf16(&self, value: Value) -> Result<&[u16], HeapError> {
        let reference = match value.repr() {
            ValueRepr::String(reference) => reference,
            _ => return Err(HeapError::InvalidObjectKind),
        };
        let index = self.validate_ref(reference)?;
        match &self.slots[index]
            .cell
            .as_ref()
            .ok_or(HeapError::StaleReference)?
            .kind
        {
            CellKind::String(units) => Ok(units),
            CellKind::Object | CellKind::Function => Err(HeapError::InvalidObjectKind),
        }
    }

    /// Adds a managed-reference edge to an object or function cell.
    ///
    /// The edge is the heap-graph primitive used by object properties, elements
    /// and closure/environment storage as those M1 layers are implemented.
    pub fn add_reference(&mut self, parent: Value, child: Value) -> Result<(), HeapError> {
        let parent_ref = parent.gc_ref().ok_or(HeapError::ReferenceRequired)?;
        let child_ref = child.gc_ref().ok_or(HeapError::ReferenceRequired)?;
        let parent_index = self.validate_ref(parent_ref)?;
        self.validate_ref(child_ref)?;
        let Self { budget, slots, .. } = self;
        let cell = slots[parent_index]
            .cell
            .as_mut()
            .ok_or(HeapError::StaleReference)?;
        match &cell.kind {
            CellKind::Object | CellKind::Function => {}
            CellKind::String(_) => return Err(HeapError::InvalidObjectKind),
        }
        reserve_vec(budget, &mut cell.edges, 1)?;
        cell.edges.push(child_ref);
        Ok(())
    }

    /// Removes one matching managed-reference edge from an object/function.
    pub fn remove_reference(&mut self, parent: Value, child: Value) -> Result<(), HeapError> {
        let parent_ref = parent.gc_ref().ok_or(HeapError::ReferenceRequired)?;
        let child_ref = child.gc_ref().ok_or(HeapError::ReferenceRequired)?;
        let parent_index = self.validate_ref(parent_ref)?;
        self.validate_ref(child_ref)?;
        let cell = self.slots[parent_index]
            .cell
            .as_mut()
            .ok_or(HeapError::StaleReference)?;
        match &cell.kind {
            CellKind::Object | CellKind::Function => {}
            CellKind::String(_) => return Err(HeapError::InvalidObjectKind),
        }
        let position = cell
            .edges
            .iter()
            .position(|reference| *reference == child_ref)
            .ok_or(HeapError::EdgeNotFound)?;
        cell.edges.remove(position);
        Ok(())
    }

    /// Opens a new interpreter frame root provider.
    pub fn enter_frame(&mut self) -> Result<FrameToken, HeapError> {
        let serial = self.next_root_token()?;
        reserve_vec(&mut self.budget, &mut self.frames, 1)?;
        self.frames.push(RootFrame {
            serial,
            roots: Vec::new(),
        });
        Ok(FrameToken {
            heap_id: self.heap_id,
            serial,
            _thread_affinity: PhantomData,
        })
    }

    /// Adds a value to the active interpreter frame's precise root set.
    ///
    /// Primitive values require no tracing root. A managed reference is checked
    /// against this heap before it is published in the frame.
    pub fn frame_root(&mut self, frame: FrameToken, value: Value) -> Result<(), HeapError> {
        self.check_frame(frame)?;
        let Some(reference) = value.gc_ref() else {
            return Ok(());
        };
        self.validate_ref(reference)?;
        let Self { budget, frames, .. } = self;
        let current = frames.last_mut().ok_or(HeapError::InvalidFrame)?;
        reserve_vec(budget, &mut current.roots, 1)?;
        current.roots.push(reference);
        Ok(())
    }

    /// Closes the most recently opened interpreter frame.
    pub fn leave_frame(&mut self, frame: FrameToken) -> Result<(), HeapError> {
        self.check_frame(frame)?;
        let removed = self.frames.pop().ok_or(HeapError::InvalidFrame)?;
        release_vec_capacity(&mut self.budget, &removed.roots)?;
        Ok(())
    }

    /// Opens a temporary root scope.
    pub fn open_scope(&mut self) -> Result<ScopeToken, HeapError> {
        let serial = self.next_root_token()?;
        reserve_vec(&mut self.budget, &mut self.scopes, 1)?;
        self.scopes.push(RootScope {
            serial,
            roots: Vec::new(),
        });
        Ok(ScopeToken {
            heap_id: self.heap_id,
            serial,
            _thread_affinity: PhantomData,
        })
    }

    /// Adds a value to the active temporary scope.
    pub fn scope_root(&mut self, scope: ScopeToken, value: Value) -> Result<(), HeapError> {
        self.check_scope(scope)?;
        let Some(reference) = value.gc_ref() else {
            return Ok(());
        };
        self.validate_ref(reference)?;
        let Self { budget, scopes, .. } = self;
        let current = scopes.last_mut().ok_or(HeapError::InvalidScope)?;
        reserve_vec(budget, &mut current.roots, 1)?;
        current.roots.push(reference);
        Ok(())
    }

    /// Closes the most recently opened temporary root scope.
    pub fn close_scope(&mut self, scope: ScopeToken) -> Result<(), HeapError> {
        self.check_scope(scope)?;
        let removed = self.scopes.pop().ok_or(HeapError::InvalidScope)?;
        release_vec_capacity(&mut self.budget, &removed.roots)?;
        Ok(())
    }

    /// Promotes a Runtime value into an opaque persistent host handle.
    ///
    /// Primitive values can be retained as opaque handles too; managed values
    /// become persistent roots. The handle contains no raw pointer or Value bits.
    pub fn retain_host_value(&mut self, value: Value) -> Result<HostHandle, HeapError> {
        if let Some(reference) = value.gc_ref() {
            self.validate_ref(reference)?;
        }
        let index = if let Some(index) = self.free_host_head {
            let slot = self
                .host_slots
                .get_mut(index as usize)
                .ok_or(HeapError::AccountingInvariant)?;
            self.free_host_head = slot.free_next.take();
            slot.value = Some(value);
            index
        } else {
            if self.host_slots.len() >= u32::MAX as usize {
                return Err(HeapError::HeapCapacityExceeded);
            }
            reserve_vec(&mut self.budget, &mut self.host_slots, 1)?;
            let index = u32::try_from(self.host_slots.len())
                .map_err(|_| HeapError::HeapCapacityExceeded)?;
            self.host_slots.push(HostSlot {
                generation: 1,
                value: Some(value),
                free_next: None,
            });
            index
        };
        let generation = self.host_slots[index as usize].generation;
        Ok(HostHandle {
            heap_id: self.heap_id,
            index,
            generation,
            _thread_affinity: PhantomData,
        })
    }

    /// Resolves an opaque persistent host handle to its Runtime value.
    pub fn resolve_host_handle(&self, handle: HostHandle) -> Result<Value, HeapError> {
        if handle.heap_id != self.heap_id {
            return Err(HeapError::WrongHeap);
        }
        let slot = self
            .host_slots
            .get(handle.index as usize)
            .ok_or(HeapError::StaleHostHandle)?;
        if slot.generation != handle.generation {
            return Err(HeapError::StaleHostHandle);
        }
        let value = slot.value.ok_or(HeapError::StaleHostHandle)?;
        if let Some(reference) = value.gc_ref() {
            self.validate_ref(reference)?;
        }
        Ok(value)
    }

    /// Releases a persistent host handle and removes its managed root.
    pub fn release_host_handle(&mut self, handle: HostHandle) -> Result<(), HeapError> {
        if handle.heap_id != self.heap_id {
            return Err(HeapError::WrongHeap);
        }
        let slot = self
            .host_slots
            .get_mut(handle.index as usize)
            .ok_or(HeapError::StaleHostHandle)?;
        if slot.generation != handle.generation || slot.value.is_none() {
            return Err(HeapError::StaleHostHandle);
        }
        let next_generation = slot
            .generation
            .checked_add(1)
            .ok_or(HeapError::GenerationExhausted)?;
        slot.value = None;
        slot.generation = next_generation;
        if slot.generation != u32::MAX {
            slot.free_next = self.free_host_head;
            self.free_host_head = Some(handle.index);
        }
        Ok(())
    }

    /// Charges externally backed memory against this Runtime's quota.
    ///
    /// The returned token must be released exactly once when the external
    /// allocation is no longer owned by the Runtime. No external pointer is
    /// stored or exposed by this accounting record.
    pub fn charge_external_memory(
        &mut self,
        bytes: usize,
    ) -> Result<ExternalMemoryCharge, HeapError> {
        if self.external_slots.len() >= u32::MAX as usize && self.free_external_head.is_none() {
            return Err(HeapError::HeapCapacityExceeded);
        }
        let index = if let Some(index) = self.free_external_head {
            index
        } else {
            reserve_vec(&mut self.budget, &mut self.external_slots, 1)?;
            u32::try_from(self.external_slots.len()).map_err(|_| HeapError::HeapCapacityExceeded)?
        };

        self.budget.charge_external(bytes)?;
        if self.free_external_head.is_some() {
            let slot = &mut self.external_slots[index as usize];
            self.free_external_head = slot.free_next.take();
            slot.bytes = bytes;
            slot.active = true;
        } else {
            self.external_slots.push(ExternalSlot {
                generation: 1,
                bytes,
                active: true,
                free_next: None,
            });
        }
        let generation = self.external_slots[index as usize].generation;
        Ok(ExternalMemoryCharge {
            heap_id: self.heap_id,
            index,
            generation,
            _thread_affinity: PhantomData,
        })
    }

    /// Releases a previously charged external allocation.
    pub fn release_external_memory(
        &mut self,
        charge: ExternalMemoryCharge,
    ) -> Result<(), HeapError> {
        if charge.heap_id != self.heap_id {
            return Err(HeapError::WrongHeap);
        }
        let slot = self
            .external_slots
            .get_mut(charge.index as usize)
            .ok_or(HeapError::StaleExternalCharge)?;
        if slot.generation != charge.generation || !slot.active {
            return Err(HeapError::StaleExternalCharge);
        }
        let bytes = slot.bytes;
        let next_generation = slot
            .generation
            .checked_add(1)
            .ok_or(HeapError::GenerationExhausted)?;
        slot.active = false;
        slot.bytes = 0;
        slot.generation = next_generation;
        if slot.generation != u32::MAX {
            slot.free_next = self.free_external_head;
            self.free_external_head = Some(charge.index);
        }
        self.budget.release_external(bytes)
    }

    /// Performs a full non-moving tracing mark-sweep collection.
    ///
    /// Mark work is linked through free metadata in the existing slot table, so
    /// collection itself does not need an unbounded scratch allocation that
    /// could violate the heap quota. All roots are read from explicit providers.
    pub fn collect(&mut self) -> Result<CollectionReport, HeapError> {
        for slot in &mut self.slots {
            if let Some(cell) = &mut slot.cell {
                cell.marked = false;
                cell.mark_next = None;
            }
        }
        self.mark_head = None;

        let mut roots_scanned = 0_usize;
        for frame_index in 0..self.frames.len() {
            for root_index in 0..self.frames[frame_index].roots.len() {
                let reference = self.frames[frame_index].roots[root_index];
                self.push_mark(reference)?;
                roots_scanned = roots_scanned.saturating_add(1);
            }
        }
        for scope_index in 0..self.scopes.len() {
            for root_index in 0..self.scopes[scope_index].roots.len() {
                let reference = self.scopes[scope_index].roots[root_index];
                self.push_mark(reference)?;
                roots_scanned = roots_scanned.saturating_add(1);
            }
        }
        for host_index in 0..self.host_slots.len() {
            if let Some(value) = self.host_slots[host_index].value
                && let Some(reference) = value.gc_ref()
            {
                self.push_mark(reference)?;
                roots_scanned = roots_scanned.saturating_add(1);
            }
        }

        while let Some(index) = self.mark_head {
            let (next, edge_count) = {
                let slot = self
                    .slots
                    .get_mut(index as usize)
                    .ok_or(HeapError::AccountingInvariant)?;
                let cell = slot.cell.as_mut().ok_or(HeapError::StaleReference)?;
                (cell.mark_next.take(), cell.edges.len())
            };
            self.mark_head = next;
            for edge_index in 0..edge_count {
                let child = self.slots[index as usize]
                    .cell
                    .as_ref()
                    .ok_or(HeapError::StaleReference)?
                    .edges[edge_index];
                self.push_mark(child)?;
            }
        }

        let mut marked_objects = 0_usize;
        let mut reclaimed_objects = 0_usize;
        let mut reclaimed_bytes = 0_usize;
        for slot_index in 0..self.slots.len() {
            let is_live = self.slots[slot_index].cell.is_some();
            if !is_live {
                continue;
            }
            let marked = self.slots[slot_index]
                .cell
                .as_ref()
                .is_some_and(|cell| cell.marked);
            if marked {
                marked_objects = marked_objects.saturating_add(1);
                if let Some(cell) = &mut self.slots[slot_index].cell {
                    cell.marked = false;
                    cell.mark_next = None;
                }
                continue;
            }

            let cell = self.slots[slot_index]
                .cell
                .take()
                .ok_or(HeapError::AccountingInvariant)?;
            let edge_bytes = cell
                .edges
                .capacity()
                .checked_mul(std::mem::size_of::<GcRef>())
                .ok_or(HeapError::SizeOverflow)?;
            let cell_bytes = std::mem::size_of::<HeapCell>();
            let string_bytes = match &cell.kind {
                CellKind::String(units) => units
                    .capacity()
                    .checked_mul(std::mem::size_of::<u16>())
                    .ok_or(HeapError::SizeOverflow)?,
                CellKind::Object | CellKind::Function => 0,
            };
            reclaimed_bytes = reclaimed_bytes
                .checked_add(cell_bytes)
                .and_then(|total| total.checked_add(edge_bytes))
                .and_then(|total| total.checked_add(string_bytes))
                .ok_or(HeapError::SizeOverflow)?;
            let slot = &mut self.slots[slot_index];
            if let Some(next_generation) = slot.generation.checked_add(1) {
                slot.generation = next_generation;
                if next_generation != u32::MAX {
                    slot.free_next = self.free_slot_head;
                    self.free_slot_head = Some(
                        u32::try_from(slot_index).map_err(|_| HeapError::HeapCapacityExceeded)?,
                    );
                }
            }
            reclaimed_objects = reclaimed_objects.saturating_add(1);
        }

        self.budget.release(reclaimed_bytes)?;
        self.live_objects = self
            .live_objects
            .checked_sub(reclaimed_objects as u64)
            .ok_or(HeapError::AccountingInvariant)?;
        self.reclaimed_objects = self
            .reclaimed_objects
            .checked_add(reclaimed_objects as u64)
            .ok_or(HeapError::SizeOverflow)?;
        self.collections = self
            .collections
            .checked_add(1)
            .ok_or(HeapError::SizeOverflow)?;

        Ok(CollectionReport {
            roots_scanned,
            marked_objects,
            reclaimed_objects,
            reclaimed_bytes,
            used_bytes_after: self.budget.used_bytes,
        })
    }

    /// ECMAScript ToBoolean for the currently represented value categories.
    pub fn to_boolean(&self, value: Value) -> Result<bool, HeapError> {
        match value.repr() {
            ValueRepr::Undefined | ValueRepr::Null => Ok(false),
            ValueRepr::Boolean(boolean) => Ok(boolean),
            ValueRepr::Number(number) => Ok(number != 0.0 && !number.is_nan()),
            ValueRepr::String(reference) => {
                Ok(!self.string_utf16(Value::from_string(reference))?.is_empty())
            }
            ValueRepr::Object(reference) | ValueRepr::Function(reference) => {
                self.validate_ref(reference)?;
                Ok(true)
            }
        }
    }

    /// ECMAScript ToNumber for primitives supported by the M1 Value layer.
    ///
    /// Object-to-primitive conversion is owned by the object model and is
    /// rejected until that layer supplies the corresponding semantics.
    pub fn to_number(&mut self, value: Value) -> Result<f64, HeapError> {
        match value.repr() {
            ValueRepr::Undefined => Ok(f64::NAN),
            ValueRepr::Null => Ok(0.0),
            ValueRepr::Boolean(boolean) => Ok(if boolean { 1.0 } else { 0.0 }),
            ValueRepr::Number(number) => Ok(number),
            ValueRepr::String(reference) => {
                let index = self.validate_ref(reference)?;
                let Self { budget, slots, .. } = self;
                let units = match &slots[index]
                    .cell
                    .as_ref()
                    .ok_or(HeapError::StaleReference)?
                    .kind
                {
                    CellKind::String(units) => units,
                    CellKind::Object | CellKind::Function => {
                        return Err(HeapError::InvalidObjectKind);
                    }
                };
                string_to_number(units, budget)
            }
            ValueRepr::Object(_) | ValueRepr::Function(_) => Err(HeapError::UnsupportedCoercion),
        }
    }

    /// ECMAScript strict equality for the current primitive/reference categories.
    ///
    /// Strings compare by UTF-16 code units; objects/functions compare by their
    /// internal generation-checked identity. NaN is unequal to every Number.
    pub fn strict_equal(&self, left: Value, right: Value) -> Result<bool, HeapError> {
        self.validate_value(left)?;
        self.validate_value(right)?;
        match (left.repr(), right.repr()) {
            (ValueRepr::Undefined, ValueRepr::Undefined) | (ValueRepr::Null, ValueRepr::Null) => {
                Ok(true)
            }
            (ValueRepr::Boolean(a), ValueRepr::Boolean(b)) => Ok(a == b),
            (ValueRepr::Number(a), ValueRepr::Number(b)) => Ok(a == b),
            (ValueRepr::String(a), ValueRepr::String(b)) => {
                Ok(self.string_at(a)? == self.string_at(b)?)
            }
            (ValueRepr::Object(a), ValueRepr::Object(b))
            | (ValueRepr::Function(a), ValueRepr::Function(b)) => Ok(a == b),
            _ => Ok(false),
        }
    }

    /// ECMAScript SameValue equality, including NaN equality and signed-zero distinction.
    pub fn same_value(&self, left: Value, right: Value) -> Result<bool, HeapError> {
        self.validate_value(left)?;
        self.validate_value(right)?;
        match (left.repr(), right.repr()) {
            (ValueRepr::Number(a), ValueRepr::Number(b)) => {
                Ok((a.is_nan() && b.is_nan())
                    || (a == b && (a != 0.0 || a.to_bits() == b.to_bits())))
            }
            (ValueRepr::String(a), ValueRepr::String(b)) => {
                Ok(self.string_at(a)? == self.string_at(b)?)
            }
            (ValueRepr::Object(a), ValueRepr::Object(b))
            | (ValueRepr::Function(a), ValueRepr::Function(b)) => Ok(a == b),
            _ => self.strict_equal(left, right),
        }
    }

    /// ECMAScript SameValueZero equality, treating both zeros and all NaNs as equal.
    pub fn same_value_zero(&self, left: Value, right: Value) -> Result<bool, HeapError> {
        self.validate_value(left)?;
        self.validate_value(right)?;
        match (left.repr(), right.repr()) {
            (ValueRepr::Number(a), ValueRepr::Number(b)) => {
                Ok((a.is_nan() && b.is_nan()) || a == b)
            }
            _ => self.strict_equal(left, right),
        }
    }

    fn try_allocate_string_from_iter<I>(
        &mut self,
        unit_count: usize,
        units: I,
    ) -> Result<Value, HeapError>
    where
        I: IntoIterator<Item = u16>,
    {
        let payload_bytes = unit_count
            .checked_mul(std::mem::size_of::<u16>())
            .ok_or(HeapError::SizeOverflow)?;
        self.check_single_allocation(payload_bytes)?;
        self.ensure_slot_capacity()?;

        let mut owned_units = Vec::new();
        reserve_vec(&mut self.budget, &mut owned_units, unit_count)?;
        owned_units.extend(units);
        if owned_units.len() != unit_count {
            release_vec_capacity(&mut self.budget, &owned_units)?;
            return Err(HeapError::AccountingInvariant);
        }
        let reference = self.publish_cell(CellKind::String(owned_units))?;
        Ok(Value::from_string(reference))
    }

    fn try_allocate_cell(&mut self, kind: CellKind) -> Result<GcRef, HeapError> {
        self.ensure_slot_capacity()?;
        self.publish_cell(kind)
    }

    fn ensure_slot_capacity(&mut self) -> Result<(), HeapError> {
        if self.free_slot_head.is_some() {
            return Ok(());
        }
        if self.slots.len() >= u32::MAX as usize {
            return Err(HeapError::HeapCapacityExceeded);
        }
        reserve_vec(&mut self.budget, &mut self.slots, 1)
    }

    fn publish_cell(&mut self, kind: CellKind) -> Result<GcRef, HeapError> {
        if self.allocations == u64::MAX || self.live_objects == u64::MAX {
            return Err(HeapError::SizeOverflow);
        }

        let reusable_index = self.free_slot_head;
        if let Some(index) = reusable_index {
            let slot = self
                .slots
                .get(index as usize)
                .ok_or(HeapError::AccountingInvariant)?;
            if slot.cell.is_some() {
                return Err(HeapError::AccountingInvariant);
            }
        } else if self.slots.len() >= self.slots.capacity() {
            return Err(HeapError::AccountingInvariant);
        }

        let storage = allocate_cell_storage(&mut self.budget, kind)?;
        let index = if let Some(index) = reusable_index {
            let slot = &mut self.slots[index as usize];
            self.free_slot_head = slot.free_next.take();
            slot.cell = Some(storage);
            index
        } else {
            let index =
                u32::try_from(self.slots.len()).map_err(|_| HeapError::HeapCapacityExceeded)?;
            self.slots.push(HeapSlot {
                generation: 1,
                cell: Some(storage),
                free_next: None,
            });
            index
        };

        let generation = self.slots[index as usize].generation;
        self.allocations = self
            .allocations
            .checked_add(1)
            .ok_or(HeapError::SizeOverflow)?;
        self.live_objects = self
            .live_objects
            .checked_add(1)
            .ok_or(HeapError::SizeOverflow)?;
        Ok(GcRef {
            heap_id: self.heap_id,
            index,
            generation,
        })
    }

    fn push_mark(&mut self, reference: GcRef) -> Result<(), HeapError> {
        let index = self.validate_ref(reference)?;
        let slot = &mut self.slots[index];
        let cell = slot.cell.as_mut().ok_or(HeapError::StaleReference)?;
        if !cell.marked {
            cell.marked = true;
            cell.mark_next = self.mark_head;
            self.mark_head = Some(reference.index);
        }
        Ok(())
    }

    fn validate_ref(&self, reference: GcRef) -> Result<usize, HeapError> {
        if reference.heap_id != self.heap_id {
            return Err(HeapError::WrongHeap);
        }
        let index = reference.index as usize;
        let slot = self.slots.get(index).ok_or(HeapError::StaleReference)?;
        if slot.generation != reference.generation || slot.cell.is_none() {
            return Err(HeapError::StaleReference);
        }
        Ok(index)
    }

    fn validate_value(&self, value: Value) -> Result<(), HeapError> {
        if let Some(reference) = value.gc_ref() {
            self.validate_ref(reference)?;
        }
        Ok(())
    }

    fn string_at(&self, reference: GcRef) -> Result<&[u16], HeapError> {
        let index = self.validate_ref(reference)?;
        match &self.slots[index]
            .cell
            .as_ref()
            .ok_or(HeapError::StaleReference)?
            .kind
        {
            CellKind::String(units) => Ok(units),
            CellKind::Object | CellKind::Function => Err(HeapError::InvalidObjectKind),
        }
    }

    fn check_frame(&self, frame: FrameToken) -> Result<(), HeapError> {
        if frame.heap_id != self.heap_id {
            return Err(HeapError::WrongHeap);
        }
        if self
            .frames
            .last()
            .is_some_and(|current| current.serial == frame.serial)
        {
            Ok(())
        } else if self
            .frames
            .iter()
            .any(|current| current.serial == frame.serial)
        {
            Err(HeapError::RootStackOrder)
        } else {
            Err(HeapError::InvalidFrame)
        }
    }

    fn check_scope(&self, scope: ScopeToken) -> Result<(), HeapError> {
        if scope.heap_id != self.heap_id {
            return Err(HeapError::WrongHeap);
        }
        if self
            .scopes
            .last()
            .is_some_and(|current| current.serial == scope.serial)
        {
            Ok(())
        } else if self
            .scopes
            .iter()
            .any(|current| current.serial == scope.serial)
        {
            Err(HeapError::RootStackOrder)
        } else {
            Err(HeapError::InvalidScope)
        }
    }

    fn next_root_token(&mut self) -> Result<u64, HeapError> {
        let serial = self.next_root_serial;
        self.next_root_serial = self
            .next_root_serial
            .checked_add(1)
            .ok_or(HeapError::RootTokenExhausted)?;
        Ok(serial)
    }

    fn check_single_allocation(&self, bytes: usize) -> Result<(), HeapError> {
        if bytes > self.budget.max_allocation_bytes {
            return Err(HeapError::AllocationTooLarge {
                requested_bytes: bytes,
                maximum_bytes: self.budget.max_allocation_bytes,
            });
        }
        Ok(())
    }
}

fn allocate_cell_storage(
    budget: &mut HeapBudget,
    kind: CellKind,
) -> Result<CellStorage, HeapError> {
    let mut storage = Vec::new();
    if let Err(error) = reserve_vec(budget, &mut storage, 1) {
        if let CellKind::String(units) = &kind {
            release_vec_capacity(budget, units)?;
        }
        return Err(error);
    }

    storage.push(HeapCell {
        kind,
        edges: Vec::new(),
        marked: false,
        mark_next: None,
    });
    let previous_capacity = storage.capacity();
    let boxed = CellStorage(storage.into_boxed_slice());
    let retained_bytes = std::mem::size_of::<HeapCell>();
    let allocated_bytes = previous_capacity
        .checked_mul(std::mem::size_of::<HeapCell>())
        .ok_or(HeapError::SizeOverflow)?;
    if allocated_bytes > retained_bytes {
        budget.release(allocated_bytes - retained_bytes)?;
    }
    Ok(boxed)
}

fn reserve_vec<T>(
    budget: &mut HeapBudget,
    vector: &mut Vec<T>,
    additional: usize,
) -> Result<(), HeapError> {
    if additional == 0 {
        return Ok(());
    }
    let required = vector
        .len()
        .checked_add(additional)
        .ok_or(HeapError::SizeOverflow)?;
    if required <= vector.capacity() {
        return Ok(());
    }
    let current_capacity = vector.capacity();
    let element_size = std::mem::size_of::<T>();
    let minimum_backing_bytes = required
        .checked_mul(element_size)
        .ok_or(HeapError::SizeOverflow)?;
    if minimum_backing_bytes > budget.max_allocation_bytes {
        return Err(HeapError::AllocationTooLarge {
            requested_bytes: minimum_backing_bytes,
            maximum_bytes: budget.max_allocation_bytes,
        });
    }

    let minimum_growth = required
        .checked_sub(current_capacity)
        .ok_or(HeapError::SizeOverflow)?;
    let minimum_growth_bytes = minimum_growth
        .checked_mul(element_size)
        .ok_or(HeapError::SizeOverflow)?;
    let available_bytes = budget.quota_bytes.saturating_sub(budget.used_bytes);
    if minimum_growth_bytes > available_bytes {
        return Err(HeapError::QuotaExceeded {
            requested_bytes: minimum_growth_bytes,
            available_bytes,
        });
    }

    let doubled = current_capacity.checked_mul(2).unwrap_or(required);
    let preferred_capacity = required.max(if current_capacity == 0 { 1 } else { doubled });
    let preferred_growth = preferred_capacity - current_capacity;
    let preferred_backing_bytes = preferred_capacity.checked_mul(element_size);
    let preferred_growth_bytes = preferred_growth.checked_mul(element_size);
    let target_capacity = match (preferred_backing_bytes, preferred_growth_bytes) {
        (Some(backing_bytes), Some(growth_bytes))
            if backing_bytes <= budget.max_allocation_bytes && growth_bytes <= available_bytes =>
        {
            preferred_capacity
        }
        _ => required,
    };
    let growth = target_capacity
        .checked_sub(current_capacity)
        .ok_or(HeapError::SizeOverflow)?;
    let requested_bytes = growth
        .checked_mul(element_size)
        .ok_or(HeapError::SizeOverflow)?;
    budget.charge(requested_bytes)?;
    let reserve_count = target_capacity
        .checked_sub(vector.len())
        .ok_or(HeapError::SizeOverflow)?;
    if vector.try_reserve_exact(reserve_count).is_err() {
        budget.release(requested_bytes)?;
        return Err(HeapError::AllocationFailed);
    }
    let actual_growth = vector
        .capacity()
        .checked_sub(current_capacity)
        .ok_or(HeapError::AccountingInvariant)?;
    let actual_bytes = actual_growth
        .checked_mul(std::mem::size_of::<T>())
        .ok_or(HeapError::SizeOverflow)?;
    if actual_bytes > requested_bytes {
        budget.charge(actual_bytes - requested_bytes)?;
    } else if requested_bytes > actual_bytes {
        budget.release(requested_bytes - actual_bytes)?;
    }
    Ok(())
}

fn release_vec_capacity<T>(budget: &mut HeapBudget, vector: &Vec<T>) -> Result<(), HeapError> {
    let bytes = vector
        .capacity()
        .checked_mul(std::mem::size_of::<T>())
        .ok_or(HeapError::SizeOverflow)?;
    budget.release(bytes)
}

fn string_to_number(units: &[u16], budget: &mut HeapBudget) -> Result<f64, HeapError> {
    let Some(start) = units
        .iter()
        .position(|unit| !is_ecmascript_whitespace(*unit))
    else {
        return Ok(0.0);
    };
    let end = units
        .iter()
        .rposition(|unit| !is_ecmascript_whitespace(*unit))
        .ok_or(HeapError::AccountingInvariant)?;
    let trimmed = &units[start..=end];
    if trimmed.iter().any(|unit| *unit > 0x7F) {
        return Ok(f64::NAN);
    }

    // Numeric and Infinity tokens are ASCII. Charge the short-lived conversion
    // buffer to the Runtime instead of allocating an untracked Rust String.
    let mut bytes = Vec::new();
    reserve_vec(budget, &mut bytes, trimmed.len())?;
    bytes.extend(trimmed.iter().map(|unit| *unit as u8));
    let result = std::str::from_utf8(&bytes).map_or(f64::NAN, parse_ascii_string_number);
    release_vec_capacity(budget, &bytes)?;
    Ok(result)
}

fn parse_ascii_string_number(text: &str) -> f64 {
    match text {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    if text.starts_with("0x") || text.starts_with("0X") {
        return parse_radix_number(&text[2..], 16).unwrap_or(f64::NAN);
    }
    if text.starts_with("0b") || text.starts_with("0B") {
        return parse_radix_number(&text[2..], 2).unwrap_or(f64::NAN);
    }
    if text.starts_with("0o") || text.starts_with("0O") {
        return parse_radix_number(&text[2..], 8).unwrap_or(f64::NAN);
    }
    if text.eq_ignore_ascii_case("inf")
        || text.eq_ignore_ascii_case("+inf")
        || text.eq_ignore_ascii_case("-inf")
        || text.eq_ignore_ascii_case("infinity")
        || text.eq_ignore_ascii_case("+infinity")
        || text.eq_ignore_ascii_case("-infinity")
    {
        return f64::NAN;
    }
    text.parse::<f64>().unwrap_or(f64::NAN)
}

fn parse_radix_number(digits: &str, radix: u32) -> Option<f64> {
    let bits_per_digit = match radix {
        2 => 1,
        8 => 3,
        16 => 4,
        _ => return None,
    };
    if digits.is_empty() {
        return None;
    }

    // Binary/octal/hexadecimal integers are powers of two. Keep the leading
    // 53 significand bits plus guard/sticky bits to round exactly to binary64.
    let mut started = false;
    let mut bit_length = 0_usize;
    let mut significand = 0_u64;
    let mut guard_bit = false;
    let mut sticky_bit = false;

    for character in digits.chars() {
        let digit = character.to_digit(radix)?;
        for shift in (0..bits_per_digit).rev() {
            let bit = (digit >> shift) & 1 != 0;
            if !started {
                if !bit {
                    continue;
                }
                started = true;
            }

            match bit_length {
                0..=52 => significand = (significand << 1) | u64::from(bit),
                53 => guard_bit = bit,
                _ => sticky_bit |= bit,
            }
            bit_length = bit_length.checked_add(1)?;
        }
    }

    if !started {
        return Some(0.0);
    }
    if bit_length <= 53 {
        return Some(significand as f64);
    }

    if guard_bit && (sticky_bit || significand & 1 != 0) {
        significand += 1;
    }
    let mut binary_exponent = bit_length.checked_sub(53)?;
    if significand == (1_u64 << 53) {
        significand >>= 1;
        binary_exponent = binary_exponent.checked_add(1)?;
    }
    if binary_exponent > 971 {
        return Some(f64::INFINITY);
    }

    Some((significand as f64) * 2.0_f64.powi(binary_exponent as i32))
}

fn is_ecmascript_whitespace(unit: u16) -> bool {
    matches!(
        unit,
        0x0009..=0x000D
            | 0x0020
            | 0x00A0
            | 0x1680
            | 0x2000..=0x200A
            | 0x2028
            | 0x2029
            | 0x202F
            | 0x205F
            | 0x3000
            | 0xFEFF
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::ValueType;

    fn heap(quota: usize, maximum: usize) -> Heap {
        Heap::new(HeapConfig::new(quota, maximum)).expect("new heap")
    }

    #[test]
    fn utf16_strings_preserve_code_units_and_compare_by_content() {
        let mut heap = heap(4096, 2048);
        let first = heap
            .allocate_string_utf16(&[0x0061, 0xD800, 0x0062])
            .unwrap();
        let second = heap
            .allocate_string_utf16(&[0x0061, 0xD800, 0x0062])
            .unwrap();
        let different = heap
            .allocate_string_utf16(&[0x0061, 0xD801, 0x0062])
            .unwrap();
        assert_eq!(heap.string_utf16(first).unwrap(), &[0x0061, 0xD800, 0x0062]);
        assert!(heap.strict_equal(first, second).unwrap());
        assert!(!heap.strict_equal(first, different).unwrap());
    }

    #[test]
    fn ecmascript_number_equality_handles_nan_infinities_and_signed_zero() {
        let heap = heap(1024, 512);
        let nan_a = Value::number(f64::NAN);
        let nan_b = Value::number(f64::from_bits(0x7ff8_0000_0000_0001));
        assert!(!heap.strict_equal(nan_a, nan_b).unwrap());
        assert!(heap.same_value(nan_a, nan_b).unwrap());
        assert!(heap.same_value_zero(nan_a, nan_b).unwrap());
        assert!(
            heap.strict_equal(Value::number(0.0), Value::number(-0.0))
                .unwrap()
        );
        assert!(
            !heap
                .same_value(Value::number(0.0), Value::number(-0.0))
                .unwrap()
        );
        assert!(
            heap.same_value_zero(Value::number(0.0), Value::number(-0.0))
                .unwrap()
        );
        assert!(
            heap.strict_equal(Value::number(f64::INFINITY), Value::number(f64::INFINITY))
                .unwrap()
        );
        assert!(
            !heap
                .strict_equal(
                    Value::number(f64::INFINITY),
                    Value::number(f64::NEG_INFINITY)
                )
                .unwrap()
        );
        assert!(
            !heap
                .strict_equal(Value::undefined(), Value::null())
                .unwrap()
        );
    }

    #[test]
    fn strings_compare_by_code_units_and_objects_compare_by_identity() {
        let mut heap = heap(4096, 2048);
        let first_string = heap.allocate_string("same").unwrap();
        let second_string = heap.allocate_string("same").unwrap();
        let first_object = heap.allocate_object().unwrap();
        let second_object = heap.allocate_object().unwrap();
        let first_function = heap.allocate_function().unwrap();
        assert!(heap.strict_equal(first_string, second_string).unwrap());
        assert!(heap.strict_equal(first_object, first_object).unwrap());
        assert!(!heap.strict_equal(first_object, second_object).unwrap());
        assert!(heap.strict_equal(first_function, first_function).unwrap());
        assert!(!heap.strict_equal(first_object, first_function).unwrap());
    }

    #[test]
    fn primitive_conversions_follow_m1_to_boolean_and_to_number_rules() {
        let mut heap = heap(4096, 2048);
        let empty = heap.allocate_string("").unwrap();
        let text = heap.allocate_string(" 0x10 \u{FEFF}").unwrap();
        let invalid = heap.allocate_string("not a number").unwrap();
        assert!(!heap.to_boolean(Value::undefined()).unwrap());
        assert!(!heap.to_boolean(Value::null()).unwrap());
        assert!(!heap.to_boolean(Value::boolean(false)).unwrap());
        assert!(!heap.to_boolean(Value::number(-0.0)).unwrap());
        assert!(!heap.to_boolean(Value::number(f64::NAN)).unwrap());
        assert!(!heap.to_boolean(empty).unwrap());
        assert!(heap.to_boolean(text).unwrap());
        let object = heap.allocate_object().unwrap();
        assert!(heap.to_boolean(object).unwrap());
        assert!(heap.to_number(Value::undefined()).unwrap().is_nan());
        assert_eq!(heap.to_number(Value::null()).unwrap(), 0.0);
        assert_eq!(heap.to_number(Value::boolean(true)).unwrap(), 1.0);
        assert_eq!(heap.to_number(text).unwrap(), 16.0);
        assert_eq!(heap.to_number(empty).unwrap(), 0.0);
        assert!(heap.to_number(invalid).unwrap().is_nan());
        let object = heap.allocate_object().unwrap();
        assert_eq!(heap.to_number(object), Err(HeapError::UnsupportedCoercion));
    }

    #[test]
    fn string_to_number_rejects_noncanonical_infinity_and_rounds_radix_values() {
        let mut runtime_heap = heap(4096, 2048);
        let lowercase_infinity = runtime_heap.allocate_string("infinity").unwrap();
        let mixed_infinity = runtime_heap.allocate_string("+InFiNiTy").unwrap();
        let halfway_down = runtime_heap.allocate_string("0x20000000000001").unwrap();
        let halfway_up = runtime_heap.allocate_string("0x20000000000003").unwrap();

        assert!(runtime_heap.to_number(lowercase_infinity).unwrap().is_nan());
        assert!(runtime_heap.to_number(mixed_infinity).unwrap().is_nan());
        assert_eq!(
            runtime_heap.to_number(halfway_down).unwrap(),
            9_007_199_254_740_992.0
        );
        assert_eq!(
            runtime_heap.to_number(halfway_up).unwrap(),
            9_007_199_254_740_996.0
        );
    }

    #[test]
    fn number_conversion_scratch_is_quota_accounted_and_released() {
        let slot_bytes = std::mem::size_of::<HeapSlot>();
        let cell_bytes = std::mem::size_of::<HeapCell>();
        let quota = slot_bytes + cell_bytes + 6;
        let mut runtime_heap = heap(quota, quota);
        let value = runtime_heap.allocate_string("123").unwrap();
        assert_eq!(runtime_heap.stats().used_bytes, quota);
        assert!(matches!(
            runtime_heap.to_number(value),
            Err(HeapError::QuotaExceeded {
                requested_bytes: 3,
                available_bytes: 0
            })
        ));
        assert_eq!(runtime_heap.stats().used_bytes, quota);

        let mut roomier_heap = heap(slot_bytes + cell_bytes + 9, slot_bytes + cell_bytes + 9);
        let numeric = roomier_heap.allocate_string("123").unwrap();
        let before = roomier_heap.stats().used_bytes;
        assert_eq!(roomier_heap.to_number(numeric).unwrap(), 123.0);
        assert_eq!(roomier_heap.stats().used_bytes, before);
    }

    #[test]
    fn managed_cell_addresses_remain_stable_when_slot_directory_grows() {
        let mut runtime_heap = heap(64 * 1024, 16 * 1024);
        let anchor = runtime_heap.allocate_object().unwrap();
        let reference = anchor.gc_ref().unwrap();
        let anchor_index = runtime_heap.validate_ref(reference).unwrap();
        let address_before = runtime_heap.slots[anchor_index]
            .cell
            .as_ref()
            .map(|cell| std::ptr::from_ref(&**cell) as usize)
            .unwrap();

        for _ in 0..128 {
            let _ = runtime_heap.allocate_object().unwrap();
        }

        let address_after = runtime_heap.slots[anchor_index]
            .cell
            .as_ref()
            .map(|cell| std::ptr::from_ref(&**cell) as usize)
            .unwrap();
        assert_ne!(runtime_heap.slots.capacity(), 1);
        assert_eq!(address_before, address_after);
        assert_eq!(runtime_heap.validate_ref(reference), Ok(anchor_index));
    }

    #[test]
    fn failed_cell_header_allocation_releases_preallocated_string_payload() {
        let slot_bytes = std::mem::size_of::<HeapSlot>();
        let cell_bytes = std::mem::size_of::<HeapCell>();
        let payload_bytes = 8;
        let quota = slot_bytes + cell_bytes + payload_bytes - 1;
        let mut runtime_heap = heap(quota, quota);
        let _unrooted_object = runtime_heap.allocate_object().unwrap();
        assert_eq!(runtime_heap.stats().used_bytes, slot_bytes + cell_bytes);

        assert!(matches!(
            runtime_heap.allocate_string_utf16(&[0x0061; 4]),
            Err(HeapError::QuotaExceeded { .. })
        ));
        assert_eq!(runtime_heap.stats().used_bytes, slot_bytes);
        assert_eq!(runtime_heap.stats().live_objects, 0);
        assert_eq!(runtime_heap.stats().reclaimed_objects, 1);
    }

    #[test]
    fn mark_sweep_reclaims_unreachable_cycles_and_keeps_live_graph_non_moving() {
        let mut heap = heap(16 * 1024, 4096);
        let root = heap.allocate_object().unwrap();
        let child = heap.allocate_object().unwrap();
        let orphan_a = heap.allocate_object().unwrap();
        let orphan_b = heap.allocate_object().unwrap();
        heap.add_reference(root, child).unwrap();
        heap.add_reference(child, root).unwrap();
        heap.add_reference(orphan_a, orphan_b).unwrap();
        heap.add_reference(orphan_b, orphan_a).unwrap();
        let frame = heap.enter_frame().unwrap();
        heap.frame_root(frame, root).unwrap();

        let report = heap.collect().unwrap();
        assert_eq!(report.marked_objects, 2);
        assert_eq!(report.reclaimed_objects, 2);
        assert!(heap.strict_equal(root, root).unwrap());
        assert_eq!(heap.stats().live_objects, 2);
        heap.leave_frame(frame).unwrap();
        assert_eq!(heap.collect().unwrap().reclaimed_objects, 2);
        assert_eq!(heap.stats().live_objects, 0);
    }

    #[test]
    fn interpreter_frame_temporary_scope_and_persistent_handle_are_all_roots() {
        let mut heap = heap(16 * 1024, 4096);
        let frame_value = heap.allocate_object().unwrap();
        let scope_value = heap.allocate_object().unwrap();
        let host_value = heap.allocate_object().unwrap();
        let frame = heap.enter_frame().unwrap();
        heap.frame_root(frame, frame_value).unwrap();
        let scope = heap.open_scope().unwrap();
        heap.scope_root(scope, scope_value).unwrap();
        let host = heap.retain_host_value(host_value).unwrap();

        let roots = heap.root_snapshot();
        assert_eq!(roots.interpreter_frames, 1);
        assert_eq!(roots.interpreter_frame_roots, 1);
        assert_eq!(roots.temporary_scopes, 1);
        assert_eq!(roots.temporary_scope_roots, 1);
        assert_eq!(roots.persistent_host_roots, 1);
        assert_eq!(heap.collect().unwrap().marked_objects, 3);

        heap.close_scope(scope).unwrap();
        heap.release_host_handle(host).unwrap();
        heap.leave_frame(frame).unwrap();
        assert_eq!(heap.collect().unwrap().reclaimed_objects, 3);
    }

    #[test]
    fn root_frames_and_scopes_enforce_strict_lifo_and_reject_wrong_heap() {
        let mut first = heap(8192, 2048);
        let mut second = heap(8192, 2048);
        let outer_scope = first.open_scope().unwrap();
        let inner_scope = first.open_scope().unwrap();
        assert_eq!(
            first.close_scope(outer_scope),
            Err(HeapError::RootStackOrder)
        );
        first.close_scope(inner_scope).unwrap();
        first.close_scope(outer_scope).unwrap();

        let outer_frame = first.enter_frame().unwrap();
        let inner_frame = first.enter_frame().unwrap();
        assert_eq!(
            first.leave_frame(outer_frame),
            Err(HeapError::RootStackOrder)
        );
        first.leave_frame(inner_frame).unwrap();
        first.leave_frame(outer_frame).unwrap();

        let value = second.allocate_object().unwrap();
        let frame = first.enter_frame().unwrap();
        assert_eq!(first.frame_root(frame, value), Err(HeapError::WrongHeap));
        first.leave_frame(frame).unwrap();
    }

    #[test]
    fn stale_host_handles_and_heap_slot_generations_are_rejected_after_reuse() {
        let mut heap = heap(8192, 2048);
        let original = heap.allocate_object().unwrap();
        let original_ref = original.gc_ref().unwrap();
        let old_handle = heap.retain_host_value(original).unwrap();
        heap.release_host_handle(old_handle).unwrap();
        assert_eq!(
            heap.resolve_host_handle(old_handle).unwrap_err(),
            HeapError::StaleHostHandle
        );
        heap.collect().unwrap();
        let replacement = heap.allocate_object().unwrap();
        let replacement_ref = replacement.gc_ref().unwrap();
        assert_eq!(original_ref.index, replacement_ref.index);
        assert_ne!(original_ref.generation, replacement_ref.generation);
        assert_eq!(
            heap.strict_equal(original, replacement),
            Err(HeapError::StaleReference)
        );
        let replacement_handle = heap.retain_host_value(replacement).unwrap();
        assert_ne!(old_handle, replacement_handle);
        assert_eq!(
            heap.resolve_host_handle(replacement_handle)
                .unwrap()
                .value_type(),
            ValueType::Object
        );
        assert_eq!(
            heap.resolve_host_handle(old_handle).unwrap_err(),
            HeapError::StaleHostHandle
        );
    }

    #[test]
    fn invalid_or_stale_roots_are_not_published() {
        let mut runtime_heap = heap(8192, 2048);
        let mut other_heap = heap(8192, 2048);
        let foreign = other_heap.allocate_object().unwrap();
        let frame = runtime_heap.enter_frame().unwrap();
        assert_eq!(
            runtime_heap.frame_root(frame, foreign),
            Err(HeapError::WrongHeap)
        );
        assert_eq!(runtime_heap.root_snapshot().interpreter_frame_roots, 0);
        runtime_heap.leave_frame(frame).unwrap();

        let stale = runtime_heap.allocate_object().unwrap();
        runtime_heap.collect().unwrap();
        let scope = runtime_heap.open_scope().unwrap();
        assert_eq!(
            runtime_heap.scope_root(scope, stale),
            Err(HeapError::StaleReference)
        );
        assert_eq!(runtime_heap.root_snapshot().temporary_scope_roots, 0);
        runtime_heap.close_scope(scope).unwrap();
    }

    #[test]
    fn vector_growth_uses_minimum_capacity_when_quota_or_single_limit_is_tight() {
        let slot_bytes = std::mem::size_of::<HeapSlot>();
        let cell_bytes = std::mem::size_of::<HeapCell>();
        let limit = slot_bytes * 3 + cell_bytes * 3;
        let mut runtime_heap = heap(limit, slot_bytes * 3);

        let _first = runtime_heap.allocate_object().unwrap();
        let _second = runtime_heap.allocate_object().unwrap();
        let _third = runtime_heap.allocate_object().unwrap();

        assert_eq!(runtime_heap.slots.capacity(), 3);
        assert_eq!(runtime_heap.stats().used_bytes, limit);
        assert_eq!(runtime_heap.stats().live_objects, 3);
        assert_eq!(runtime_heap.stats().collections, 0);
    }

    #[test]
    fn quota_pressure_collects_unrooted_objects_and_retries_allocation() {
        let mut heap = heap(1200, 1024);
        let first = heap.allocate_string_utf16(&[0x0061; 400]).unwrap();
        assert_eq!(heap.stats().live_objects, 1);
        let second = heap.allocate_string_utf16(&[0x0062; 400]).unwrap();
        assert_eq!(heap.stats().collections, 1);
        assert_eq!(heap.stats().reclaimed_objects, 1);
        assert_eq!(heap.stats().live_objects, 1);
        assert!(heap.string_utf16(first).is_err());
        assert_eq!(heap.string_utf16(second).unwrap().len(), 400);
    }

    #[test]
    fn oversized_allocations_fail_before_publishing_a_cell() {
        let mut runtime_heap = heap(4096, 128);
        let error = runtime_heap
            .allocate_string_utf16(&[0x0061; 100])
            .unwrap_err();
        assert!(matches!(error, HeapError::AllocationTooLarge { .. }));
        let stats = runtime_heap.stats();
        assert_eq!(stats.live_objects, 0);
        assert_eq!(stats.allocations, 0);
        assert_eq!(stats.used_bytes, 0);

        let mut tiny_heap = heap(4096, 1);
        assert!(matches!(
            tiny_heap.allocate_object(),
            Err(HeapError::AllocationTooLarge { .. })
        ));
        assert_eq!(tiny_heap.stats().used_bytes, 0);
        assert_eq!(tiny_heap.stats().live_objects, 0);
    }

    #[test]
    fn external_memory_is_quota_charged_and_generation_checked() {
        let mut heap = heap(900, 512);
        let charge = heap.charge_external_memory(400).unwrap();
        assert_eq!(heap.stats().external_memory_bytes, 400);
        assert!(matches!(
            heap.charge_external_memory(500),
            Err(HeapError::QuotaExceeded { .. })
        ));
        heap.release_external_memory(charge).unwrap();
        assert_eq!(heap.stats().external_memory_bytes, 0);
        assert_eq!(
            heap.release_external_memory(charge),
            Err(HeapError::StaleExternalCharge)
        );

        let replacement = heap.charge_external_memory(128).unwrap();
        assert_ne!(charge, replacement);
        assert_eq!(heap.stats().external_memory_bytes, 128);
        assert_eq!(
            heap.release_external_memory(charge),
            Err(HeapError::StaleExternalCharge)
        );
        assert_eq!(heap.stats().external_memory_bytes, 128);
        heap.release_external_memory(replacement).unwrap();
        assert_eq!(heap.stats().external_memory_bytes, 0);
    }

    #[test]
    fn primitive_host_handles_are_persistent_but_not_gc_roots() {
        let mut runtime_heap = heap(4096, 1024);
        let handle = runtime_heap.retain_host_value(Value::number(42.0)).unwrap();

        assert_eq!(runtime_heap.root_snapshot().persistent_host_roots, 0);
        assert_eq!(
            runtime_heap
                .resolve_host_handle(handle)
                .unwrap()
                .as_number(),
            Some(42.0)
        );
        assert_eq!(runtime_heap.collect().unwrap().marked_objects, 0);
        runtime_heap.release_host_handle(handle).unwrap();
        assert_eq!(
            runtime_heap.resolve_host_handle(handle).unwrap_err(),
            HeapError::StaleHostHandle
        );
    }

    #[test]
    fn handles_and_external_charges_cannot_cross_runtime_boundaries() {
        let mut first = heap(4096, 1024);
        let mut second = heap(4096, 1024);
        let value = first.allocate_object().unwrap();
        let handle = first.retain_host_value(value).unwrap();
        let charge = first.charge_external_memory(32).unwrap();
        assert_eq!(
            second.resolve_host_handle(handle).unwrap_err(),
            HeapError::WrongHeap
        );
        assert_eq!(
            second.release_external_memory(charge),
            Err(HeapError::WrongHeap)
        );
    }

    #[test]
    fn allocation_recovers_after_releasing_external_memory() {
        let mut heap = heap(512, 512);
        let charge = heap.charge_external_memory(400).unwrap();
        assert!(matches!(
            heap.allocate_string_utf16(&[0x0061; 100]),
            Err(HeapError::QuotaExceeded { .. })
        ));
        heap.release_external_memory(charge).unwrap();
        let value = heap.allocate_string_utf16(&[0x0061; 100]).unwrap();
        assert_eq!(heap.string_utf16(value).unwrap().len(), 100);
    }

    #[test]
    fn root_scopes_preserve_values_through_nested_callback_style_collection() {
        let mut heap = heap(8192, 2048);
        let callback_value = heap.allocate_object().unwrap();
        let persistent = heap.retain_host_value(callback_value).unwrap();
        let outer = heap.open_scope().unwrap();
        heap.scope_root(outer, callback_value).unwrap();
        let inner = heap.open_scope().unwrap();
        heap.scope_root(inner, callback_value).unwrap();
        heap.collect().unwrap();
        assert_eq!(
            heap.resolve_host_handle(persistent).unwrap().value_type(),
            ValueType::Object
        );
        heap.close_scope(inner).unwrap();
        heap.collect().unwrap();
        assert_eq!(
            heap.resolve_host_handle(persistent).unwrap().value_type(),
            ValueType::Object
        );
        heap.close_scope(outer).unwrap();
        heap.release_host_handle(persistent).unwrap();
        assert_eq!(heap.collect().unwrap().reclaimed_objects, 1);
    }
}
