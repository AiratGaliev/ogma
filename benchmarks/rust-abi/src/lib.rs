//! M0 proof of the versioned Rust embedding boundary.
//! This is a mock value store and callback dispatcher, not an ECMAScript runtime.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, ThreadId};

const ABI_MAJOR: u16 = 1;
const ABI_MINOR: u16 = 0;
const ABI_PATCH: u16 = 0;
const MAX_ARGUMENTS: usize = 4096;
const MAX_BULK_ITEMS: usize = 1_048_576;
const MAX_REENTRANCY_DEPTH: u32 = 32;
const ABI_FEATURES: u64 = FEATURE_OPAQUE_HANDLES
    | FEATURE_ROOT_SCOPES
    | FEATURE_PERSISTENT_HANDLES
    | FEATURE_CALLBACKS
    | FEATURE_COMPLETIONS
    | FEATURE_MOCK_PROOF;
static NEXT_RUNTIME_ID: AtomicU64 = AtomicU64::new(1);

pub const FEATURE_OPAQUE_HANDLES: u64 = 1 << 0;
pub const FEATURE_ROOT_SCOPES: u64 = 1 << 1;
pub const FEATURE_PERSISTENT_HANDLES: u64 = 1 << 2;
pub const FEATURE_CALLBACKS: u64 = 1 << 3;
pub const FEATURE_COMPLETIONS: u64 = 1 << 4;
pub const FEATURE_MOCK_PROOF: u64 = 1 << 63;

#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OgmaStatus(u32);
impl OgmaStatus {
    pub const OK: Self = Self(0);
    pub const INVALID_ARGUMENT: Self = Self(1);
    pub const INCOMPATIBLE_ABI: Self = Self(2);
    pub const UNSUPPORTED_FEATURE: Self = Self(3);
    pub const WRONG_THREAD: Self = Self(4);
    pub const STALE_HANDLE: Self = Self(5);
    pub const TYPE_MISMATCH: Self = Self(6);
    pub const OUT_OF_RANGE: Self = Self(7);
    pub const REENTRANCY_LIMIT: Self = Self(8);
    pub const CALLBACK_ERROR: Self = Self(9);
    pub const INVALID_COMPLETION: Self = Self(10);
    pub const BUSY: Self = Self(11);
    pub const PANIC: Self = Self(12);
    pub const INTERNAL_ERROR: Self = Self(13);

    fn is_known(self) -> bool {
        self.0 <= Self::INTERNAL_ERROR.0
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OgmaAbiVersion {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
    pub reserved: u16,
    pub struct_size: u32,
    pub reserved2: u32,
    pub features: u64,
}

trait OpaqueToken: Copy {
    fn words(self) -> [u64; 5];
    fn from_words(words: [u64; 5]) -> Self;
}

macro_rules! define_token {
    ($name:ident) => {
        #[repr(C)]
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
        pub struct $name {
            opaque: [u64; 5],
        }
        impl OpaqueToken for $name {
            fn words(self) -> [u64; 5] {
                self.opaque
            }
            fn from_words(opaque: [u64; 5]) -> Self {
                Self { opaque }
            }
        }
        impl $name {
            fn is_zero(self) -> bool {
                self.opaque == [0; 5]
            }
        }
    };
}
define_token!(OgmaContextHandle);
define_token!(OgmaHandle);
define_token!(OgmaRootScopeHandle);
define_token!(OgmaPersistentHandle);
define_token!(OgmaCallbackHandle);

pub const COMPLETION_NORMAL: u32 = 0;
pub const COMPLETION_THROW: u32 = 1;
pub const COMPLETION_RETURN: u32 = 2;
pub const COMPLETION_BREAK: u32 = 3;
pub const COMPLETION_CONTINUE: u32 = 4;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OgmaCompletion {
    pub kind: u32,
    pub reserved: u32,
    pub value: OgmaHandle,
}
impl OgmaCompletion {
    fn normal(value: OgmaHandle) -> Self {
        Self {
            kind: COMPLETION_NORMAL,
            reserved: 0,
            value,
        }
    }
}
#[cfg(test)]
fn completion_is_empty(c: OgmaCompletion) -> bool {
    c.kind == COMPLETION_NORMAL && c.reserved == 0 && c.value.is_zero()
}

/// Rust callbacks use C-unwind so a Rust panic can be caught at the boundary.
/// C callbacks must return normally and must not unwind through this ABI.
pub type OgmaHostCallback = unsafe extern "C-unwind" fn(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    args: *const OgmaHandle,
    args_len: usize,
    completion: *mut OgmaCompletion,
    user_data: *mut c_void,
) -> OgmaStatus;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TokenId {
    index: usize,
    generation: u64,
}
struct ArenaSlot<T> {
    generation: u64,
    value: Option<T>,
}

fn arena_insert<T>(slots: &mut Vec<ArenaSlot<T>>, value: T) -> Result<TokenId, OgmaStatus> {
    for (index, slot) in slots.iter_mut().enumerate() {
        if slot.value.is_none() && slot.generation != u64::MAX {
            slot.value = Some(value);
            return Ok(TokenId {
                index,
                generation: slot.generation,
            });
        }
    }
    let index = slots.len();
    slots.push(ArenaSlot {
        generation: 1,
        value: Some(value),
    });
    Ok(TokenId {
        index,
        generation: 1,
    })
}
fn arena_get<T>(slots: &[ArenaSlot<T>], id: TokenId) -> Result<&T, OgmaStatus> {
    let slot = slots.get(id.index).ok_or(OgmaStatus::STALE_HANDLE)?;
    if slot.generation != id.generation {
        return Err(OgmaStatus::STALE_HANDLE);
    }
    slot.value.as_ref().ok_or(OgmaStatus::STALE_HANDLE)
}
fn arena_get_mut<T>(slots: &mut [ArenaSlot<T>], id: TokenId) -> Result<&mut T, OgmaStatus> {
    let slot = slots.get_mut(id.index).ok_or(OgmaStatus::STALE_HANDLE)?;
    if slot.generation != id.generation {
        return Err(OgmaStatus::STALE_HANDLE);
    }
    slot.value.as_mut().ok_or(OgmaStatus::STALE_HANDLE)
}
fn arena_remove<T>(slots: &mut [ArenaSlot<T>], id: TokenId) -> Result<T, OgmaStatus> {
    let slot = slots.get_mut(id.index).ok_or(OgmaStatus::STALE_HANDLE)?;
    if slot.generation != id.generation {
        return Err(OgmaStatus::STALE_HANDLE);
    }
    let value = slot.value.take().ok_or(OgmaStatus::STALE_HANDLE)?;
    if slot.generation != u64::MAX {
        slot.generation += 1;
    }
    Ok(value)
}

#[derive(Clone)]
enum MockValue {
    Number(f64),
    Boolean(bool),
    Function(TokenId),
}
struct ValueSlot {
    generation: u64,
    value: Option<MockValue>,
}
#[derive(Default)]
struct Heap {
    slots: Vec<ValueSlot>,
}
impl Heap {
    fn allocate(&mut self, value: MockValue) -> Result<TokenId, OgmaStatus> {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if slot.value.is_none() && slot.generation != u64::MAX {
                slot.value = Some(value);
                return Ok(TokenId {
                    index,
                    generation: slot.generation,
                });
            }
        }
        let index = self.slots.len();
        self.slots.push(ValueSlot {
            generation: 1,
            value: Some(value),
        });
        Ok(TokenId {
            index,
            generation: 1,
        })
    }
    fn get(&self, id: TokenId) -> Result<&MockValue, OgmaStatus> {
        let slot = self.slots.get(id.index).ok_or(OgmaStatus::STALE_HANDLE)?;
        if slot.generation != id.generation {
            return Err(OgmaStatus::STALE_HANDLE);
        }
        slot.value.as_ref().ok_or(OgmaStatus::STALE_HANDLE)
    }
    fn collect(&mut self, roots: &HashSet<TokenId>) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            let id = TokenId {
                index,
                generation: slot.generation,
            };
            if slot.value.is_some() && !roots.contains(&id) {
                slot.value = None;
                if slot.generation != u64::MAX {
                    slot.generation += 1;
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct CallbackEntry {
    callback: OgmaHostCallback,
    user_data: *mut c_void,
}

struct ContextState {
    index: u64,
    generation: u64,
    heap: RefCell<Heap>,
    scopes: RefCell<Vec<ArenaSlot<Vec<OgmaHandle>>>>,
    scope_stack: RefCell<Vec<TokenId>>,
    persistents: RefCell<Vec<ArenaSlot<OgmaHandle>>>,
    callbacks: RefCell<Vec<ArenaSlot<CallbackEntry>>>,
    call_roots: RefCell<Vec<Vec<OgmaHandle>>>,
    call_depth: Cell<u32>,
}
impl ContextState {
    fn new(index: usize, generation: u64) -> Self {
        Self {
            index: index as u64,
            generation,
            heap: RefCell::new(Heap::default()),
            scopes: RefCell::new(Vec::new()),
            scope_stack: RefCell::new(Vec::new()),
            persistents: RefCell::new(Vec::new()),
            callbacks: RefCell::new(Vec::new()),
            call_roots: RefCell::new(Vec::new()),
            call_depth: Cell::new(0),
        }
    }
    fn context_handle(&self, runtime: u64) -> OgmaContextHandle {
        OgmaContextHandle::from_words([runtime, self.index, self.generation, 0, 0])
    }
    fn value_handle(&self, runtime: u64, id: TokenId) -> OgmaHandle {
        OgmaHandle::from_words([
            runtime,
            self.index,
            self.generation,
            id.index as u64,
            id.generation,
        ])
    }
    fn token_handle<T: OpaqueToken>(&self, runtime: u64, id: TokenId) -> T {
        T::from_words([
            runtime,
            self.index,
            self.generation,
            id.index as u64,
            id.generation,
        ])
    }
    fn token_id<T: OpaqueToken>(&self, runtime: u64, token: T) -> Result<TokenId, OgmaStatus> {
        let w = token.words();
        if w[0] != runtime || w[1] != self.index || w[2] != self.generation || w[4] == 0 {
            return Err(OgmaStatus::STALE_HANDLE);
        }
        Ok(TokenId {
            index: usize::try_from(w[3]).map_err(|_| OgmaStatus::STALE_HANDLE)?,
            generation: w[4],
        })
    }
    fn read_value(&self, runtime: u64, handle: OgmaHandle) -> Result<MockValue, OgmaStatus> {
        let id = self.token_id(runtime, handle)?;
        self.heap.borrow().get(id).cloned()
    }
    fn read_number(&self, runtime: u64, handle: OgmaHandle) -> Result<f64, OgmaStatus> {
        match self.read_value(runtime, handle)? {
            MockValue::Number(value) => Ok(value),
            MockValue::Boolean(_) | MockValue::Function(_) => Err(OgmaStatus::TYPE_MISMATCH),
        }
    }
    fn collect(&self, runtime: u64) {
        let mut roots = HashSet::new();
        for slot in self.scopes.borrow().iter() {
            if let Some(values) = &slot.value {
                for value in values {
                    if let Ok(id) = self.token_id(runtime, *value) {
                        roots.insert(id);
                    }
                }
            }
        }
        for slot in self.persistents.borrow().iter() {
            if let Some(value) = slot.value {
                if let Ok(id) = self.token_id(runtime, value) {
                    roots.insert(id);
                }
            }
        }
        for frame in self.call_roots.borrow().iter() {
            for value in frame {
                if let Ok(id) = self.token_id(runtime, *value) {
                    roots.insert(id);
                }
            }
        }
        self.heap.borrow_mut().collect(&roots);
    }
}

struct ContextSlot {
    generation: u64,
    state: Option<Rc<ContextState>>,
}
pub struct OgmaRuntime {
    id: u64,
    owner_thread: ThreadId,
    contexts: RefCell<Vec<ContextSlot>>,
    active_calls: Cell<u32>,
    _thread_affinity: PhantomData<Rc<()>>,
}
struct ApiGuard<'a> {
    runtime: &'a OgmaRuntime,
}
impl Drop for ApiGuard<'_> {
    fn drop(&mut self) {
        self.runtime
            .active_calls
            .set(self.runtime.active_calls.get().saturating_sub(1));
    }
}
impl OgmaRuntime {
    fn new() -> Result<Box<Self>, OgmaStatus> {
        let id = NEXT_RUNTIME_ID
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
            .map_err(|_| OgmaStatus::INTERNAL_ERROR)?;
        Ok(Box::new(Self {
            id,
            owner_thread: thread::current().id(),
            contexts: RefCell::new(Vec::new()),
            active_calls: Cell::new(0),
            _thread_affinity: PhantomData,
        }))
    }
    fn enter(&self) -> Result<ApiGuard<'_>, OgmaStatus> {
        if thread::current().id() != self.owner_thread {
            return Err(OgmaStatus::WRONG_THREAD);
        }
        let n = self.active_calls.get();
        if n == u32::MAX {
            return Err(OgmaStatus::BUSY);
        }
        self.active_calls.set(n + 1);
        Ok(ApiGuard { runtime: self })
    }
    fn create_context(&self) -> Result<OgmaContextHandle, OgmaStatus> {
        let mut slots = self.contexts.borrow_mut();
        for (index, slot) in slots.iter_mut().enumerate() {
            if slot.state.is_none() && slot.generation != u64::MAX {
                let state = Rc::new(ContextState::new(index, slot.generation));
                slot.state = Some(state.clone());
                return Ok(state.context_handle(self.id));
            }
        }
        let index = slots.len();
        let state = Rc::new(ContextState::new(index, 1));
        slots.push(ContextSlot {
            generation: 1,
            state: Some(state.clone()),
        });
        Ok(state.context_handle(self.id))
    }
    fn context(&self, handle: OgmaContextHandle) -> Result<Rc<ContextState>, OgmaStatus> {
        let w = handle.words();
        if w[0] != self.id || w[3] != 0 || w[4] != 0 {
            return Err(OgmaStatus::STALE_HANDLE);
        }
        let index = usize::try_from(w[1]).map_err(|_| OgmaStatus::STALE_HANDLE)?;
        let slots = self.contexts.borrow();
        let slot = slots.get(index).ok_or(OgmaStatus::STALE_HANDLE)?;
        if slot.generation != w[2] {
            return Err(OgmaStatus::STALE_HANDLE);
        }
        slot.state.clone().ok_or(OgmaStatus::STALE_HANDLE)
    }
    fn destroy_context(&self, handle: OgmaContextHandle) -> Result<(), OgmaStatus> {
        let state = self.context(handle)?;
        if state.call_depth.get() != 0 || !state.scope_stack.borrow().is_empty() {
            return Err(OgmaStatus::BUSY);
        }
        let mut slots = self.contexts.borrow_mut();
        let slot = slots
            .get_mut(state.index as usize)
            .ok_or(OgmaStatus::STALE_HANDLE)?;
        if slot.generation != state.generation || slot.state.is_none() {
            return Err(OgmaStatus::STALE_HANDLE);
        }
        slot.state = None;
        if slot.generation != u64::MAX {
            slot.generation += 1;
        }
        Ok(())
    }
}
struct CallDepthGuard<'a>(&'a ContextState);
impl Drop for CallDepthGuard<'_> {
    fn drop(&mut self) {
        self.0
            .call_depth
            .set(self.0.call_depth.get().saturating_sub(1));
    }
}
struct CallRootGuard(Rc<ContextState>);
impl Drop for CallRootGuard {
    fn drop(&mut self) {
        let _ = self.0.call_roots.borrow_mut().pop();
    }
}
fn guarded(f: impl FnOnce() -> OgmaStatus) -> OgmaStatus {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(OgmaStatus::PANIC)
}
unsafe fn with_runtime(
    runtime: *const OgmaRuntime,
    f: impl FnOnce(&OgmaRuntime) -> OgmaStatus,
) -> OgmaStatus {
    if runtime.is_null() {
        return OgmaStatus::INVALID_ARGUMENT;
    }
    // SAFETY: caller must pass a live pointer returned by ogma_runtime_create.
    let runtime = unsafe { &*runtime };
    let _guard = match runtime.enter() {
        Ok(g) => g,
        Err(status) => return status,
    };
    f(runtime)
}
fn create_value(
    runtime: &OgmaRuntime,
    context: &ContextState,
    value: MockValue,
) -> Result<OgmaHandle, OgmaStatus> {
    let id = context.heap.borrow_mut().allocate(value)?;
    Ok(context.value_handle(runtime.id, id))
}
fn decode_scope(
    runtime: &OgmaRuntime,
    context: &ContextState,
    h: OgmaRootScopeHandle,
) -> Result<TokenId, OgmaStatus> {
    context.token_id(runtime.id, h)
}
fn decode_persistent(
    runtime: &OgmaRuntime,
    context: &ContextState,
    h: OgmaPersistentHandle,
) -> Result<TokenId, OgmaStatus> {
    context.token_id(runtime.id, h)
}
fn decode_callback(
    runtime: &OgmaRuntime,
    context: &ContextState,
    h: OgmaCallbackHandle,
) -> Result<TokenId, OgmaStatus> {
    context.token_id(runtime.id, h)
}
fn validate_completion(
    runtime: &OgmaRuntime,
    context: &ContextState,
    c: OgmaCompletion,
) -> Result<(), OgmaStatus> {
    if c.reserved != 0 || c.kind > COMPLETION_CONTINUE {
        return Err(OgmaStatus::INVALID_COMPLETION);
    }
    if !c.value.is_zero() {
        context.read_value(runtime.id, c.value)?;
    } else if c.kind == COMPLETION_THROW {
        return Err(OgmaStatus::INVALID_COMPLETION);
    }
    Ok(())
}

#[no_mangle]
pub extern "C" fn ogma_abi_version() -> OgmaAbiVersion {
    OgmaAbiVersion {
        major: ABI_MAJOR,
        minor: ABI_MINOR,
        patch: ABI_PATCH,
        reserved: 0,
        struct_size: std::mem::size_of::<OgmaAbiVersion>() as u32,
        reserved2: 0,
        features: ABI_FEATURES,
    }
}

/// # Safety
/// out_version must point to writable storage for one OgmaAbiVersion.
#[no_mangle]
pub unsafe extern "C" fn ogma_abi_negotiate(
    required_major: u16,
    minimum_minor: u16,
    required_features: u64,
    out_version: *mut OgmaAbiVersion,
) -> OgmaStatus {
    guarded(|| {
        if out_version.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        if required_major != ABI_MAJOR || minimum_minor > ABI_MINOR {
            return OgmaStatus::INCOMPATIBLE_ABI;
        }
        if required_features & !ABI_FEATURES != 0 {
            return OgmaStatus::UNSUPPORTED_FEATURE;
        }
        // SAFETY: checked non-null; caller guarantees writable output.
        unsafe {
            out_version.write(ogma_abi_version());
        }
        OgmaStatus::OK
    })
}

/// # Safety
/// out_runtime must point to writable storage for an OgmaRuntime pointer.
#[no_mangle]
pub unsafe extern "C" fn ogma_runtime_create(out_runtime: *mut *mut OgmaRuntime) -> OgmaStatus {
    guarded(|| {
        if out_runtime.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        unsafe {
            out_runtime.write(ptr::null_mut());
        }
        match OgmaRuntime::new() {
            Ok(runtime) => {
                unsafe {
                    out_runtime.write(Box::into_raw(runtime));
                }
                OgmaStatus::OK
            }
            Err(status) => status,
        }
    })
}

/// # Safety
/// runtime_slot must be writable storage containing the unique pointer returned by runtime_create.
#[no_mangle]
pub unsafe extern "C" fn ogma_runtime_destroy(runtime_slot: *mut *mut OgmaRuntime) -> OgmaStatus {
    guarded(|| {
        if runtime_slot.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        let raw = unsafe { runtime_slot.read() };
        if raw.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        // SAFETY: pointer is the live Box::into_raw result provided by this API.
        let runtime = unsafe { &*raw };
        if thread::current().id() != runtime.owner_thread {
            return OgmaStatus::WRONG_THREAD;
        }
        if runtime.active_calls.get() != 0 {
            return OgmaStatus::BUSY;
        }
        unsafe {
            drop(Box::from_raw(raw));
        }
        unsafe {
            runtime_slot.write(ptr::null_mut());
        }
        OgmaStatus::OK
    })
}

/// # Safety
/// runtime must be live and out_context must point to writable storage.
#[no_mangle]
pub unsafe extern "C" fn ogma_context_create(
    runtime: *const OgmaRuntime,
    out_context: *mut OgmaContextHandle,
) -> OgmaStatus {
    guarded(|| {
        if out_context.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        unsafe {
            with_runtime(runtime, |r| match r.create_context() {
                Ok(h) => {
                    out_context.write(h);
                    OgmaStatus::OK
                }
                Err(e) => e,
            })
        }
    })
}

/// # Safety
/// runtime must be live and the context handle must have been returned by that runtime.
#[no_mangle]
pub unsafe extern "C" fn ogma_context_destroy(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
) -> OgmaStatus {
    guarded(|| unsafe {
        with_runtime(runtime, |r| match r.destroy_context(context) {
            Ok(()) => OgmaStatus::OK,
            Err(e) => e,
        })
    })
}

/// # Safety
/// runtime must be live and out_value must point to writable storage.
#[no_mangle]
pub unsafe extern "C" fn ogma_context_create_number(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    value: f64,
    out_value: *mut OgmaHandle,
) -> OgmaStatus {
    guarded(|| {
        if out_value.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        unsafe {
            with_runtime(runtime, |r| {
                let c = match r.context(context) {
                    Ok(c) => c,
                    Err(e) => return e,
                };
                match create_value(r, &c, MockValue::Number(value)) {
                    Ok(h) => {
                        out_value.write(h);
                        OgmaStatus::OK
                    }
                    Err(e) => e,
                }
            })
        }
    })
}

/// # Safety
/// runtime must be live, value must be 0 or 1, and out_value must point to writable storage.
#[no_mangle]
pub unsafe extern "C" fn ogma_context_create_boolean(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    value: u8,
    out_value: *mut OgmaHandle,
) -> OgmaStatus {
    guarded(|| {
        if out_value.is_null() || value > 1 {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        unsafe {
            with_runtime(runtime, |r| {
                let c = match r.context(context) {
                    Ok(c) => c,
                    Err(e) => return e,
                };
                match create_value(r, &c, MockValue::Boolean(value != 0)) {
                    Ok(h) => {
                        out_value.write(h);
                        OgmaStatus::OK
                    }
                    Err(e) => e,
                }
            })
        }
    })
}

/// # Safety
/// runtime must be live and out_value must point to writable storage.
#[no_mangle]
pub unsafe extern "C" fn ogma_context_get_number(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    handle: OgmaHandle,
    out_value: *mut f64,
) -> OgmaStatus {
    guarded(|| {
        if out_value.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        unsafe {
            with_runtime(runtime, |r| {
                let c = match r.context(context) {
                    Ok(c) => c,
                    Err(e) => return e,
                };
                match c.read_number(r.id, handle) {
                    Ok(v) => {
                        out_value.write(v);
                        OgmaStatus::OK
                    }
                    Err(e) => e,
                }
            })
        }
    })
}

/// # Safety
/// runtime must be live and out_value must point to writable storage.
#[no_mangle]
pub unsafe extern "C" fn ogma_context_get_boolean(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    handle: OgmaHandle,
    out_value: *mut u8,
) -> OgmaStatus {
    guarded(|| {
        if out_value.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        unsafe {
            with_runtime(runtime, |r| {
                let c = match r.context(context) {
                    Ok(c) => c,
                    Err(e) => return e,
                };
                match c.read_value(r.id, handle) {
                    Ok(MockValue::Boolean(v)) => {
                        out_value.write(u8::from(v));
                        OgmaStatus::OK
                    }
                    Ok(MockValue::Number(_) | MockValue::Function(_)) => OgmaStatus::TYPE_MISMATCH,
                    Err(e) => e,
                }
            })
        }
    })
}

/// # Safety
/// runtime must be live and out_value must point to writable storage.
#[no_mangle]
pub unsafe extern "C" fn ogma_context_get_i32(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    handle: OgmaHandle,
    out_value: *mut i32,
) -> OgmaStatus {
    guarded(|| {
        if out_value.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        unsafe {
            with_runtime(runtime, |r| {
                let c = match r.context(context) {
                    Ok(c) => c,
                    Err(e) => return e,
                };
                let n = match c.read_number(r.id, handle) {
                    Ok(n) => n,
                    Err(e) => return e,
                };
                if !n.is_finite()
                    || n.fract() != 0.0
                    || !(i32::MIN as f64..=i32::MAX as f64).contains(&n)
                {
                    return OgmaStatus::OUT_OF_RANGE;
                }
                out_value.write(n as i32);
                OgmaStatus::OK
            })
        }
    })
}

/// # Safety
/// out_value must point to writable storage.
#[no_mangle]
pub unsafe extern "C" fn ogma_primitive_add_f64(
    left: f64,
    right: f64,
    out_value: *mut f64,
) -> OgmaStatus {
    guarded(|| {
        if out_value.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        unsafe {
            out_value.write(left + right);
        }
        OgmaStatus::OK
    })
}

/// # Safety
/// runtime must be live. If count is non-zero, handles must point to count readable handles;
/// out_sum must point to writable storage.
#[no_mangle]
pub unsafe extern "C" fn ogma_context_sum_numbers(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    handles: *const OgmaHandle,
    count: usize,
    out_sum: *mut f64,
) -> OgmaStatus {
    guarded(|| {
        if out_sum.is_null() || count > MAX_BULK_ITEMS || (count > 0 && handles.is_null()) {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        unsafe {
            with_runtime(runtime, |r| {
                let c = match r.context(context) {
                    Ok(c) => c,
                    Err(e) => return e,
                };
                let input = if count == 0 {
                    &[][..]
                } else {
                    // SAFETY: caller guarantees a readable array of count handles.
                    std::slice::from_raw_parts(handles, count)
                };
                let mut sum = 0.0;
                for h in input {
                    match c.read_number(r.id, *h) {
                        Ok(v) => sum += v,
                        Err(e) => return e,
                    }
                }
                out_sum.write(sum);
                OgmaStatus::OK
            })
        }
    })
}

/// # Safety
/// runtime must be live and out_scope must point to writable storage.
#[no_mangle]
pub unsafe extern "C" fn ogma_root_scope_open(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    out_scope: *mut OgmaRootScopeHandle,
) -> OgmaStatus {
    guarded(|| {
        if out_scope.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        unsafe {
            with_runtime(runtime, |r| {
                let c = match r.context(context) {
                    Ok(c) => c,
                    Err(e) => return e,
                };
                let id = match arena_insert(&mut c.scopes.borrow_mut(), Vec::new()) {
                    Ok(id) => id,
                    Err(e) => return e,
                };
                c.scope_stack.borrow_mut().push(id);
                out_scope.write(c.token_handle(r.id, id));
                OgmaStatus::OK
            })
        }
    })
}

/// # Safety
/// runtime must be live and scope/value handles must originate from this runtime/context.
#[no_mangle]
pub unsafe extern "C" fn ogma_root_scope_add(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    scope: OgmaRootScopeHandle,
    value: OgmaHandle,
) -> OgmaStatus {
    guarded(|| unsafe {
        with_runtime(runtime, |r| {
            let c = match r.context(context) {
                Ok(c) => c,
                Err(e) => return e,
            };
            let id = match decode_scope(r, &c, scope) {
                Ok(id) => id,
                Err(e) => return e,
            };
            if !c.scope_stack.borrow().contains(&id) {
                return OgmaStatus::STALE_HANDLE;
            }
            if let Err(e) = c.read_value(r.id, value) {
                return e;
            }
            let mut scopes = c.scopes.borrow_mut();
            let roots = match arena_get_mut(&mut scopes, id) {
                Ok(roots) => roots,
                Err(e) => return e,
            };
            if !roots.contains(&value) {
                roots.push(value);
            }
            OgmaStatus::OK
        })
    })
}

/// # Safety
/// runtime must be live and scope must be a handle returned for this runtime/context.
#[no_mangle]
pub unsafe extern "C" fn ogma_root_scope_close(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    scope: OgmaRootScopeHandle,
) -> OgmaStatus {
    guarded(|| unsafe {
        with_runtime(runtime, |r| {
            let c = match r.context(context) {
                Ok(c) => c,
                Err(e) => return e,
            };
            let id = match decode_scope(r, &c, scope) {
                Ok(id) => id,
                Err(e) => return e,
            };
            if c.scope_stack.borrow().last().copied() != Some(id) {
                return OgmaStatus::BUSY;
            }
            if let Err(e) = arena_remove(&mut c.scopes.borrow_mut(), id) {
                return e;
            }
            c.scope_stack.borrow_mut().pop();
            OgmaStatus::OK
        })
    })
}

/// # Safety
/// runtime must be live; value must be a handle from this context; out_persistent must be writable.
#[no_mangle]
pub unsafe extern "C" fn ogma_persistent_create(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    value: OgmaHandle,
    out_persistent: *mut OgmaPersistentHandle,
) -> OgmaStatus {
    guarded(|| {
        if out_persistent.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        unsafe {
            with_runtime(runtime, |r| {
                let c = match r.context(context) {
                    Ok(c) => c,
                    Err(e) => return e,
                };
                if let Err(e) = c.read_value(r.id, value) {
                    return e;
                }
                let id = match arena_insert(&mut c.persistents.borrow_mut(), value) {
                    Ok(id) => id,
                    Err(e) => return e,
                };
                out_persistent.write(c.token_handle(r.id, id));
                OgmaStatus::OK
            })
        }
    })
}

/// # Safety
/// runtime must be live and out_value must point to writable storage.
#[no_mangle]
pub unsafe extern "C" fn ogma_persistent_get(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    persistent: OgmaPersistentHandle,
    out_value: *mut OgmaHandle,
) -> OgmaStatus {
    guarded(|| {
        if out_value.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        unsafe {
            with_runtime(runtime, |r| {
                let c = match r.context(context) {
                    Ok(c) => c,
                    Err(e) => return e,
                };
                let id = match decode_persistent(r, &c, persistent) {
                    Ok(id) => id,
                    Err(e) => return e,
                };
                let value = match arena_get(&c.persistents.borrow(), id) {
                    Ok(v) => *v,
                    Err(e) => return e,
                };
                if let Err(e) = c.read_value(r.id, value) {
                    return e;
                }
                out_value.write(value);
                OgmaStatus::OK
            })
        }
    })
}

/// # Safety
/// runtime must be live and persistent must be a handle returned by this runtime/context.
#[no_mangle]
pub unsafe extern "C" fn ogma_persistent_release(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    persistent: OgmaPersistentHandle,
) -> OgmaStatus {
    guarded(|| unsafe {
        with_runtime(runtime, |r| {
            let c = match r.context(context) {
                Ok(c) => c,
                Err(e) => return e,
            };
            let id = match decode_persistent(r, &c, persistent) {
                Ok(id) => id,
                Err(e) => return e,
            };
            let result = {
                let mut arena = c.persistents.borrow_mut();

                arena_remove(&mut arena, id)
            };

            match result {
                Ok(_) => OgmaStatus::OK,

                Err(e) => e,
            }
        })
    })
}

/// # Safety
/// runtime must be live and context must belong to it.
#[no_mangle]
pub unsafe extern "C" fn ogma_context_collect_unrooted(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
) -> OgmaStatus {
    guarded(|| unsafe {
        with_runtime(runtime, |r| {
            let c = match r.context(context) {
                Ok(c) => c,
                Err(e) => return e,
            };
            c.collect(r.id);
            OgmaStatus::OK
        })
    })
}

/// # Safety
/// runtime must be live; callback must be valid for future calls; user_data must remain valid until unregister.
#[no_mangle]
pub unsafe extern "C" fn ogma_callback_register(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    callback: Option<OgmaHostCallback>,
    user_data: *mut c_void,
    out_callback: *mut OgmaCallbackHandle,
) -> OgmaStatus {
    guarded(|| {
        let Some(callback) = callback else {
            return OgmaStatus::INVALID_ARGUMENT;
        };
        if out_callback.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        unsafe {
            with_runtime(runtime, |r| {
                let c = match r.context(context) {
                    Ok(c) => c,
                    Err(e) => return e,
                };
                let id = match arena_insert(
                    &mut c.callbacks.borrow_mut(),
                    CallbackEntry {
                        callback,
                        user_data,
                    },
                ) {
                    Ok(id) => id,
                    Err(e) => return e,
                };
                out_callback.write(c.token_handle(r.id, id));
                OgmaStatus::OK
            })
        }
    })
}

/// # Safety
/// runtime must be live and callback must be a registration from this runtime/context.
#[no_mangle]
pub unsafe extern "C" fn ogma_callback_unregister(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    callback: OgmaCallbackHandle,
) -> OgmaStatus {
    guarded(|| unsafe {
        with_runtime(runtime, |r| {
            let c = match r.context(context) {
                Ok(c) => c,
                Err(e) => return e,
            };
            let id = match decode_callback(r, &c, callback) {
                Ok(id) => id,
                Err(e) => return e,
            };
            let result = {
                let mut arena = c.callbacks.borrow_mut();

                arena_remove(&mut arena, id)
            };

            match result {
                Ok(_) => OgmaStatus::OK,

                Err(e) => e,
            }
        })
    })
}

/// # Safety
/// runtime must be live and out_function must point to writable storage.
#[no_mangle]
pub unsafe extern "C" fn ogma_mock_function_create(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    callback: OgmaCallbackHandle,
    out_function: *mut OgmaHandle,
) -> OgmaStatus {
    guarded(|| {
        if out_function.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        unsafe {
            with_runtime(runtime, |r| {
                let c = match r.context(context) {
                    Ok(c) => c,
                    Err(e) => return e,
                };
                let callback_id = match decode_callback(r, &c, callback) {
                    Ok(id) => id,
                    Err(e) => return e,
                };
                if let Err(e) = arena_get(&c.callbacks.borrow(), callback_id) {
                    return e;
                }
                match create_value(r, &c, MockValue::Function(callback_id)) {
                    Ok(h) => {
                        out_function.write(h);
                        OgmaStatus::OK
                    }
                    Err(e) => e,
                }
            })
        }
    })
}

/// # Safety
/// runtime must be live; args must address args_len readable handles; completion must be writable.
/// The argument array is borrowed only for this call. A callback must persist any value it retains.
#[no_mangle]
pub unsafe extern "C" fn ogma_context_call(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    function: OgmaHandle,
    args: *const OgmaHandle,
    args_len: usize,
    completion: *mut OgmaCompletion,
) -> OgmaStatus {
    guarded(|| {
        if completion.is_null() || args_len > MAX_ARGUMENTS || (args_len > 0 && args.is_null()) {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        unsafe {
            completion.write(OgmaCompletion::default());
        }
        unsafe {
            with_runtime(runtime, |r| {
                let c = match r.context(context) {
                    Ok(c) => c,
                    Err(e) => return e,
                };
                let callback_id = match c.read_value(r.id, function) {
                    Ok(MockValue::Function(id)) => id,
                    Ok(MockValue::Number(_) | MockValue::Boolean(_)) => {
                        return OgmaStatus::TYPE_MISMATCH
                    }
                    Err(e) => return e,
                };
                let callback_entry = match arena_get(&c.callbacks.borrow(), callback_id) {
                    Ok(entry) => *entry,
                    Err(e) => return e,
                };
                let input = if args_len == 0 {
                    &[][..]
                } else {
                    // SAFETY: caller guarantees args_len readable elements.
                    std::slice::from_raw_parts(args, args_len)
                };
                let copied_args = input.to_vec();
                for h in &copied_args {
                    if let Err(e) = c.read_value(r.id, *h) {
                        return e;
                    }
                }
                let depth = c.call_depth.get();
                if depth >= MAX_REENTRANCY_DEPTH {
                    return OgmaStatus::REENTRANCY_LIMIT;
                }
                c.call_depth.set(depth + 1);
                let _depth_guard = CallDepthGuard(&c);
                let mut roots = Vec::with_capacity(copied_args.len() + 1);
                roots.push(function);
                roots.extend_from_slice(&copied_args);
                c.call_roots.borrow_mut().push(roots);
                let _root_guard = CallRootGuard(c.clone());
                let mut result = OgmaCompletion::default();
                let args_ptr = if copied_args.is_empty() {
                    ptr::null()
                } else {
                    copied_args.as_ptr()
                };
                // No internal borrow is held while user callback code runs.
                let status = (callback_entry.callback)(
                    r,
                    context,
                    args_ptr,
                    copied_args.len(),
                    &mut result,
                    callback_entry.user_data,
                );
                if status != OgmaStatus::OK {
                    return if status.is_known() {
                        status
                    } else {
                        OgmaStatus::CALLBACK_ERROR
                    };
                }
                if let Err(e) = validate_completion(r, &c, result) {
                    return e;
                }
                completion.write(result);
                OgmaStatus::OK
            })
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (*mut OgmaRuntime, OgmaContextHandle) {
        let mut r = ptr::null_mut();
        assert_eq!(unsafe { ogma_runtime_create(&mut r) }, OgmaStatus::OK);
        let mut c = OgmaContextHandle::default();
        assert_eq!(unsafe { ogma_context_create(r, &mut c) }, OgmaStatus::OK);
        (r, c)
    }
    fn cleanup(mut r: *mut OgmaRuntime) {
        assert_eq!(unsafe { ogma_runtime_destroy(&mut r) }, OgmaStatus::OK);
        assert!(r.is_null());
    }
    fn number(r: *const OgmaRuntime, c: OgmaContextHandle, n: f64) -> OgmaHandle {
        let mut h = OgmaHandle::default();
        assert_eq!(
            unsafe { ogma_context_create_number(r, c, n, &mut h) },
            OgmaStatus::OK
        );
        h
    }
    fn read_number(r: *const OgmaRuntime, c: OgmaContextHandle, h: OgmaHandle) -> f64 {
        let mut n = 0.0;
        assert_eq!(
            unsafe { ogma_context_get_number(r, c, h, &mut n) },
            OgmaStatus::OK
        );
        n
    }
    unsafe extern "C-unwind" fn identity(
        r: *const OgmaRuntime,
        c: OgmaContextHandle,
        args: *const OgmaHandle,
        n: usize,
        out: *mut OgmaCompletion,
        _data: *mut c_void,
    ) -> OgmaStatus {
        if n != 1 || args.is_null() || out.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        let h = unsafe { *args };
        let mut value = 0.0;
        let status = unsafe { ogma_context_get_number(r, c, h, &mut value) };
        if status != OgmaStatus::OK {
            return status;
        }
        let status = unsafe { ogma_context_collect_unrooted(r, c) };
        if status != OgmaStatus::OK {
            return status;
        }
        unsafe {
            out.write(OgmaCompletion::normal(h));
        }
        OgmaStatus::OK
    }
    unsafe extern "C-unwind" fn throw_value(
        _r: *const OgmaRuntime,
        _c: OgmaContextHandle,
        args: *const OgmaHandle,
        n: usize,
        out: *mut OgmaCompletion,
        _data: *mut c_void,
    ) -> OgmaStatus {
        if n != 1 || args.is_null() || out.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        unsafe {
            out.write(OgmaCompletion {
                kind: COMPLETION_THROW,
                reserved: 0,
                value: *args,
            });
        }
        OgmaStatus::OK
    }
    unsafe extern "C-unwind" fn retain_arg(
        r: *const OgmaRuntime,
        c: OgmaContextHandle,
        args: *const OgmaHandle,
        n: usize,
        out: *mut OgmaCompletion,
        data: *mut c_void,
    ) -> OgmaStatus {
        if n != 1 || args.is_null() || out.is_null() || data.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        let status =
            unsafe { ogma_persistent_create(r, c, *args, data.cast::<OgmaPersistentHandle>()) };
        if status != OgmaStatus::OK {
            return status;
        }
        unsafe {
            out.write(OgmaCompletion::normal(*args));
        }
        OgmaStatus::OK
    }

    unsafe extern "C-unwind" fn callback_error(
        _r: *const OgmaRuntime,
        _c: OgmaContextHandle,
        _a: *const OgmaHandle,
        _n: usize,
        _out: *mut OgmaCompletion,
        _data: *mut c_void,
    ) -> OgmaStatus {
        OgmaStatus::CALLBACK_ERROR
    }

    unsafe extern "C-unwind" fn panic_callback(
        _r: *const OgmaRuntime,
        _c: OgmaContextHandle,
        _a: *const OgmaHandle,
        _n: usize,
        _out: *mut OgmaCompletion,
        _data: *mut c_void,
    ) -> OgmaStatus {
        panic!("boundary must catch this panic");
    }
    unsafe extern "C-unwind" fn recurse(
        r: *const OgmaRuntime,
        c: OgmaContextHandle,
        _a: *const OgmaHandle,
        _n: usize,
        _out: *mut OgmaCompletion,
        data: *mut c_void,
    ) -> OgmaStatus {
        if data.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        let function = unsafe { *data.cast::<OgmaHandle>() };
        let mut nested = OgmaCompletion::default();
        unsafe { ogma_context_call(r, c, function, ptr::null(), 0, &mut nested) }
    }
    fn register(
        r: *const OgmaRuntime,
        c: OgmaContextHandle,
        cb: OgmaHostCallback,
        data: *mut c_void,
    ) -> (OgmaCallbackHandle, OgmaHandle) {
        let mut token = OgmaCallbackHandle::default();
        assert_eq!(
            unsafe { ogma_callback_register(r, c, Some(cb), data, &mut token) },
            OgmaStatus::OK
        );
        let mut function = OgmaHandle::default();
        assert_eq!(
            unsafe { ogma_mock_function_create(r, c, token, &mut function) },
            OgmaStatus::OK
        );
        (token, function)
    }

    #[test]
    fn abi_negotiation_is_version_and_feature_checked() {
        let mut version = OgmaAbiVersion::default();
        assert_eq!(
            unsafe { ogma_abi_negotiate(1, 0, FEATURE_OPAQUE_HANDLES, &mut version) },
            OgmaStatus::OK
        );
        assert_eq!(version.major, 1);
        assert_eq!(
            version.struct_size as usize,
            std::mem::size_of::<OgmaAbiVersion>()
        );
        assert_eq!(
            unsafe { ogma_abi_negotiate(2, 0, 0, &mut version) },
            OgmaStatus::INCOMPATIBLE_ABI
        );
        assert_eq!(
            unsafe { ogma_abi_negotiate(1, 1, 0, &mut version) },
            OgmaStatus::INCOMPATIBLE_ABI
        );
        assert_eq!(
            unsafe { ogma_abi_negotiate(1, 0, 1 << 40, &mut version) },
            OgmaStatus::UNSUPPORTED_FEATURE
        );
    }

    #[test]
    fn scopes_and_persistent_roots_control_handle_lifecycle() {
        let (r, c) = setup();
        let h = number(r, c, 42.0);
        let mut scope = OgmaRootScopeHandle::default();
        assert_eq!(
            unsafe { ogma_root_scope_open(r, c, &mut scope) },
            OgmaStatus::OK
        );
        assert_eq!(
            unsafe { ogma_root_scope_add(r, c, scope, h) },
            OgmaStatus::OK
        );
        let mut persistent = OgmaPersistentHandle::default();
        assert_eq!(
            unsafe { ogma_persistent_create(r, c, h, &mut persistent) },
            OgmaStatus::OK
        );
        assert_eq!(
            unsafe { ogma_root_scope_close(r, c, scope) },
            OgmaStatus::OK
        );
        assert_eq!(
            unsafe { ogma_context_collect_unrooted(r, c) },
            OgmaStatus::OK
        );
        let mut retained = OgmaHandle::default();
        assert_eq!(
            unsafe { ogma_persistent_get(r, c, persistent, &mut retained) },
            OgmaStatus::OK
        );
        assert_eq!(read_number(r, c, retained), 42.0);
        assert_eq!(
            unsafe { ogma_persistent_release(r, c, persistent) },
            OgmaStatus::OK
        );
        assert_eq!(
            unsafe { ogma_persistent_release(r, c, persistent) },
            OgmaStatus::STALE_HANDLE
        );
        assert_eq!(
            unsafe { ogma_context_collect_unrooted(r, c) },
            OgmaStatus::OK
        );
        let replacement = number(r, c, 17.0);
        assert_ne!(h, replacement);
        assert_eq!(read_number(r, c, replacement), 17.0);
        let mut output = 0.0;
        assert_eq!(
            unsafe { ogma_context_get_number(r, c, h, &mut output) },
            OgmaStatus::STALE_HANDLE
        );
        cleanup(r);
    }

    #[test]
    fn handle_generations_and_runtime_identity_are_validated() {
        let (a, ca) = setup();
        let (b, cb) = setup();
        let ha = number(a, ca, 9.0);
        let mut output = 0.0;
        assert_eq!(
            unsafe { ogma_context_get_number(b, cb, ha, &mut output) },
            OgmaStatus::STALE_HANDLE
        );
        assert_eq!(unsafe { ogma_context_destroy(a, ca) }, OgmaStatus::OK);
        let mut replacement = OgmaContextHandle::default();
        assert_eq!(
            unsafe { ogma_context_create(a, &mut replacement) },
            OgmaStatus::OK
        );
        assert_eq!(
            unsafe { ogma_context_get_number(a, ca, ha, &mut output) },
            OgmaStatus::STALE_HANDLE
        );
        cleanup(a);
        cleanup(b);
    }

    #[test]
    fn conversions_are_checked_and_bulk_reads_preserve_type_errors() {
        let (r, c) = setup();
        let h1 = number(r, c, -17.0);
        let h2 = number(r, c, 2.5);
        let mut boolean = OgmaHandle::default();
        assert_eq!(
            unsafe { ogma_context_create_boolean(r, c, 1, &mut boolean) },
            OgmaStatus::OK
        );
        let mut i = 0;
        assert_eq!(
            unsafe { ogma_context_get_i32(r, c, h1, &mut i) },
            OgmaStatus::OK
        );
        assert_eq!(i, -17);
        assert_eq!(
            unsafe { ogma_context_get_i32(r, c, h2, &mut i) },
            OgmaStatus::OUT_OF_RANGE
        );
        let mut sum = 0.0;
        let values = [h1, boolean];
        assert_eq!(
            unsafe { ogma_context_sum_numbers(r, c, values.as_ptr(), 2, &mut sum) },
            OgmaStatus::TYPE_MISMATCH
        );
        cleanup(r);
    }

    #[test]
    fn callback_reentrancy_roots_arguments_and_maps_throw_completion() {
        let (r, c) = setup();
        let h = number(r, c, 88.0);
        let mut scope = OgmaRootScopeHandle::default();
        assert_eq!(
            unsafe { ogma_root_scope_open(r, c, &mut scope) },
            OgmaStatus::OK
        );
        assert_eq!(
            unsafe { ogma_root_scope_add(r, c, scope, h) },
            OgmaStatus::OK
        );
        let (_token, function) = register(r, c, identity, ptr::null_mut());
        let mut completion = OgmaCompletion::default();
        assert_eq!(
            unsafe { ogma_context_call(r, c, function, &h, 1, &mut completion) },
            OgmaStatus::OK
        );
        assert_eq!(completion.kind, COMPLETION_NORMAL);
        assert_eq!(read_number(r, c, completion.value), 88.0);
        let (_throw_token, thrower) = register(r, c, throw_value, ptr::null_mut());
        assert_eq!(
            unsafe { ogma_context_call(r, c, thrower, &h, 1, &mut completion) },
            OgmaStatus::OK
        );
        assert_eq!(completion.kind, COMPLETION_THROW);
        assert_eq!(read_number(r, c, completion.value), 88.0);
        assert_eq!(
            unsafe { ogma_root_scope_close(r, c, scope) },
            OgmaStatus::OK
        );
        cleanup(r);
    }

    #[test]
    fn persistent_promotion_can_retain_a_borrowed_callback_argument() {
        let (r, c) = setup();
        let h = number(r, c, 6.25);
        let mut scope = OgmaRootScopeHandle::default();
        assert_eq!(
            unsafe { ogma_root_scope_open(r, c, &mut scope) },
            OgmaStatus::OK
        );
        assert_eq!(
            unsafe { ogma_root_scope_add(r, c, scope, h) },
            OgmaStatus::OK
        );
        let mut persistent = OgmaPersistentHandle::default();
        let (_token, function) = register(
            r,
            c,
            retain_arg,
            (&mut persistent as *mut OgmaPersistentHandle).cast(),
        );
        let mut completion = OgmaCompletion::default();
        assert_eq!(
            unsafe { ogma_context_call(r, c, function, &h, 1, &mut completion) },
            OgmaStatus::OK
        );
        assert_eq!(
            unsafe { ogma_root_scope_close(r, c, scope) },
            OgmaStatus::OK
        );
        assert_eq!(
            unsafe { ogma_context_collect_unrooted(r, c) },
            OgmaStatus::OK
        );
        let mut retained = OgmaHandle::default();
        assert_eq!(
            unsafe { ogma_persistent_get(r, c, persistent, &mut retained) },
            OgmaStatus::OK
        );
        assert_eq!(read_number(r, c, retained), 6.25);
        cleanup(r);
    }

    #[test]
    fn callback_panics_are_contained() {
        let (r, c) = setup();
        let (_token, function) = register(r, c, panic_callback, ptr::null_mut());
        let mut completion = OgmaCompletion::default();
        assert_eq!(
            unsafe { ogma_context_call(r, c, function, ptr::null(), 0, &mut completion) },
            OgmaStatus::PANIC
        );
        assert!(completion_is_empty(completion));
        cleanup(r);
    }

    #[test]
    fn unregister_and_reentrancy_depth_have_deterministic_results() {
        let (r, c) = setup();
        let (token, function) = register(r, c, identity, ptr::null_mut());
        assert_eq!(
            unsafe { ogma_callback_unregister(r, c, token) },
            OgmaStatus::OK
        );
        let mut completion = OgmaCompletion::default();
        assert_eq!(
            unsafe { ogma_context_call(r, c, function, ptr::null(), 0, &mut completion) },
            OgmaStatus::STALE_HANDLE
        );
        let (_error_token, error_function) = register(r, c, callback_error, ptr::null_mut());
        assert_eq!(
            unsafe { ogma_context_call(r, c, error_function, ptr::null(), 0, &mut completion) },
            OgmaStatus::CALLBACK_ERROR
        );
        assert!(completion_is_empty(completion));
        let mut recursive = OgmaHandle::default();
        let (_token, created) = register(r, c, recurse, (&mut recursive as *mut OgmaHandle).cast());
        recursive = created;
        assert_eq!(
            unsafe { ogma_context_call(r, c, recursive, ptr::null(), 0, &mut completion) },
            OgmaStatus::REENTRANCY_LIMIT
        );
        cleanup(r);
    }

    #[test]
    fn owner_thread_and_root_scope_stack_are_enforced() {
        let (r, c) = setup();
        let address = r as usize;
        let status = thread::spawn(move || {
            let mut h = OgmaHandle::default();
            unsafe { ogma_context_create_number(address as *const OgmaRuntime, c, 1.0, &mut h) }
        })
        .join()
        .unwrap();
        assert_eq!(status, OgmaStatus::WRONG_THREAD);
        let mut outer = OgmaRootScopeHandle::default();
        let mut inner = OgmaRootScopeHandle::default();
        assert_eq!(
            unsafe { ogma_root_scope_open(r, c, &mut outer) },
            OgmaStatus::OK
        );
        assert_eq!(
            unsafe { ogma_root_scope_open(r, c, &mut inner) },
            OgmaStatus::OK
        );
        assert_eq!(
            unsafe { ogma_root_scope_close(r, c, outer) },
            OgmaStatus::BUSY
        );
        assert_eq!(
            unsafe { ogma_root_scope_close(r, c, inner) },
            OgmaStatus::OK
        );
        assert_eq!(
            unsafe { ogma_root_scope_close(r, c, outer) },
            OgmaStatus::OK
        );
        cleanup(r);
    }

    #[test]
    fn invalid_completion_and_active_runtime_destruction_are_rejected() {
        unsafe extern "C-unwind" fn bad_completion(
            _r: *const OgmaRuntime,
            _c: OgmaContextHandle,
            _a: *const OgmaHandle,
            _n: usize,
            out: *mut OgmaCompletion,
            _data: *mut c_void,
        ) -> OgmaStatus {
            unsafe {
                out.write(OgmaCompletion {
                    kind: 99,
                    reserved: 0,
                    value: OgmaHandle::default(),
                });
            }
            OgmaStatus::OK
        }
        unsafe extern "C-unwind" fn destroy_from_callback(
            _r: *const OgmaRuntime,
            _c: OgmaContextHandle,
            _a: *const OgmaHandle,
            _n: usize,
            _out: *mut OgmaCompletion,
            data: *mut c_void,
        ) -> OgmaStatus {
            unsafe { ogma_runtime_destroy(data.cast::<*mut OgmaRuntime>()) }
        }
        let (r, c) = setup();
        let (_token, bad) = register(r, c, bad_completion, ptr::null_mut());
        let mut completion = OgmaCompletion::default();
        assert_eq!(
            unsafe { ogma_context_call(r, c, bad, ptr::null(), 0, &mut completion) },
            OgmaStatus::INVALID_COMPLETION
        );
        let mut runtime_slot = r;
        let (_token, destroy) = register(
            r,
            c,
            destroy_from_callback,
            (&mut runtime_slot as *mut *mut OgmaRuntime).cast(),
        );
        assert_eq!(
            unsafe { ogma_context_call(r, c, destroy, ptr::null(), 0, &mut completion) },
            OgmaStatus::BUSY
        );
        assert_eq!(runtime_slot, r);
        cleanup(r);
    }
}
