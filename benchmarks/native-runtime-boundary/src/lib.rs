#![forbid(unsafe_code)]

//! OGM-27 M0 proof model. This is deliberately not a production JS Runtime.

use std::collections::BTreeMap;
use std::fmt;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GcRef {
    index: usize,
    generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsValue {
    Undefined,
    Boolean(bool),
    Number(u64),
    Managed(GcRef),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeapError {
    StaleReference,
}

impl fmt::Display for HeapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for HeapError {}

#[derive(Clone, Debug)]
struct ObjectSlot {
    generation: u64,
    alive: bool,
    edges: Vec<GcRef>,
}

/// Small non-moving mark/sweep heap used only to prove root enumeration.
#[derive(Default)]
pub struct GcHeap {
    slots: Vec<ObjectSlot>,
    free: Vec<usize>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CollectionStats {
    pub roots_scanned: usize,
    pub objects_marked: usize,
    pub objects_reclaimed: usize,
    pub objects_live: usize,
}

impl GcHeap {
    pub fn allocate(&mut self) -> GcRef {
        let index = if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index];
            debug_assert!(!slot.alive);
            slot.alive = true;
            slot.edges.clear();
            index
        } else {
            let index = self.slots.len();
            self.slots.push(ObjectSlot {
                generation: 1,
                alive: true,
                edges: Vec::new(),
            });
            index
        };

        GcRef {
            index,
            generation: self.slots[index].generation,
        }
    }

    pub fn add_edge(&mut self, from: GcRef, to: GcRef) -> Result<(), HeapError> {
        self.validate(from)?;
        self.validate(to)?;
        self.slots[from.index].edges.push(to);
        Ok(())
    }

    pub fn is_alive(&self, reference: GcRef) -> bool {
        self.validate(reference).is_ok()
    }

    pub fn live_count(&self) -> usize {
        self.slots.iter().filter(|slot| slot.alive).count()
    }

    pub fn collect(
        &mut self,
        roots: impl IntoIterator<Item = GcRef>,
    ) -> Result<CollectionStats, HeapError> {
        let mut marked = vec![false; self.slots.len()];
        let mut pending = Vec::new();
        let mut roots_scanned = 0;

        for root in roots {
            self.validate(root)?;
            roots_scanned += 1;
            if !marked[root.index] {
                marked[root.index] = true;
                pending.push(root);
            }
        }

        while let Some(reference) = pending.pop() {
            for target in self.slots[reference.index].edges.iter().copied() {
                self.validate(target)?;
                if !marked[target.index] {
                    marked[target.index] = true;
                    pending.push(target);
                }
            }
        }

        let objects_marked = marked.iter().filter(|is_marked| **is_marked).count();
        let mut objects_reclaimed = 0;
        for (index, is_marked) in marked.into_iter().enumerate() {
            let slot = &mut self.slots[index];
            if slot.alive && !is_marked {
                slot.alive = false;
                slot.edges.clear();
                objects_reclaimed += 1;
                if let Some(next_generation) = slot.generation.checked_add(1) {
                    slot.generation = next_generation;
                    self.free.push(index);
                }
            }
        }

        Ok(CollectionStats {
            roots_scanned,
            objects_marked,
            objects_reclaimed,
            objects_live: self.live_count(),
        })
    }

    fn validate(&self, reference: GcRef) -> Result<(), HeapError> {
        let slot = self
            .slots
            .get(reference.index)
            .ok_or(HeapError::StaleReference)?;
        if !slot.alive || slot.generation != reference.generation {
            return Err(HeapError::StaleReference);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SafepointId(pub u32);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootMap {
    pub safepoint: SafepointId,
    /// Slot indexes containing live GC-managed references at this safepoint.
    pub slots: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct FrameDescriptor {
    name: String,
    slot_count: usize,
    entry_safepoint: SafepointId,
    root_maps: BTreeMap<SafepointId, Vec<usize>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    DuplicateSafepoint,
    DuplicateRootSlot,
    RootSlotOutOfBounds,
    MissingEntryRootMap,
    SlotCountMismatch,
    UnknownSafepoint,
    LiveRootMapMismatch,
    NonManagedRootSlot,
    FrameStackOrder,
    TokenSpaceExhausted,
    InvalidFrame,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for FrameError {}

impl FrameDescriptor {
    pub fn new(
        name: impl Into<String>,
        slot_count: usize,
        entry_safepoint: SafepointId,
        root_maps: Vec<RootMap>,
    ) -> Result<Self, FrameError> {
        let mut maps = BTreeMap::new();

        for mut root_map in root_maps {
            root_map.slots.sort_unstable();
            if root_map.slots.windows(2).any(|pair| pair[0] == pair[1]) {
                return Err(FrameError::DuplicateRootSlot);
            }
            if root_map.slots.iter().any(|slot| *slot >= slot_count) {
                return Err(FrameError::RootSlotOutOfBounds);
            }
            if maps.insert(root_map.safepoint, root_map.slots).is_some() {
                return Err(FrameError::DuplicateSafepoint);
            }
        }

        if !maps.contains_key(&entry_safepoint) {
            return Err(FrameError::MissingEntryRootMap);
        }

        Ok(Self {
            name: name.into(),
            slot_count,
            entry_safepoint,
            root_maps: maps,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn slot_count(&self) -> usize {
        self.slot_count
    }
}

/// Precise shadow-frame prototype. A compiler supplies a root map for each
/// safepoint; no native stack scanning or raw object pointers are involved.
pub struct NativeFrame {
    descriptor: FrameDescriptor,
    slots: Vec<JsValue>,
    current_safepoint: SafepointId,
}

impl NativeFrame {
    pub fn new(descriptor: FrameDescriptor, slots: Vec<JsValue>) -> Result<Self, FrameError> {
        if slots.len() != descriptor.slot_count {
            return Err(FrameError::SlotCountMismatch);
        }

        let frame = Self {
            current_safepoint: descriptor.entry_safepoint,
            descriptor,
            slots,
        };
        frame.validate_active_map()?;
        Ok(frame)
    }

    pub fn set_safepoint(
        &mut self,
        safepoint: SafepointId,
        independently_live_reference_slots: &[usize],
    ) -> Result<(), FrameError> {
        let map = self
            .descriptor
            .root_maps
            .get(&safepoint)
            .ok_or(FrameError::UnknownSafepoint)?;

        let mut expected = independently_live_reference_slots.to_vec();
        expected.sort_unstable();
        if expected.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(FrameError::DuplicateRootSlot);
        }
        if map != &expected {
            return Err(FrameError::LiveRootMapMismatch);
        }
        for slot in map {
            if !matches!(self.slots[*slot], JsValue::Managed(_)) {
                return Err(FrameError::NonManagedRootSlot);
            }
        }

        self.current_safepoint = safepoint;
        Ok(())
    }

    pub fn set_slot(&mut self, slot: usize, value: JsValue) -> Result<(), FrameError> {
        let target = self
            .slots
            .get_mut(slot)
            .ok_or(FrameError::RootSlotOutOfBounds)?;
        *target = value;
        Ok(())
    }

    fn validate_active_map(&self) -> Result<(), FrameError> {
        let map = self
            .descriptor
            .root_maps
            .get(&self.current_safepoint)
            .ok_or(FrameError::UnknownSafepoint)?;
        for slot in map {
            if !matches!(self.slots[*slot], JsValue::Managed(_)) {
                return Err(FrameError::NonManagedRootSlot);
            }
        }
        Ok(())
    }

    fn roots(&self) -> Result<Vec<GcRef>, FrameError> {
        self.validate_active_map()?;
        let map = self
            .descriptor
            .root_maps
            .get(&self.current_safepoint)
            .ok_or(FrameError::UnknownSafepoint)?;
        map.iter()
            .map(|slot| match self.slots[*slot] {
                JsValue::Managed(reference) => Ok(reference),
                _ => Err(FrameError::NonManagedRootSlot),
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeFrameToken(u64);

#[derive(Default)]
pub struct NativeFrameStack {
    next_token: u64,
    frames: Vec<(NativeFrameToken, NativeFrame)>,
}

impl NativeFrameStack {
    pub fn enter(&mut self, frame: NativeFrame) -> Result<NativeFrameToken, FrameError> {
        let token_value = if self.next_token == 0 {
            1
        } else {
            self.next_token
        };
        self.next_token = token_value
            .checked_add(1)
            .ok_or(FrameError::TokenSpaceExhausted)?;
        let token = NativeFrameToken(token_value);
        self.frames.push((token, frame));
        Ok(token)
    }

    pub fn current_mut(&mut self, token: NativeFrameToken) -> Result<&mut NativeFrame, FrameError> {
        let (current, frame) = self.frames.last_mut().ok_or(FrameError::InvalidFrame)?;
        if *current != token {
            return Err(FrameError::FrameStackOrder);
        }
        Ok(frame)
    }

    pub fn leave(&mut self, token: NativeFrameToken) -> Result<(), FrameError> {
        let (current, _) = self.frames.last().ok_or(FrameError::InvalidFrame)?;
        if *current != token {
            return Err(FrameError::FrameStackOrder);
        }
        self.frames.pop();
        Ok(())
    }

    fn roots(&self) -> Result<Vec<GcRef>, FrameError> {
        let mut roots = Vec::new();
        for (_, frame) in &self.frames {
            roots.extend(frame.roots()?);
        }
        Ok(roots)
    }

    pub fn depth(&self) -> usize {
        self.frames.len()
    }
}

pub struct InterpreterFrame {
    pub function: String,
    pub closure_environment: Vec<GcRef>,
    pub locals: Vec<JsValue>,
}

impl InterpreterFrame {
    pub fn new(
        function: impl Into<String>,
        closure_environment: Vec<GcRef>,
        locals: Vec<JsValue>,
    ) -> Self {
        Self {
            function: function.into(),
            closure_environment,
            locals,
        }
    }

    fn roots(&self) -> Vec<GcRef> {
        let mut roots = self.closure_environment.clone();
        roots.extend(self.locals.iter().filter_map(|value| match value {
            JsValue::Managed(reference) => Some(*reference),
            JsValue::Undefined | JsValue::Boolean(_) | JsValue::Number(_) => None,
        }));
        roots
    }
}

#[derive(Default)]
pub struct InterpreterStack {
    frames: Vec<InterpreterFrame>,
}

impl InterpreterStack {
    pub fn enter(&mut self, frame: InterpreterFrame) {
        self.frames.push(frame);
    }

    pub fn leave(&mut self) -> Option<InterpreterFrame> {
        self.frames.pop()
    }

    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    fn roots(&self) -> Vec<GcRef> {
        self.frames
            .iter()
            .flat_map(InterpreterFrame::roots)
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostHandle {
    index: usize,
    generation: u64,
}

#[derive(Clone, Copy, Debug)]
struct HostSlot {
    generation: u64,
    target: Option<GcRef>,
}

#[derive(Default)]
pub struct PersistentHandles {
    slots: Vec<HostSlot>,
    free: Vec<usize>,
}

impl PersistentHandles {
    pub fn retain(&mut self, heap: &GcHeap, reference: GcRef) -> Result<HostHandle, HeapError> {
        heap.validate(reference)?;
        let index = if let Some(index) = self.free.pop() {
            self.slots[index].target = Some(reference);
            index
        } else {
            let index = self.slots.len();
            self.slots.push(HostSlot {
                generation: 1,
                target: Some(reference),
            });
            index
        };
        Ok(HostHandle {
            index,
            generation: self.slots[index].generation,
        })
    }

    pub fn resolve(&self, handle: HostHandle) -> Result<GcRef, FrameError> {
        let slot = self
            .slots
            .get(handle.index)
            .ok_or(FrameError::InvalidFrame)?;
        if slot.generation != handle.generation {
            return Err(FrameError::InvalidFrame);
        }
        slot.target.ok_or(FrameError::InvalidFrame)
    }

    pub fn release(&mut self, handle: HostHandle) -> Result<GcRef, FrameError> {
        let slot = self
            .slots
            .get_mut(handle.index)
            .ok_or(FrameError::InvalidFrame)?;
        if slot.generation != handle.generation {
            return Err(FrameError::InvalidFrame);
        }
        let target = slot.target.take().ok_or(FrameError::InvalidFrame)?;
        if let Some(next_generation) = slot.generation.checked_add(1) {
            slot.generation = next_generation;
            self.free.push(handle.index);
        }
        Ok(target)
    }

    fn roots(&self) -> Vec<GcRef> {
        self.slots.iter().filter_map(|slot| slot.target).collect()
    }
}

/// The three provider classes consumed by the common tracing root protocol.
#[derive(Default)]
pub struct RootProviders {
    pub interpreter: InterpreterStack,
    pub native: NativeFrameStack,
    pub persistent: PersistentHandles,
}

impl RootProviders {
    pub fn collect(&self, heap: &mut GcHeap) -> Result<CollectionStats, RootCollectionError> {
        let mut roots = self.interpreter.roots();
        roots.extend(self.native.roots()?);
        roots.extend(self.persistent.roots());
        Ok(heap.collect(roots)?)
    }

    pub fn collect_with_execution(
        &self,
        heap: &mut GcHeap,
        execution: &ActivationStack,
    ) -> Result<CollectionStats, RootCollectionError> {
        let mut roots = self.interpreter.roots();
        roots.extend(self.native.roots()?);
        roots.extend(self.persistent.roots());
        roots.extend(execution.roots());
        Ok(heap.collect(roots)?)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RootCollectionError {
    Frame(FrameError),
    Heap(HeapError),
}

impl From<FrameError> for RootCollectionError {
    fn from(value: FrameError) -> Self {
        Self::Frame(value)
    }
}

impl From<HeapError> for RootCollectionError {
    fn from(value: HeapError) -> Self {
        Self::Heap(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivationKind {
    Native,
    Interpreter,
    HostCallback,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLocation {
    pub file: String,
    pub line: u32,
    pub column: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activation {
    pub kind: ActivationKind,
    pub function: String,
    pub closure_context: Option<u64>,
    pub source: SourceLocation,
}

impl Activation {
    pub fn new(
        kind: ActivationKind,
        function: impl Into<String>,
        closure_context: Option<u64>,
        source: SourceLocation,
    ) -> Self {
        Self {
            kind,
            function: function.into(),
            closure_context,
            source,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ActivationToken(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitionError {
    FrameStackOrder,
    TokenSpaceExhausted,
    PendingCompletionNotConsumed,
    MissingPendingCompletion,
    CompletionMismatch,
    HostCompletionNotConsumed,
    MissingHostCompletion,
    HostCompletionMismatch,
    ConsumedCompletionNotFound,
    LiveRootNotFound,
}

impl fmt::Display for TransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for TransitionError {}

struct CompletionRecord {
    value: Completion,
    /// Captured at the original throw site and retained while the completion
    /// propagates through native/interpreter/host activations.
    stack_trace: Option<Vec<Activation>>,
}

struct ActiveActivation {
    token: ActivationToken,
    context: Activation,
    pending_completion: Option<CompletionRecord>,
    consumed_completions: Vec<CompletionRecord>,
    live_roots: Vec<GcRef>,
}

#[derive(Default)]
pub struct ActivationStack {
    next_token: u64,
    frames: Vec<ActiveActivation>,
    host_completion: Option<CompletionRecord>,
}

impl ActivationStack {
    pub fn current(&self) -> Option<&Activation> {
        self.frames.last().map(|frame| &frame.context)
    }

    pub fn snapshot(&self) -> Vec<Activation> {
        self.frames
            .iter()
            .map(|frame| frame.context.clone())
            .collect()
    }

    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    /// Roots pending completions and completion values consumed into active
    /// caller locals. The top-level result stays rooted until host promotion.
    pub fn roots(&self) -> Vec<GcRef> {
        let mut roots = Vec::new();
        for frame in &self.frames {
            roots.extend(frame.live_roots.iter().copied());
            if let Some(reference) = frame
                .pending_completion
                .as_ref()
                .and_then(|record| record.value.managed_root())
            {
                roots.push(reference);
            }
        }
        if let Some(reference) = self
            .host_completion
            .as_ref()
            .and_then(|record| record.value.managed_root())
        {
            roots.push(reference);
        }
        roots
    }

    /// Returns the source/closure stack captured at the original throw site
    /// while the current activation is holding a pending completion.
    pub fn pending_stack_trace(&self) -> Option<&[Activation]> {
        self.frames
            .last()?
            .pending_completion
            .as_ref()?
            .stack_trace
            .as_deref()
    }

    /// Returns the source/closure stack captured at the original throw site
    /// for a completion already consumed by the current activation.
    pub fn consumed_stack_trace(&self, expected: Completion) -> Option<&[Activation]> {
        self.frames
            .last()?
            .consumed_completions
            .iter()
            .rev()
            .find(|record| record.value == expected)?
            .stack_trace
            .as_deref()
    }

    /// Returns the captured source/closure stack for the top-level result.
    pub fn host_stack_trace(&self) -> Option<&[Activation]> {
        self.host_completion.as_ref()?.stack_trace.as_deref()
    }

    /// Transfers a pending callee completion into the current activation's
    /// live roots before the caller inspects or otherwise uses its value.
    pub fn consume_pending_completion(
        &mut self,
        expected: Completion,
    ) -> Result<(), TransitionError> {
        let frame = self
            .frames
            .last_mut()
            .ok_or(TransitionError::MissingPendingCompletion)?;
        let record = frame
            .pending_completion
            .take()
            .ok_or(TransitionError::MissingPendingCompletion)?;
        if record.value != expected {
            frame.pending_completion = Some(record);
            return Err(TransitionError::CompletionMismatch);
        }
        if let Some(reference) = record.value.managed_root() {
            frame.live_roots.push(reference);
        }
        frame.consumed_completions.push(record);
        Ok(())
    }

    /// Releases one completion-derived local root after the caller has either
    /// finished using the value or transferred it to another registered root.
    pub fn release_live_root(&mut self, reference: GcRef) -> Result<(), TransitionError> {
        let frame = self
            .frames
            .last_mut()
            .ok_or(TransitionError::LiveRootNotFound)?;
        let position = frame
            .live_roots
            .iter()
            .rposition(|root| *root == reference)
            .ok_or(TransitionError::LiveRootNotFound)?;
        frame.live_roots.remove(position);

        if let Some(record_position) = frame
            .consumed_completions
            .iter()
            .rposition(|record| record.value.managed_root() == Some(reference))
        {
            frame.consumed_completions.remove(record_position);
        }
        Ok(())
    }

    /// Drops a consumed completion after it has been handled. For managed
    /// values this also releases its caller-local root.
    pub fn release_consumed_completion(
        &mut self,
        expected: Completion,
    ) -> Result<(), TransitionError> {
        let frame = self
            .frames
            .last_mut()
            .ok_or(TransitionError::MissingPendingCompletion)?;
        let position = frame
            .consumed_completions
            .iter()
            .rposition(|record| record.value == expected)
            .ok_or(TransitionError::ConsumedCompletionNotFound)?;
        let record = frame.consumed_completions.remove(position);
        if let Some(reference) = record.value.managed_root() {
            if let Some(root_position) =
                frame.live_roots.iter().rposition(|root| *root == reference)
            {
                frame.live_roots.remove(root_position);
            }
        }
        Ok(())
    }

    pub fn host_completion(&self) -> Option<Completion> {
        self.host_completion.as_ref().map(|record| record.value)
    }

    /// Call only after a managed host result has been promoted to a persistent
    /// host handle. Primitive completions need no persistent root.
    pub fn release_host_completion_after_promotion(
        &mut self,
        expected: Completion,
    ) -> Result<(), TransitionError> {
        let record = self
            .host_completion
            .take()
            .ok_or(TransitionError::MissingHostCompletion)?;
        if record.value != expected {
            self.host_completion = Some(record);
            return Err(TransitionError::HostCompletionMismatch);
        }
        Ok(())
    }

    pub fn invoke(
        &mut self,
        activation: Activation,
        callback: impl FnOnce(&mut Self) -> Completion,
    ) -> Result<Completion, TransitionError> {
        if self.host_completion.is_some() {
            return Err(TransitionError::HostCompletionNotConsumed);
        }
        if self
            .frames
            .last()
            .is_some_and(|frame| frame.pending_completion.is_some())
        {
            return Err(TransitionError::PendingCompletionNotConsumed);
        }

        let token_value = if self.next_token == 0 {
            1
        } else {
            self.next_token
        };
        self.next_token = token_value
            .checked_add(1)
            .ok_or(TransitionError::TokenSpaceExhausted)?;
        let token = ActivationToken(token_value);
        self.frames.push(ActiveActivation {
            token,
            context: activation,
            pending_completion: None,
            consumed_completions: Vec::new(),
            live_roots: Vec::new(),
        });

        let callback_result = catch_unwind(AssertUnwindSafe(|| callback(self)));
        let completion = match callback_result {
            Ok(completion) => completion,
            Err(payload) => {
                match self.frames.last() {
                    Some(frame) if frame.token == token => {
                        self.frames.pop();
                    }
                    _ => {
                        resume_unwind(payload);
                    }
                }
                resume_unwind(payload);
            }
        };

        let current = self.frames.last().ok_or(TransitionError::FrameStackOrder)?;
        if current.token != token {
            return Err(TransitionError::FrameStackOrder);
        }
        if current
            .pending_completion
            .as_ref()
            .is_some_and(|pending| pending.value != completion)
        {
            self.frames.pop();
            return Err(TransitionError::CompletionMismatch);
        }

        let has_pending = current.pending_completion.is_some();
        let consumed_position = if has_pending {
            None
        } else {
            current
                .consumed_completions
                .iter()
                .rposition(|record| record.value == completion)
        };
        let should_capture_throw_trace = !has_pending
            && consumed_position.is_none()
            && matches!(completion, Completion::Throw(_));
        let new_stack_trace = if should_capture_throw_trace {
            Some(self.snapshot())
        } else {
            None
        };

        let current = self
            .frames
            .last_mut()
            .ok_or(TransitionError::FrameStackOrder)?;
        let record = if let Some(pending) = current.pending_completion.take() {
            pending
        } else if let Some(position) = consumed_position {
            current.consumed_completions.remove(position)
        } else {
            CompletionRecord {
                value: completion,
                stack_trace: new_stack_trace,
            }
        };

        self.frames.pop();
        if let Some(parent) = self.frames.last_mut() {
            if parent.pending_completion.is_some() {
                return Err(TransitionError::PendingCompletionNotConsumed);
            }
            parent.pending_completion = Some(record);
        } else {
            if self.host_completion.is_some() {
                return Err(TransitionError::HostCompletionNotConsumed);
            }
            self.host_completion = Some(record);
        }
        Ok(completion)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Completion {
    Normal(JsValue),
    Return(JsValue),
    Break(u32),
    Continue(u32),
    Throw(JsValue),
}

impl Completion {
    pub fn managed_root(self) -> Option<GcRef> {
        match self {
            Self::Normal(JsValue::Managed(reference))
            | Self::Return(JsValue::Managed(reference))
            | Self::Throw(JsValue::Managed(reference)) => Some(reference),
            Self::Normal(JsValue::Undefined | JsValue::Boolean(_) | JsValue::Number(_))
            | Self::Return(JsValue::Undefined | JsValue::Boolean(_) | JsValue::Number(_))
            | Self::Throw(JsValue::Undefined | JsValue::Boolean(_) | JsValue::Number(_))
            | Self::Break(_)
            | Self::Continue(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollSite {
    FunctionEntry,
    LoopBackEdge,
    AllocationBoundary,
    CallBoundary,
    BuiltinChunk(BuiltinKind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinKind {
    RegExp,
    ComparatorSort,
    Json,
    String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterruptStrategy {
    CooperativeChunks {
        max_work_per_poll: u64,
    },
    ComparatorCallbacks {
        poll_before_and_after: bool,
        managed_callback_must_poll: bool,
        untrusted_native_callback_requires_isolation: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuiltinPolicy {
    pub kind: BuiltinKind,
    pub strategy: InterruptStrategy,
}

pub const fn interrupt_policy(kind: BuiltinKind) -> BuiltinPolicy {
    let strategy = match kind {
        BuiltinKind::RegExp => InterruptStrategy::CooperativeChunks {
            max_work_per_poll: 64,
        },
        BuiltinKind::ComparatorSort => InterruptStrategy::ComparatorCallbacks {
            poll_before_and_after: true,
            managed_callback_must_poll: true,
            untrusted_native_callback_requires_isolation: true,
        },
        BuiltinKind::Json | BuiltinKind::String => InterruptStrategy::CooperativeChunks {
            max_work_per_poll: 256,
        },
    };
    BuiltinPolicy { kind, strategy }
}

#[derive(Clone, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollError {
    InvalidPollWindow,
    PollWindowExceeded {
        attempted: u64,
        maximum: u64,
    },
    BudgetExceeded {
        site: PollSite,
        limit: u64,
        observed: u64,
        overshoot: u64,
    },
    Cancelled {
        site: PollSite,
    },
    CounterOverflow,
    BuiltinRequiresComparatorBoundary,
}

impl fmt::Display for PollError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for PollError {}

pub struct BudgetMeter {
    limit: u64,
    spent: u64,
    pending: u64,
    max_unpolled_work: u64,
    cancellation: CancellationToken,
    poll_trace: Vec<PollSite>,
}

impl BudgetMeter {
    pub fn new(limit: u64, max_unpolled_work: u64) -> Result<Self, PollError> {
        if max_unpolled_work == 0 {
            return Err(PollError::InvalidPollWindow);
        }
        Ok(Self {
            limit,
            spent: 0,
            pending: 0,
            max_unpolled_work,
            cancellation: CancellationToken::default(),
            poll_trace: Vec::new(),
        })
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub fn spent(&self) -> u64 {
        self.spent
    }

    pub fn max_unpolled_work(&self) -> u64 {
        self.max_unpolled_work
    }

    pub fn poll_trace(&self) -> &[PollSite] {
        &self.poll_trace
    }

    pub fn record_work(&mut self, units: u64) -> Result<(), PollError> {
        let attempted = self
            .pending
            .checked_add(units)
            .ok_or(PollError::CounterOverflow)?;
        if attempted > self.max_unpolled_work {
            return Err(PollError::PollWindowExceeded {
                attempted,
                maximum: self.max_unpolled_work,
            });
        }
        self.pending = attempted;
        Ok(())
    }

    pub fn poll(&mut self, site: PollSite) -> Result<(), PollError> {
        self.poll_trace.push(site);
        if self.cancellation.is_cancelled() {
            return Err(PollError::Cancelled { site });
        }

        let observed = self
            .spent
            .checked_add(self.pending)
            .ok_or(PollError::CounterOverflow)?;
        if observed > self.limit {
            return Err(PollError::BudgetExceeded {
                site,
                limit: self.limit,
                observed,
                overshoot: observed - self.limit,
            });
        }

        self.spent = observed;
        self.pending = 0;
        Ok(())
    }
}

pub fn run_chunked_builtin(
    kind: BuiltinKind,
    work_units: u64,
    meter: &mut BudgetMeter,
) -> Result<(), PollError> {
    let chunk_size = match interrupt_policy(kind).strategy {
        InterruptStrategy::CooperativeChunks { max_work_per_poll } => max_work_per_poll,
        InterruptStrategy::ComparatorCallbacks { .. } => {
            return Err(PollError::BuiltinRequiresComparatorBoundary);
        }
    };

    meter.poll(PollSite::FunctionEntry)?;
    let mut completed = 0;
    while completed < work_units {
        let chunk = (work_units - completed).min(chunk_size);
        meter.record_work(chunk)?;
        meter.poll(PollSite::BuiltinChunk(kind))?;
        completed += chunk;
    }
    Ok(())
}

/// The sort loop polls around every comparator call. Managed comparators share
/// this meter and must poll internally; native host callbacks are cooperative
/// trusted code, not preemptible or sandboxed by this contract.
pub fn run_comparator_call(
    meter: &mut BudgetMeter,
    callback: impl FnOnce(&mut BudgetMeter) -> Result<(), PollError>,
) -> Result<(), PollError> {
    meter.poll(PollSite::CallBoundary)?;
    callback(meter)?;
    meter.poll(PollSite::CallBoundary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(line: u32) -> SourceLocation {
        SourceLocation {
            file: "fixture.js".to_owned(),
            line,
            column: 1,
        }
    }

    fn native_frame(
        reference: GcRef,
        entry: SafepointId,
        additional_maps: Vec<RootMap>,
    ) -> NativeFrame {
        let mut maps = vec![RootMap {
            safepoint: entry,
            slots: vec![0],
        }];
        maps.extend(additional_maps);
        let descriptor =
            FrameDescriptor::new("native_function", 1, entry, maps).expect("valid descriptor");
        NativeFrame::new(descriptor, vec![JsValue::Managed(reference)]).expect("valid native frame")
    }

    #[test]
    fn gc_enumerates_interpreter_native_and_persistent_host_roots() {
        let mut heap = GcHeap::default();
        let interpreter_local = heap.allocate();
        let closure_capture = heap.allocate();
        let native_local = heap.allocate();
        let host_retained = heap.allocate();
        let reachable_through_edge = heap.allocate();
        let unreachable = heap.allocate();
        heap.add_edge(interpreter_local, reachable_through_edge)
            .expect("valid edge");

        let mut roots = RootProviders::default();
        roots.interpreter.enter(InterpreterFrame::new(
            "closure",
            vec![closure_capture],
            vec![JsValue::Managed(interpreter_local)],
        ));

        let frame = native_frame(native_local, SafepointId(0), Vec::new());
        let native_token = roots.native.enter(frame).expect("frame entered");
        let host_handle = roots
            .persistent
            .retain(&heap, host_retained)
            .expect("host handle retained");

        let report = roots.collect(&mut heap).expect("collection succeeds");
        assert_eq!(report.roots_scanned, 4);
        assert_eq!(report.objects_reclaimed, 1);
        assert!(heap.is_alive(interpreter_local));
        assert!(heap.is_alive(closure_capture));
        assert!(heap.is_alive(native_local));
        assert!(heap.is_alive(host_retained));
        assert!(heap.is_alive(reachable_through_edge));
        assert!(!heap.is_alive(unreachable));

        roots.native.leave(native_token).expect("native frame left");
        roots.interpreter.leave().expect("interpreter frame left");
        roots
            .persistent
            .release(host_handle)
            .expect("host handle released");
        let final_report = roots.collect(&mut heap).expect("collection succeeds");
        assert_eq!(final_report.objects_live, 0);
    }

    #[test]
    fn native_root_map_must_match_live_references_at_each_safepoint() {
        let mut heap = GcHeap::default();
        let first = heap.allocate();
        let second = heap.allocate();
        let descriptor = FrameDescriptor::new(
            "two_slots",
            2,
            SafepointId(0),
            vec![
                RootMap {
                    safepoint: SafepointId(0),
                    slots: vec![0],
                },
                RootMap {
                    safepoint: SafepointId(1),
                    slots: vec![1],
                },
            ],
        )
        .expect("valid descriptor");
        let mut frame = NativeFrame::new(
            descriptor,
            vec![JsValue::Managed(first), JsValue::Managed(second)],
        )
        .expect("valid frame");

        assert_eq!(
            frame.set_safepoint(SafepointId(1), &[0]),
            Err(FrameError::LiveRootMapMismatch)
        );
        frame
            .set_safepoint(SafepointId(1), &[1])
            .expect("exact root map accepted");

        let mut providers = RootProviders::default();
        providers.native.enter(frame).expect("native frame entered");
        let report = providers.collect(&mut heap).expect("collection succeeds");
        assert_eq!(report.objects_reclaimed, 1);
        assert!(heap.is_alive(second));
        assert!(!heap.is_alive(first));
    }

    #[test]
    fn native_frame_root_lifecycle_is_lifo_and_releases_roots() {
        let mut heap = GcHeap::default();
        let outer_ref = heap.allocate();
        let inner_ref = heap.allocate();
        let mut frames = NativeFrameStack::default();
        let outer = frames
            .enter(native_frame(outer_ref, SafepointId(0), Vec::new()))
            .expect("outer entered");
        let inner = frames
            .enter(native_frame(inner_ref, SafepointId(0), Vec::new()))
            .expect("inner entered");

        assert_eq!(frames.leave(outer), Err(FrameError::FrameStackOrder));
        frames.leave(inner).expect("inner left");
        frames.leave(outer).expect("outer left");
        assert_eq!(frames.depth(), 0);
    }

    #[test]
    fn malformed_native_root_maps_are_rejected_before_execution() {
        assert_eq!(
            FrameDescriptor::new(
                "duplicate",
                1,
                SafepointId(0),
                vec![RootMap {
                    safepoint: SafepointId(0),
                    slots: vec![0, 0],
                }],
            )
            .err(),
            Some(FrameError::DuplicateRootSlot)
        );
        assert_eq!(
            FrameDescriptor::new(
                "out_of_bounds",
                1,
                SafepointId(0),
                vec![RootMap {
                    safepoint: SafepointId(0),
                    slots: vec![1],
                }],
            )
            .err(),
            Some(FrameError::RootSlotOutOfBounds)
        );
    }

    #[test]
    fn all_script_completions_cross_nested_execution_boundaries_unchanged() {
        let completions = [
            Completion::Normal(JsValue::Number(1)),
            Completion::Return(JsValue::Number(2)),
            Completion::Break(3),
            Completion::Continue(4),
            Completion::Throw(JsValue::Number(5)),
        ];

        for completion in completions {
            let mut stack = ActivationStack::default();
            let actual = stack
                .invoke(
                    Activation::new(ActivationKind::Native, "aot_entry", None, source(1)),
                    |stack| {
                        stack
                            .invoke(
                                Activation::new(
                                    ActivationKind::Interpreter,
                                    "interpreted_callee",
                                    Some(77),
                                    source(2),
                                ),
                                |_| completion,
                            )
                            .expect("interpreter transition succeeds")
                    },
                )
                .expect("native transition succeeds");
            assert_eq!(actual, completion);
            assert_eq!(stack.depth(), 0);
            assert_eq!(stack.host_completion(), Some(completion));
            stack
                .release_host_completion_after_promotion(completion)
                .expect("top-level completion released after handoff");
            assert_eq!(stack.host_completion(), None);
        }
    }

    #[test]
    fn nested_host_callback_preserves_stack_source_and_closure_context() {
        let mut stack = ActivationStack::default();
        let result = stack
            .invoke(
                Activation::new(ActivationKind::Native, "native_outer", None, source(10)),
                |stack| {
                    stack
                        .invoke(
                            Activation::new(
                                ActivationKind::Interpreter,
                                "closure_body",
                                Some(9001),
                                source(20),
                            ),
                            |stack| {
                                assert_eq!(
                                    stack.current().map(|frame| frame.closure_context),
                                    Some(Some(9001))
                                );
                                stack
                                    .invoke(
                                        Activation::new(
                                            ActivationKind::HostCallback,
                                            "host_callback",
                                            Some(9001),
                                            source(21),
                                        ),
                                        |stack| {
                                            assert_eq!(stack.depth(), 3);
                                            stack
                                                .invoke(
                                                    Activation::new(
                                                        ActivationKind::Interpreter,
                                                        "reentrant_interpreter",
                                                        Some(9001),
                                                        source(22),
                                                    ),
                                                    |stack| {
                                                        let snapshot = stack.snapshot();
                                                        assert_eq!(snapshot.len(), 4);
                                                        assert_eq!(
                                                            snapshot[1].function,
                                                            "closure_body"
                                                        );
                                                        assert_eq!(
                                                            snapshot[1].closure_context,
                                                            Some(9001)
                                                        );
                                                        assert_eq!(
                                                            snapshot[2].kind,
                                                            ActivationKind::HostCallback
                                                        );
                                                        assert_eq!(snapshot[3].source.line, 22);
                                                        Completion::Throw(JsValue::Number(404))
                                                    },
                                                )
                                                .expect("reentrant interpreter returns")
                                        },
                                    )
                                    .expect("host callback returns")
                            },
                        )
                        .expect("interpreter returns")
                },
            )
            .expect("native invocation returns");

        assert_eq!(result, Completion::Throw(JsValue::Number(404)));
        assert_eq!(stack.depth(), 0);
        assert_eq!(stack.host_completion(), Some(result));
        stack
            .release_host_completion_after_promotion(result)
            .expect("primitive result released");
    }

    #[test]
    fn managed_completion_stays_rooted_across_callbacks_gc_and_host_promotion() {
        let mut heap = GcHeap::default();
        let exception = heap.allocate();
        let mut providers = RootProviders::default();
        let mut stack = ActivationStack::default();
        let thrown = Completion::Throw(JsValue::Managed(exception));

        let result = stack
            .invoke(
                Activation::new(ActivationKind::Native, "native_outer", None, source(1)),
                |stack| {
                    stack
                        .invoke(
                            Activation::new(
                                ActivationKind::Interpreter,
                                "script_caller",
                                Some(42),
                                source(2),
                            ),
                            |stack| {
                                let callback_result = stack
                                    .invoke(
                                        Activation::new(
                                            ActivationKind::HostCallback,
                                            "host_callback",
                                            Some(42),
                                            source(3),
                                        ),
                                        |stack| {
                                            let completion = stack
                                                .invoke(
                                                    Activation::new(
                                                        ActivationKind::Interpreter,
                                                        "throwing_callee",
                                                        Some(42),
                                                        source(4),
                                                    ),
                                                    |_| thrown,
                                                )
                                                .expect("throwing interpreter returns");

                                            assert_eq!(stack.host_completion(), None);
                                            assert_eq!(stack.roots(), vec![exception]);
                                            let original_trace = stack
                                                .pending_stack_trace()
                                                .expect("throw captures source and closure stack")
                                                .to_vec();
                                            assert_eq!(original_trace.len(), 4);
                                            assert_eq!(original_trace[0].function, "native_outer");
                                            assert_eq!(original_trace[1].closure_context, Some(42));
                                            assert_eq!(
                                                original_trace[2].kind,
                                                ActivationKind::HostCallback
                                            );
                                            assert_eq!(
                                                original_trace[3].function,
                                                "throwing_callee"
                                            );
                                            assert_eq!(original_trace[3].source.line, 4);
                                            providers
                                                .collect_with_execution(&mut heap, stack)
                                                .expect("GC sees pending completion root");
                                            assert!(heap.is_alive(exception));

                                            stack
                                                .consume_pending_completion(completion)
                                                .expect("caller consumes pending completion");
                                            assert_eq!(stack.roots(), vec![exception]);
                                            assert_eq!(
                                                stack.consumed_stack_trace(completion),
                                                Some(original_trace.as_slice())
                                            );
                                            providers
                                                .collect_with_execution(&mut heap, stack)
                                                .expect("GC sees caller live root");
                                            assert!(heap.is_alive(exception));
                                            completion
                                        },
                                    )
                                    .expect("host callback returns");

                                stack
                                    .consume_pending_completion(callback_result)
                                    .expect("interpreter caller consumes callback completion");
                                assert_eq!(stack.roots(), vec![exception]);
                                callback_result
                            },
                        )
                        .expect("script caller returns")
                },
            )
            .expect("native entry returns");

        assert_eq!(result, thrown);
        assert_eq!(stack.roots(), vec![exception]);
        let host_trace = stack
            .host_stack_trace()
            .expect("exception source stack survives activation unwinding");
        assert_eq!(host_trace.len(), 4);
        assert_eq!(host_trace[0].function, "native_outer");
        assert_eq!(host_trace[1].function, "script_caller");
        assert_eq!(host_trace[2].function, "host_callback");
        assert_eq!(host_trace[3].function, "throwing_callee");
        assert_eq!(host_trace[3].source.line, 4);
        let handle = providers
            .persistent
            .retain(&heap, exception)
            .expect("host promotes managed exception to persistent handle");
        stack
            .release_host_completion_after_promotion(result)
            .expect("execution root released only after host promotion");

        providers
            .collect_with_execution(&mut heap, &stack)
            .expect("persistent handle keeps exception alive");
        assert!(heap.is_alive(exception));

        providers
            .persistent
            .release(handle)
            .expect("handle released");
        providers
            .collect_with_execution(&mut heap, &stack)
            .expect("collection runs after host handle release");
        assert!(!heap.is_alive(exception));
    }

    #[test]
    fn pending_completion_must_be_consumed_before_another_nested_call() {
        let mut stack = ActivationStack::default();
        let thrown = Completion::Throw(JsValue::Number(9));
        let result = stack
            .invoke(
                Activation::new(ActivationKind::Native, "native_outer", None, source(1)),
                |stack| {
                    let completion = stack
                        .invoke(
                            Activation::new(
                                ActivationKind::Interpreter,
                                "throwing_callee",
                                None,
                                source(2),
                            ),
                            |_| thrown,
                        )
                        .expect("callee returns throw completion");

                    assert_eq!(
                        stack.invoke(
                            Activation::new(
                                ActivationKind::HostCallback,
                                "must_not_run",
                                None,
                                source(3),
                            ),
                            |_| Completion::Normal(JsValue::Undefined),
                        ),
                        Err(TransitionError::PendingCompletionNotConsumed)
                    );
                    completion
                },
            )
            .expect("original completion propagates");

        assert_eq!(result, thrown);
        assert_eq!(stack.host_completion(), Some(thrown));
        stack
            .release_host_completion_after_promotion(thrown)
            .expect("primitive completion released");
    }

    #[test]
    fn releasing_consumed_completion_removes_its_temporary_root() {
        let mut heap = GcHeap::default();
        let exception = heap.allocate();
        let providers = RootProviders::default();
        let mut stack = ActivationStack::default();
        let thrown = Completion::Throw(JsValue::Managed(exception));

        stack
            .invoke(
                Activation::new(ActivationKind::Native, "native_outer", None, source(1)),
                |stack| {
                    let completion = stack
                        .invoke(
                            Activation::new(
                                ActivationKind::Interpreter,
                                "throwing_callee",
                                None,
                                source(2),
                            ),
                            |_| thrown,
                        )
                        .expect("callee returns managed throw");
                    stack
                        .consume_pending_completion(completion)
                        .expect("caller roots consumed exception");
                    providers
                        .collect_with_execution(&mut heap, stack)
                        .expect("consumed completion remains rooted");
                    assert!(heap.is_alive(exception));

                    stack
                        .release_consumed_completion(completion)
                        .expect("caller releases handled exception");
                    providers
                        .collect_with_execution(&mut heap, stack)
                        .expect("released completion is no longer rooted");
                    assert!(!heap.is_alive(exception));
                    Completion::Normal(JsValue::Undefined)
                },
            )
            .expect("outer activation returns normally");
    }

    #[test]
    fn panic_unwinds_all_activation_frames_without_leaking_stack_state() {
        let mut stack = ActivationStack::default();
        let panic_result = catch_unwind(AssertUnwindSafe(|| {
            let _ = stack.invoke(
                Activation::new(ActivationKind::Native, "native_outer", None, source(1)),
                |stack| {
                    stack
                        .invoke(
                            Activation::new(
                                ActivationKind::Interpreter,
                                "panicking_script",
                                None,
                                source(2),
                            ),
                            |_| panic!("simulated panic inside script callback"),
                        )
                        .expect("panic must resume through caller")
                },
            );
        }));

        assert!(panic_result.is_err());
        assert_eq!(stack.depth(), 0);
        assert_eq!(stack.host_completion(), None);
    }

    #[test]
    fn budget_polls_cover_function_loop_allocation_and_call_boundaries() {
        let mut meter = BudgetMeter::new(100, 8).expect("valid poll window");
        for site in [
            PollSite::FunctionEntry,
            PollSite::LoopBackEdge,
            PollSite::AllocationBoundary,
            PollSite::CallBoundary,
        ] {
            meter.record_work(2).expect("work stays within poll window");
            meter.poll(site).expect("budget remains available");
        }
        assert_eq!(
            meter.poll_trace(),
            &[
                PollSite::FunctionEntry,
                PollSite::LoopBackEdge,
                PollSite::AllocationBoundary,
                PollSite::CallBoundary,
            ]
        );
    }

    #[test]
    fn maximum_budget_overshoot_is_bounded_by_one_poll_window() {
        let mut meter = BudgetMeter::new(10, 3).expect("valid poll window");
        for _ in 0..3 {
            meter.record_work(3).expect("bounded work");
            meter
                .poll(PollSite::LoopBackEdge)
                .expect("budget below limit");
        }

        meter.record_work(3).expect("last bounded work");
        let error = meter
            .poll(PollSite::LoopBackEdge)
            .expect_err("budget must be exhausted");
        match error {
            PollError::BudgetExceeded {
                limit,
                observed,
                overshoot,
                ..
            } => {
                assert_eq!(limit, 10);
                assert_eq!(observed, 12);
                assert_eq!(overshoot, 2);
                assert!(overshoot <= meter.max_unpolled_work());
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn cancellation_requested_inside_nested_host_callback_is_observed_at_call_poll() {
        let mut meter = BudgetMeter::new(100, 8).expect("valid meter");
        let cancellation = meter.cancellation_token();
        let mut stack = ActivationStack::default();

        let result = stack
            .invoke(
                Activation::new(ActivationKind::Native, "native", None, source(1)),
                |stack| {
                    stack
                        .invoke(
                            Activation::new(
                                ActivationKind::Interpreter,
                                "script",
                                Some(1),
                                source(2),
                            ),
                            |stack| {
                                stack
                                    .invoke(
                                        Activation::new(
                                            ActivationKind::HostCallback,
                                            "cancel",
                                            Some(1),
                                            source(3),
                                        ),
                                        |_| {
                                            cancellation.cancel();
                                            Completion::Normal(JsValue::Undefined)
                                        },
                                    )
                                    .expect("callback returns")
                            },
                        )
                        .expect("interpreter returns")
                },
            )
            .expect("native returns");

        assert_eq!(result, Completion::Normal(JsValue::Undefined));
        assert_eq!(
            meter.poll(PollSite::CallBoundary),
            Err(PollError::Cancelled {
                site: PollSite::CallBoundary
            })
        );
    }

    #[test]
    fn long_running_builtin_policies_are_explicit_and_chunked_work_is_interruptible() {
        assert_eq!(
            interrupt_policy(BuiltinKind::RegExp).strategy,
            InterruptStrategy::CooperativeChunks {
                max_work_per_poll: 64
            }
        );
        assert_eq!(
            interrupt_policy(BuiltinKind::Json).strategy,
            InterruptStrategy::CooperativeChunks {
                max_work_per_poll: 256
            }
        );
        assert_eq!(
            interrupt_policy(BuiltinKind::String).strategy,
            InterruptStrategy::CooperativeChunks {
                max_work_per_poll: 256
            }
        );
        assert_eq!(
            interrupt_policy(BuiltinKind::ComparatorSort).strategy,
            InterruptStrategy::ComparatorCallbacks {
                poll_before_and_after: true,
                managed_callback_must_poll: true,
                untrusted_native_callback_requires_isolation: true,
            }
        );

        let mut meter = BudgetMeter::new(300, 64).expect("valid meter");
        let error = run_chunked_builtin(BuiltinKind::RegExp, 512, &mut meter)
            .expect_err("regexp workload exceeds budget");
        assert!(matches!(
            error,
            PollError::BudgetExceeded {
                site: PollSite::BuiltinChunk(BuiltinKind::RegExp),
                overshoot: 20,
                ..
            }
        ));
    }

    #[test]
    fn comparator_callbacks_share_the_meter_and_poll_before_and_after() {
        let mut meter = BudgetMeter::new(5, 2).expect("valid meter");
        let error = run_comparator_call(&mut meter, |shared_meter| {
            shared_meter.record_work(2)?;
            shared_meter.poll(PollSite::LoopBackEdge)?;
            shared_meter.record_work(2)?;
            shared_meter.poll(PollSite::LoopBackEdge)?;
            shared_meter.record_work(2)?;
            shared_meter.poll(PollSite::LoopBackEdge)
        })
        .expect_err("comparator exceeds shared budget");

        assert!(matches!(
            error,
            PollError::BudgetExceeded {
                site: PollSite::LoopBackEdge,
                limit: 5,
                observed: 6,
                overshoot: 1
            }
        ));
        assert_eq!(meter.poll_trace()[0], PollSite::CallBoundary);
    }

    #[test]
    fn released_persistent_handle_is_stale_and_no_longer_roots_its_target() {
        let mut heap = GcHeap::default();
        let reference = heap.allocate();
        let mut handles = PersistentHandles::default();
        let handle = handles.retain(&heap, reference).expect("retained");
        assert_eq!(handles.resolve(handle), Ok(reference));
        handles.release(handle).expect("released");
        assert_eq!(handles.resolve(handle), Err(FrameError::InvalidFrame));
        let report = heap.collect(handles.roots()).expect("collects");
        assert_eq!(report.objects_reclaimed, 1);
        assert!(!heap.is_alive(reference));
    }
}
