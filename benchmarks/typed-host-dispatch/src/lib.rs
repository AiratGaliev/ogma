//! M0 proof of typed host-function registration and guarded dispatch.
//! This is a mock harness, not a production Ogma Runtime or full ECMAScript object model.

use ogma_host_execution_scope_proof::{HostError, HostExecutionScope};
use ogma_rust_abi_proof::{
    ogma_abi_negotiate, ogma_abi_version, ogma_callback_register, ogma_callback_unregister,
    ogma_context_call, ogma_context_collect_unrooted, ogma_context_create,
    ogma_context_create_boolean, ogma_context_create_number, ogma_context_destroy,
    ogma_context_get_boolean, ogma_context_get_number, ogma_mock_function_create,
    ogma_persistent_create, ogma_persistent_release, ogma_root_scope_add, ogma_root_scope_close,
    ogma_root_scope_open, ogma_runtime_create, ogma_runtime_destroy, OgmaAbiVersion,
    OgmaCallbackHandle, OgmaCompletion, OgmaContextHandle, OgmaHandle, OgmaHostCallback,
    OgmaPersistentHandle, OgmaRootScopeHandle, OgmaRuntime, OgmaStatus, COMPLETION_NORMAL,
    COMPLETION_THROW, FEATURE_CALLBACKS, FEATURE_COMPLETIONS, FEATURE_OPAQUE_HANDLES,
    FEATURE_PERSISTENT_HANDLES, FEATURE_ROOT_SCOPES,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::ptr;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, ThreadId};

const DESCRIPTOR_VERSION: u16 = 1;
const ABI_MAJOR_REQUIRED: u16 = 1;
const ABI_MINOR_REQUIRED: u16 = 0;
const MAX_NAME_BYTES: usize = 128;
const MAX_ARGUMENTS: usize = 4096;
const KNOWN_EFFECTS: u32 = EFFECT_MAY_ALLOCATE
    | EFFECT_MAY_GC
    | EFFECT_MAY_THROW
    | EFFECT_MAY_SUSPEND
    | EFFECT_MAY_REENTER;
const COMPLEX_EFFECTS: u32 =
    EFFECT_MAY_ALLOCATE | EFFECT_MAY_GC | EFFECT_MAY_THROW | EFFECT_MAY_REENTER;
const REQUIRED_ABI_FEATURES: u64 = FEATURE_OPAQUE_HANDLES
    | FEATURE_ROOT_SCOPES
    | FEATURE_PERSISTENT_HANDLES
    | FEATURE_CALLBACKS
    | FEATURE_COMPLETIONS;

static NEXT_REGISTRY_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_OBJECT_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_FUNCTION_ID: AtomicU64 = AtomicU64::new(1);

pub const EFFECT_MAY_ALLOCATE: u32 = 1 << 0;
pub const EFFECT_MAY_GC: u32 = 1 << 1;
pub const EFFECT_MAY_THROW: u32 = 1 << 2;
pub const EFFECT_MAY_SUSPEND: u32 = 1 << 3;
pub const EFFECT_MAY_REENTER: u32 = 1 << 4;

pub const SIGNATURE_GENERIC: u64 = 0;
pub const SIGNATURE_F64_BINARY: u64 = 0x1001;
pub const SIGNATURE_NUMBER_HANDLE_BINARY: u64 = 0x1002;

pub type PrimitiveF64Thunk = unsafe extern "C-unwind" fn(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    left: f64,
    right: f64,
    out_result: *mut f64,
    user_data: *mut c_void,
) -> OgmaStatus;

pub type NumberHandleF64Thunk = unsafe extern "C-unwind" fn(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    left: OgmaHandle,
    right: OgmaHandle,
    out_result: *mut f64,
    user_data: *mut c_void,
) -> OgmaStatus;

#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HostFunctionId(pub u64);

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostFunctionSignature {
    Generic = 0,
    F64Binary = 1,
    NumberHandleBinary = 2,
}

/// Fixed-layout C descriptor. user_data stays host-owned until unregister completes.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct HostFunctionDescriptor {
    pub struct_size: u32,
    pub descriptor_version: u16,
    pub abi_major: u16,
    pub abi_minor: u16,
    pub reserved: u16,
    pub function_id: u64,
    pub name: [u8; MAX_NAME_BYTES],
    pub name_len: u16,
    pub reserved_name: u16,
    pub signature: HostFunctionSignature,
    pub signature_id: u64,
    pub effects: u32,
    pub reserved_effects: u32,
    pub required_capabilities: u64,
    pub primitive_f64_thunk: Option<PrimitiveF64Thunk>,
    pub number_handle_f64_thunk: Option<NumberHandleF64Thunk>,
    pub generic_callback: Option<OgmaHostCallback>,
    pub user_data: *mut c_void,
}

struct DescriptorConfig {
    required_capabilities: u64,
    effects: u32,
    callback: OgmaHostCallback,
    user_data: *mut c_void,
}

impl HostFunctionDescriptor {
    fn base(
        id: HostFunctionId,
        name: &str,
        signature: HostFunctionSignature,
        signature_id: u64,
        config: DescriptorConfig,
    ) -> Result<Self, DispatchError> {
        let bytes = name.as_bytes();
        if bytes.is_empty() || bytes.len() > MAX_NAME_BYTES || bytes.contains(&0) {
            return Err(DispatchError::InvalidDescriptor);
        }
        let version = ogma_abi_version();
        let mut name_buffer = [0_u8; MAX_NAME_BYTES];
        name_buffer[..bytes.len()].copy_from_slice(bytes);
        Ok(Self {
            struct_size: std::mem::size_of::<Self>() as u32,
            descriptor_version: DESCRIPTOR_VERSION,
            abi_major: version.major,
            abi_minor: version.minor,
            reserved: 0,
            function_id: id.0,
            name: name_buffer,
            name_len: bytes.len() as u16,
            reserved_name: 0,
            signature,
            signature_id,
            effects: config.effects,
            reserved_effects: 0,
            required_capabilities: config.required_capabilities,
            primitive_f64_thunk: None,
            number_handle_f64_thunk: None,
            generic_callback: Some(config.callback),
            user_data: config.user_data,
        })
    }

    pub fn generic(
        id: HostFunctionId,
        name: &str,
        required_capabilities: u64,
        effects: u32,
        callback: OgmaHostCallback,
        user_data: *mut c_void,
    ) -> Result<Self, DispatchError> {
        Self::base(
            id,
            name,
            HostFunctionSignature::Generic,
            SIGNATURE_GENERIC,
            DescriptorConfig {
                required_capabilities,
                effects,
                callback,
                user_data,
            },
        )
    }

    pub fn f64_binary(
        id: HostFunctionId,
        name: &str,
        required_capabilities: u64,
        effects: u32,
        typed_thunk: PrimitiveF64Thunk,
        generic_callback: OgmaHostCallback,
        user_data: *mut c_void,
    ) -> Result<Self, DispatchError> {
        let mut descriptor = Self::base(
            id,
            name,
            HostFunctionSignature::F64Binary,
            SIGNATURE_F64_BINARY,
            DescriptorConfig {
                required_capabilities,
                effects,
                callback: generic_callback,
                user_data,
            },
        )?;
        descriptor.primitive_f64_thunk = Some(typed_thunk);
        Ok(descriptor)
    }

    pub fn number_handle_binary(
        id: HostFunctionId,
        name: &str,
        required_capabilities: u64,
        effects: u32,
        typed_thunk: NumberHandleF64Thunk,
        generic_callback: OgmaHostCallback,
        user_data: *mut c_void,
    ) -> Result<Self, DispatchError> {
        let mut descriptor = Self::base(
            id,
            name,
            HostFunctionSignature::NumberHandleBinary,
            SIGNATURE_NUMBER_HANDLE_BINARY,
            DescriptorConfig {
                required_capabilities,
                effects,
                callback: generic_callback,
                user_data,
            },
        )?;
        descriptor.number_handle_f64_thunk = Some(typed_thunk);
        Ok(descriptor)
    }

    pub fn function_id(&self) -> HostFunctionId {
        HostFunctionId(self.function_id)
    }

    pub fn name(&self) -> Result<&str, DispatchError> {
        let len = usize::from(self.name_len);
        if len == 0 || len > MAX_NAME_BYTES || self.name[..len].contains(&0) {
            return Err(DispatchError::InvalidDescriptor);
        }
        std::str::from_utf8(&self.name[..len]).map_err(|_| DispatchError::InvalidDescriptor)
    }
}

/// Registry-local token. Generation changes on unregister; unrelated table mutations do not.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct HostFunctionSlot {
    pub registry_id: u64,
    pub slot_index: u32,
    pub slot_generation: u32,
    pub abi_major: u16,
    pub abi_minor: u16,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DispatchValue {
    Undefined,
    Number(f64),
    Boolean(bool),
    Handle(OgmaHandle),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JsException {
    TypeError,
    RangeError,
    ReferenceError,
    AsyncNotSupported,
    CapabilityDenied { missing: u64 },
    Thrown(DispatchValue),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JsCompletion {
    Normal(DispatchValue),
    Throw(JsException),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchError {
    Abi(OgmaStatus),
    HostExecution(HostError),
    WrongThread,
    InvalidDescriptor,
    DuplicateFunctionId,
    DuplicateName,
    WrongRegistry,
    StaleSlot,
    GenerationExhausted,
    UnsupportedValue,
    UnexpectedCompletion(u32),
    TooManyArguments,
    CleanupFailure(OgmaStatus),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DispatchMetrics {
    pub table_version: u64,
    pub active_slots: u64,
    pub total_calls: u64,
    pub typed_calls: u64,
    pub generic_calls: u64,
    pub generic_fallback_calls: u64,
    pub name_lookups: u64,
    pub property_lookups: u64,
    pub guarded_property_loads: u64,
    pub cache_hits: u64,
    pub guard_misses: u64,
    pub proxy_get_traps: u64,
    pub capability_denials: u64,
    pub signature_mismatches: u64,
}

#[derive(Clone, Copy)]
struct RegisteredFunction {
    descriptor: HostFunctionDescriptor,
    callback: OgmaCallbackHandle,
    function: OgmaHandle,
    persistent: OgmaPersistentHandle,
}
struct SlotCell {
    generation: u32,
    registered: Option<RegisteredFunction>,
}
struct RegistryState {
    slots: Vec<SlotCell>,
    by_id: HashMap<HostFunctionId, usize>,
    by_name: HashMap<String, usize>,
    table_version: u64,
}

struct RootScopeGuard {
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    token: OgmaRootScopeHandle,
    active: bool,
}
impl RootScopeGuard {
    fn open(
        runtime: *const OgmaRuntime,
        context: OgmaContextHandle,
    ) -> Result<Self, DispatchError> {
        let mut token = OgmaRootScopeHandle::default();
        let status = unsafe { ogma_root_scope_open(runtime, context, &mut token) };
        if status != OgmaStatus::OK {
            return Err(DispatchError::Abi(status));
        }
        Ok(Self {
            runtime,
            context,
            token,
            active: true,
        })
    }
    fn add(&self, value: OgmaHandle) -> Result<(), DispatchError> {
        let status = unsafe { ogma_root_scope_add(self.runtime, self.context, self.token, value) };
        if status == OgmaStatus::OK {
            Ok(())
        } else {
            Err(DispatchError::Abi(status))
        }
    }
    fn close(mut self) -> Result<(), DispatchError> {
        self.active = false;
        let status = unsafe { ogma_root_scope_close(self.runtime, self.context, self.token) };
        if status == OgmaStatus::OK {
            Ok(())
        } else {
            Err(DispatchError::Abi(status))
        }
    }
}
impl Drop for RootScopeGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = unsafe { ogma_root_scope_close(self.runtime, self.context, self.token) };
            self.active = false;
        }
    }
}

/// Thread-affine registry attached to one negotiated OGM-19 Runtime/Context.
/// Entry metadata is copied out before any external thunk/callback is invoked.
pub struct HostFunctionRegistry {
    registry_id: u64,
    owner_thread: ThreadId,
    runtime: *mut OgmaRuntime,
    context: OgmaContextHandle,
    abi_version: OgmaAbiVersion,
    state: RefCell<RegistryState>,
    metrics: Cell<DispatchMetrics>,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl HostFunctionRegistry {
    pub fn new() -> Result<Self, DispatchError> {
        let mut abi = OgmaAbiVersion::default();
        let status = unsafe {
            ogma_abi_negotiate(
                ABI_MAJOR_REQUIRED,
                ABI_MINOR_REQUIRED,
                REQUIRED_ABI_FEATURES,
                &mut abi,
            )
        };
        if status != OgmaStatus::OK {
            return Err(DispatchError::Abi(status));
        }

        let mut runtime = ptr::null_mut();
        let status = unsafe { ogma_runtime_create(&mut runtime) };
        if status != OgmaStatus::OK || runtime.is_null() {
            return Err(DispatchError::Abi(status));
        }
        let mut context = OgmaContextHandle::default();
        let status = unsafe { ogma_context_create(runtime, &mut context) };
        if status != OgmaStatus::OK {
            let _ = unsafe { ogma_runtime_destroy(&mut runtime) };
            return Err(DispatchError::Abi(status));
        }

        Ok(Self {
            registry_id: NEXT_REGISTRY_ID.fetch_add(1, Ordering::Relaxed),
            owner_thread: thread::current().id(),
            runtime,
            context,
            abi_version: abi,
            state: RefCell::new(RegistryState {
                slots: Vec::new(),
                by_id: HashMap::new(),
                by_name: HashMap::new(),
                table_version: 0,
            }),
            metrics: Cell::new(DispatchMetrics::default()),
            _not_send_or_sync: PhantomData,
        })
    }

    fn ensure_owner_thread(&self) -> Result<(), DispatchError> {
        if thread::current().id() == self.owner_thread {
            Ok(())
        } else {
            Err(DispatchError::WrongThread)
        }
    }

    fn validate_descriptor(
        &self,
        descriptor: &HostFunctionDescriptor,
    ) -> Result<String, DispatchError> {
        if descriptor.struct_size < std::mem::size_of::<HostFunctionDescriptor>() as u32
            || descriptor.descriptor_version != DESCRIPTOR_VERSION
            || descriptor.abi_major != self.abi_version.major
            || descriptor.abi_minor > self.abi_version.minor
            || descriptor.reserved != 0
            || descriptor.reserved_name != 0
            || descriptor.reserved_effects != 0
            || descriptor.function_id == 0
            || descriptor.effects & !KNOWN_EFFECTS != 0
            || descriptor.effects & EFFECT_MAY_SUSPEND != 0
            || descriptor.generic_callback.is_none()
        {
            return Err(DispatchError::InvalidDescriptor);
        }

        let name = descriptor.name()?.to_owned();
        let expected = match descriptor.signature {
            HostFunctionSignature::Generic => {
                if descriptor.primitive_f64_thunk.is_some()
                    || descriptor.number_handle_f64_thunk.is_some()
                {
                    return Err(DispatchError::InvalidDescriptor);
                }
                SIGNATURE_GENERIC
            }
            HostFunctionSignature::F64Binary => {
                if descriptor.primitive_f64_thunk.is_none()
                    || descriptor.number_handle_f64_thunk.is_some()
                {
                    return Err(DispatchError::InvalidDescriptor);
                }
                SIGNATURE_F64_BINARY
            }
            HostFunctionSignature::NumberHandleBinary => {
                if descriptor.primitive_f64_thunk.is_some()
                    || descriptor.number_handle_f64_thunk.is_none()
                {
                    return Err(DispatchError::InvalidDescriptor);
                }
                SIGNATURE_NUMBER_HANDLE_BINARY
            }
        };
        if descriptor.signature_id != expected {
            return Err(DispatchError::InvalidDescriptor);
        }
        Ok(name)
    }

    /// Registers a descriptor and roots its generic OGM-19 mock function for fallback.
    /// The host must keep user_data valid until this registration is unregistered.
    pub fn register(
        &self,
        descriptor: HostFunctionDescriptor,
    ) -> Result<HostFunctionSlot, DispatchError> {
        self.ensure_owner_thread()?;
        let name = self.validate_descriptor(&descriptor)?;
        let function_id = descriptor.function_id();

        let (index, generation) = {
            let state = self.state.borrow();
            if state.by_id.contains_key(&function_id) {
                return Err(DispatchError::DuplicateFunctionId);
            }
            if state.by_name.contains_key(&name) {
                return Err(DispatchError::DuplicateName);
            }
            match state
                .slots
                .iter()
                .position(|slot| slot.registered.is_none())
            {
                Some(index) => (index, state.slots[index].generation),
                None => (state.slots.len(), 1),
            }
        };

        let callback_fn = descriptor
            .generic_callback
            .ok_or(DispatchError::InvalidDescriptor)?;
        let mut callback = OgmaCallbackHandle::default();
        let status = unsafe {
            ogma_callback_register(
                self.runtime,
                self.context,
                Some(callback_fn),
                descriptor.user_data,
                &mut callback,
            )
        };
        if status != OgmaStatus::OK {
            return Err(DispatchError::Abi(status));
        }

        let mut function = OgmaHandle::default();
        let status = unsafe {
            ogma_mock_function_create(self.runtime, self.context, callback, &mut function)
        };
        if status != OgmaStatus::OK {
            let _ = unsafe { ogma_callback_unregister(self.runtime, self.context, callback) };
            return Err(DispatchError::Abi(status));
        }

        let mut persistent = OgmaPersistentHandle::default();
        let status = unsafe {
            ogma_persistent_create(self.runtime, self.context, function, &mut persistent)
        };
        if status != OgmaStatus::OK {
            let _ = unsafe { ogma_callback_unregister(self.runtime, self.context, callback) };
            return Err(DispatchError::Abi(status));
        }

        let registered = RegisteredFunction {
            descriptor,
            callback,
            function,
            persistent,
        };
        {
            let mut state = self.state.borrow_mut();
            if index == state.slots.len() {
                state.slots.push(SlotCell {
                    generation,
                    registered: Some(registered),
                });
            } else {
                let cell = &mut state.slots[index];
                if cell.registered.is_some() || cell.generation != generation {
                    let _ =
                        unsafe { ogma_callback_unregister(self.runtime, self.context, callback) };
                    let _ =
                        unsafe { ogma_persistent_release(self.runtime, self.context, persistent) };
                    return Err(DispatchError::StaleSlot);
                }
                cell.registered = Some(registered);
            }
            state.by_id.insert(function_id, index);
            state.by_name.insert(name, index);
            state.table_version = state.table_version.saturating_add(1);
            let mut metrics = self.metrics.get();
            metrics.table_version = state.table_version;
            metrics.active_slots = state.by_id.len() as u64;
            self.metrics.set(metrics);
        }
        Ok(HostFunctionSlot {
            registry_id: self.registry_id,
            slot_index: index as u32,
            slot_generation: generation,
            abi_major: self.abi_version.major,
            abi_minor: self.abi_version.minor,
        })
    }

    pub fn resolve_name(&self, name: &str) -> Result<HostFunctionSlot, DispatchError> {
        self.ensure_owner_thread()?;
        let index = self
            .state
            .borrow()
            .by_name
            .get(name)
            .copied()
            .ok_or(DispatchError::StaleSlot)?;
        self.add_metrics(|m| m.name_lookups = m.name_lookups.saturating_add(1));
        self.slot_for_index(index)
    }

    pub fn resolve_id(&self, id: HostFunctionId) -> Result<HostFunctionSlot, DispatchError> {
        self.ensure_owner_thread()?;
        let index = self
            .state
            .borrow()
            .by_id
            .get(&id)
            .copied()
            .ok_or(DispatchError::StaleSlot)?;
        self.slot_for_index(index)
    }

    fn slot_for_index(&self, index: usize) -> Result<HostFunctionSlot, DispatchError> {
        let state = self.state.borrow();
        let cell = state.slots.get(index).ok_or(DispatchError::StaleSlot)?;
        if cell.registered.is_none() {
            return Err(DispatchError::StaleSlot);
        }
        Ok(HostFunctionSlot {
            registry_id: self.registry_id,
            slot_index: index as u32,
            slot_generation: cell.generation,
            abi_major: self.abi_version.major,
            abi_minor: self.abi_version.minor,
        })
    }

    fn snapshot(&self, slot: HostFunctionSlot) -> Result<RegisteredFunction, DispatchError> {
        self.ensure_owner_thread()?;
        if slot.registry_id != self.registry_id {
            return Err(DispatchError::WrongRegistry);
        }
        if slot.abi_major != self.abi_version.major || slot.abi_minor > self.abi_version.minor {
            return Err(DispatchError::StaleSlot);
        }
        let state = self.state.borrow();
        let cell = state
            .slots
            .get(slot.slot_index as usize)
            .ok_or(DispatchError::StaleSlot)?;
        if cell.generation != slot.slot_generation {
            return Err(DispatchError::StaleSlot);
        }
        cell.registered.ok_or(DispatchError::StaleSlot)
    }

    pub fn unregister(&self, slot: HostFunctionSlot) -> Result<(), DispatchError> {
        self.ensure_owner_thread()?;
        if slot.registry_id != self.registry_id {
            return Err(DispatchError::WrongRegistry);
        }
        let entry = {
            let mut state = self.state.borrow_mut();
            let cell = state
                .slots
                .get_mut(slot.slot_index as usize)
                .ok_or(DispatchError::StaleSlot)?;
            if cell.generation != slot.slot_generation || cell.registered.is_none() {
                return Err(DispatchError::StaleSlot);
            }
            if cell.generation == u32::MAX {
                return Err(DispatchError::GenerationExhausted);
            }
            let entry = cell.registered.take().ok_or(DispatchError::StaleSlot)?;
            cell.generation += 1;
            let name = entry.descriptor.name()?.to_owned();
            state.by_id.remove(&entry.descriptor.function_id());
            state.by_name.remove(&name);
            state.table_version = state.table_version.saturating_add(1);
            let mut metrics = self.metrics.get();
            metrics.table_version = state.table_version;
            metrics.active_slots = state.by_id.len() as u64;
            self.metrics.set(metrics);
            entry
        };

        let callback_status =
            unsafe { ogma_callback_unregister(self.runtime, self.context, entry.callback) };
        let root_status =
            unsafe { ogma_persistent_release(self.runtime, self.context, entry.persistent) };
        let collect_status = unsafe { ogma_context_collect_unrooted(self.runtime, self.context) };
        for status in [callback_status, root_status, collect_status] {
            if status != OgmaStatus::OK {
                return Err(DispatchError::CleanupFailure(status));
            }
        }
        Ok(())
    }

    pub fn create_number(&self, value: f64) -> Result<OgmaHandle, DispatchError> {
        self.ensure_owner_thread()?;
        let mut out = OgmaHandle::default();
        let status =
            unsafe { ogma_context_create_number(self.runtime, self.context, value, &mut out) };
        if status == OgmaStatus::OK {
            Ok(out)
        } else {
            Err(DispatchError::Abi(status))
        }
    }

    pub fn create_boolean(&self, value: bool) -> Result<OgmaHandle, DispatchError> {
        self.ensure_owner_thread()?;
        let mut out = OgmaHandle::default();
        let status = unsafe {
            ogma_context_create_boolean(self.runtime, self.context, u8::from(value), &mut out)
        };
        if status == OgmaStatus::OK {
            Ok(out)
        } else {
            Err(DispatchError::Abi(status))
        }
    }

    pub fn read_number(&self, handle: OgmaHandle) -> Result<f64, DispatchError> {
        self.ensure_owner_thread()?;
        let mut out = 0.0;
        let status =
            unsafe { ogma_context_get_number(self.runtime, self.context, handle, &mut out) };
        if status == OgmaStatus::OK {
            Ok(out)
        } else {
            Err(DispatchError::Abi(status))
        }
    }

    pub fn table_version(&self) -> u64 {
        self.state.borrow().table_version
    }

    pub fn metrics(&self) -> DispatchMetrics {
        let mut metrics = self.metrics.get();
        let state = self.state.borrow();
        metrics.table_version = state.table_version;
        metrics.active_slots = state.by_id.len() as u64;
        metrics
    }

    fn add_metrics(&self, f: impl FnOnce(&mut DispatchMetrics)) {
        let mut metrics = self.metrics.get();
        f(&mut metrics);
        self.metrics.set(metrics);
    }

    pub fn invoke_by_name(
        &self,
        scope: &HostExecutionScope<'_>,
        name: &str,
        capabilities: u64,
        args: &[DispatchValue],
    ) -> Result<JsCompletion, DispatchError> {
        let slot = self.resolve_name(name)?;
        self.invoke_slot(scope, slot, capabilities, args)
    }

    pub fn invoke_slot(
        &self,
        scope: &HostExecutionScope<'_>,
        slot: HostFunctionSlot,
        capabilities: u64,
        args: &[DispatchValue],
    ) -> Result<JsCompletion, DispatchError> {
        self.invoke_scoped(scope, slot, capabilities, args, false)
    }

    pub fn invoke_generic_slot(
        &self,
        scope: &HostExecutionScope<'_>,
        slot: HostFunctionSlot,
        capabilities: u64,
        args: &[DispatchValue],
    ) -> Result<JsCompletion, DispatchError> {
        self.invoke_scoped(scope, slot, capabilities, args, true)
    }

    fn invoke_scoped(
        &self,
        scope: &HostExecutionScope<'_>,
        slot: HostFunctionSlot,
        capabilities: u64,
        args: &[DispatchValue],
        force_generic: bool,
    ) -> Result<JsCompletion, DispatchError> {
        scope
            .run_callback(|_| self.invoke_inner(slot, capabilities, args, force_generic))
            .map_err(DispatchError::HostExecution)?
    }

    fn invoke_inner(
        &self,
        slot: HostFunctionSlot,
        capabilities: u64,
        args: &[DispatchValue],
        force_generic: bool,
    ) -> Result<JsCompletion, DispatchError> {
        let entry = self.snapshot(slot)?;
        self.add_metrics(|m| m.total_calls = m.total_calls.saturating_add(1));

        let missing = entry.descriptor.required_capabilities & !capabilities;
        if missing != 0 {
            self.add_metrics(|m| m.capability_denials = m.capability_denials.saturating_add(1));
            return Ok(JsCompletion::Throw(JsException::CapabilityDenied {
                missing,
            }));
        }
        if entry.descriptor.effects & EFFECT_MAY_SUSPEND != 0 {
            return Ok(JsCompletion::Throw(JsException::AsyncNotSupported));
        }

        let typed_allowed = !force_generic && entry.descriptor.effects & COMPLEX_EFFECTS == 0;
        match entry.descriptor.signature {
            HostFunctionSignature::F64Binary => {
                let [DispatchValue::Number(left), DispatchValue::Number(right)] = args else {
                    return self.signature_mismatch();
                };
                if typed_allowed {
                    self.call_primitive_typed(entry, *left, *right)
                } else {
                    self.call_generic(entry, args)
                }
            }
            HostFunctionSignature::NumberHandleBinary => {
                let [DispatchValue::Handle(left), DispatchValue::Handle(right)] = args else {
                    return self.signature_mismatch();
                };
                if typed_allowed {
                    self.call_number_handle_typed(entry, *left, *right)
                } else {
                    self.call_generic(entry, args)
                }
            }
            HostFunctionSignature::Generic => self.call_generic(entry, args),
        }
    }

    fn signature_mismatch(&self) -> Result<JsCompletion, DispatchError> {
        self.add_metrics(|m| m.signature_mismatches = m.signature_mismatches.saturating_add(1));
        Ok(JsCompletion::Throw(JsException::TypeError))
    }

    fn call_primitive_typed(
        &self,
        entry: RegisteredFunction,
        left: f64,
        right: f64,
    ) -> Result<JsCompletion, DispatchError> {
        let thunk = entry
            .descriptor
            .primitive_f64_thunk
            .ok_or(DispatchError::InvalidDescriptor)?;
        let mut out = 0.0;
        let status = unsafe {
            thunk(
                self.runtime,
                self.context,
                left,
                right,
                &mut out,
                entry.descriptor.user_data,
            )
        };
        self.map_typed_status(status, out)
    }

    fn call_number_handle_typed(
        &self,
        entry: RegisteredFunction,
        left: OgmaHandle,
        right: OgmaHandle,
    ) -> Result<JsCompletion, DispatchError> {
        let thunk = entry
            .descriptor
            .number_handle_f64_thunk
            .ok_or(DispatchError::InvalidDescriptor)?;
        let roots = RootScopeGuard::open(self.runtime, self.context)?;
        roots.add(left)?;
        roots.add(right)?;
        let mut out = 0.0;
        let status = unsafe {
            thunk(
                self.runtime,
                self.context,
                left,
                right,
                &mut out,
                entry.descriptor.user_data,
            )
        };
        roots.close()?;
        self.map_typed_status(status, out)
    }

    fn map_typed_status(
        &self,
        status: OgmaStatus,
        value: f64,
    ) -> Result<JsCompletion, DispatchError> {
        if status == OgmaStatus::OK {
            self.add_metrics(|m| m.typed_calls = m.typed_calls.saturating_add(1));
            Ok(JsCompletion::Normal(DispatchValue::Number(value)))
        } else if status == OgmaStatus::TYPE_MISMATCH {
            self.signature_mismatch()
        } else if status == OgmaStatus::OUT_OF_RANGE {
            Ok(JsCompletion::Throw(JsException::RangeError))
        } else {
            Err(DispatchError::Abi(status))
        }
    }

    fn call_generic(
        &self,
        entry: RegisteredFunction,
        args: &[DispatchValue],
    ) -> Result<JsCompletion, DispatchError> {
        if args.len() > MAX_ARGUMENTS {
            return Err(DispatchError::TooManyArguments);
        }
        self.add_metrics(|m| {
            m.generic_calls = m.generic_calls.saturating_add(1);
            m.generic_fallback_calls = m.generic_fallback_calls.saturating_add(1);
        });
        let roots = RootScopeGuard::open(self.runtime, self.context)?;
        let mut handles = Vec::with_capacity(args.len());
        for argument in args {
            let handle = match argument {
                DispatchValue::Number(value) => self.create_number(*value)?,
                DispatchValue::Boolean(value) => self.create_boolean(*value)?,
                DispatchValue::Handle(value) => *value,
                DispatchValue::Undefined => return Err(DispatchError::UnsupportedValue),
            };
            roots.add(handle)?;
            handles.push(handle);
        }

        let mut completion = OgmaCompletion::default();
        let status = unsafe {
            ogma_context_call(
                self.runtime,
                self.context,
                entry.function,
                handles.as_ptr(),
                handles.len(),
                &mut completion,
            )
        };
        let result = if status == OgmaStatus::TYPE_MISMATCH {
            JsCompletion::Throw(JsException::TypeError)
        } else if status == OgmaStatus::OUT_OF_RANGE {
            JsCompletion::Throw(JsException::RangeError)
        } else if status != OgmaStatus::OK {
            return Err(DispatchError::Abi(status));
        } else {
            let value = if completion.value == OgmaHandle::default() {
                DispatchValue::Undefined
            } else {
                self.decode_value(completion.value)?
            };
            match completion.kind {
                COMPLETION_NORMAL => JsCompletion::Normal(value),
                COMPLETION_THROW => JsCompletion::Throw(JsException::Thrown(value)),
                other => return Err(DispatchError::UnexpectedCompletion(other)),
            }
        };
        roots.close()?;
        Ok(result)
    }

    fn decode_value(&self, handle: OgmaHandle) -> Result<DispatchValue, DispatchError> {
        let mut number = 0.0;
        let status =
            unsafe { ogma_context_get_number(self.runtime, self.context, handle, &mut number) };
        if status == OgmaStatus::OK {
            return Ok(DispatchValue::Number(number));
        }
        if status != OgmaStatus::TYPE_MISMATCH {
            return Err(DispatchError::Abi(status));
        }

        let mut boolean = 0_u8;
        let status =
            unsafe { ogma_context_get_boolean(self.runtime, self.context, handle, &mut boolean) };
        if status == OgmaStatus::OK {
            return Ok(DispatchValue::Boolean(boolean != 0));
        }
        if status == OgmaStatus::TYPE_MISMATCH {
            Err(DispatchError::UnsupportedValue)
        } else {
            Err(DispatchError::Abi(status))
        }
    }

    pub fn resolve_call_site(
        &self,
        object: &OrdinaryHostObject,
        property: &str,
    ) -> Option<CallSiteGuard> {
        let property_slot = object.property_slot(property)?;
        match object.get_by_slot(property_slot)? {
            JsCallable::Host { identity, slot } => Some(CallSiteGuard {
                object_identity: object.identity,
                shape_id: object.shape_id,
                property: property.to_owned(),
                property_slot,
                function_identity: identity,
                slot,
            }),
            JsCallable::Script { .. } => None,
        }
    }

    /// Proxy gets always execute the trap and use generic call dispatch. Ordinary-object
    /// fast paths require both the same layout and the current function-object identity.
    pub fn call_property(
        &self,
        scope: &HostExecutionScope<'_>,
        receiver: PropertyReceiver<'_>,
        property: &str,
        guard: Option<&CallSiteGuard>,
        capabilities: u64,
        args: &[DispatchValue],
    ) -> Result<JsCompletion, DispatchError> {
        let callable = match receiver {
            PropertyReceiver::Ordinary(object) => {
                let matching_guard = guard.filter(|candidate| {
                    candidate.object_identity == object.identity
                        && candidate.shape_id == object.shape_id
                        && candidate.property == property
                });

                if let Some(guard) = matching_guard {
                    self.add_metrics(|metrics| {
                        metrics.guarded_property_loads =
                            metrics.guarded_property_loads.saturating_add(1);
                    });
                    let current = object.get_by_slot(guard.property_slot);
                    if let Some(JsCallable::Host { identity, slot }) = current {
                        if identity == guard.function_identity && slot == guard.slot {
                            self.add_metrics(|metrics| {
                                metrics.cache_hits = metrics.cache_hits.saturating_add(1);
                            });
                            return self.invoke_slot(scope, slot, capabilities, args);
                        }
                    }
                    self.add_metrics(|metrics| {
                        metrics.guard_misses = metrics.guard_misses.saturating_add(1);
                    });
                    current
                } else {
                    if guard.is_some() {
                        self.add_metrics(|metrics| {
                            metrics.guard_misses = metrics.guard_misses.saturating_add(1);
                        });
                    }
                    self.add_metrics(|metrics| {
                        metrics.property_lookups = metrics.property_lookups.saturating_add(1);
                    });
                    object.get(property)
                }
            }
            PropertyReceiver::Proxy(proxy) => {
                self.add_metrics(|metrics| {
                    metrics.proxy_get_traps = metrics.proxy_get_traps.saturating_add(1);
                    metrics.property_lookups = metrics.property_lookups.saturating_add(1);
                });
                proxy.get(property)
            }
        };

        let Some(callable) = callable else {
            return Ok(JsCompletion::Throw(JsException::ReferenceError));
        };

        match callable {
            JsCallable::Host { slot, .. } => {
                self.invoke_generic_slot(scope, slot, capabilities, args)
            }
            JsCallable::Script { result, .. } => Ok(JsCompletion::Normal(result)),
        }
    }
}

impl Drop for HostFunctionRegistry {
    fn drop(&mut self) {
        if thread::current().id() != self.owner_thread {
            return;
        }
        let slots = {
            let state = self.state.borrow();
            state
                .slots
                .iter()
                .enumerate()
                .filter_map(|(index, cell)| {
                    cell.registered.as_ref().map(|_| HostFunctionSlot {
                        registry_id: self.registry_id,
                        slot_index: index as u32,
                        slot_generation: cell.generation,
                        abi_major: self.abi_version.major,
                        abi_minor: self.abi_version.minor,
                    })
                })
                .collect::<Vec<_>>()
        };
        for slot in slots {
            let _ = self.unregister(slot);
        }
        let _ = unsafe { ogma_context_destroy(self.runtime, self.context) };
        let _ = unsafe { ogma_runtime_destroy(&mut self.runtime) };
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JsCallable {
    Host {
        identity: u64,
        slot: HostFunctionSlot,
    },
    Script {
        identity: u64,
        result: DispatchValue,
    },
}
impl JsCallable {
    pub fn host(slot: HostFunctionSlot) -> Self {
        Self::Host {
            identity: NEXT_FUNCTION_ID.fetch_add(1, Ordering::Relaxed),
            slot,
        }
    }
    pub fn script(result: DispatchValue) -> Self {
        Self::Script {
            identity: NEXT_FUNCTION_ID.fetch_add(1, Ordering::Relaxed),
            result,
        }
    }
}

pub struct OrdinaryHostObject {
    identity: u64,
    shape_id: u64,
    property_slots: HashMap<String, usize>,
    properties: Vec<Option<JsCallable>>,
}
impl OrdinaryHostObject {
    pub fn new() -> Self {
        Self {
            identity: NEXT_OBJECT_ID.fetch_add(1, Ordering::Relaxed),
            shape_id: 1,
            property_slots: HashMap::new(),
            properties: Vec::new(),
        }
    }
    pub fn define(&mut self, property: impl Into<String>, callable: JsCallable) {
        let property = property.into();
        if let Some(index) = self.property_slots.get(&property).copied() {
            self.properties[index] = Some(callable);
            return;
        }
        let index = self.properties.len();
        self.property_slots.insert(property, index);
        self.properties.push(Some(callable));
        self.shape_id = self.shape_id.saturating_add(1);
    }
    /// Replaces a property value without changing its layout; the function identity guard fails.
    pub fn replace(&mut self, property: &str, callable: JsCallable) -> bool {
        match self.property_slots.get(property).copied() {
            Some(index) => {
                self.properties[index] = Some(callable);
                true
            }
            None => false,
        }
    }
    pub fn delete(&mut self, property: &str) -> bool {
        let Some(index) = self.property_slots.remove(property) else {
            return false;
        };
        self.properties[index] = None;
        self.shape_id = self.shape_id.saturating_add(1);
        true
    }
    fn property_slot(&self, property: &str) -> Option<usize> {
        self.property_slots.get(property).copied()
    }
    fn get_by_slot(&self, slot: usize) -> Option<JsCallable> {
        self.properties.get(slot).copied().flatten()
    }
    fn get(&self, property: &str) -> Option<JsCallable> {
        self.get_by_slot(self.property_slot(property)?)
    }
}
impl Default for OrdinaryHostObject {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ProxyHostObject {
    target: OrdinaryHostObject,
    get_overrides: HashMap<String, JsCallable>,
    get_trap_calls: Cell<u64>,
}
impl ProxyHostObject {
    pub fn new(target: OrdinaryHostObject) -> Self {
        Self {
            target,
            get_overrides: HashMap::new(),
            get_trap_calls: Cell::new(0),
        }
    }
    pub fn set_get_result(&mut self, property: impl Into<String>, callable: JsCallable) {
        self.get_overrides.insert(property.into(), callable);
    }
    pub fn get_trap_calls(&self) -> u64 {
        self.get_trap_calls.get()
    }
    fn get(&self, property: &str) -> Option<JsCallable> {
        self.get_trap_calls
            .set(self.get_trap_calls.get().saturating_add(1));
        self.get_overrides
            .get(property)
            .copied()
            .or_else(|| self.target.get(property))
    }
}

pub enum PropertyReceiver<'a> {
    Ordinary(&'a OrdinaryHostObject),
    Proxy(&'a ProxyHostObject),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallSiteGuard {
    object_identity: u64,
    shape_id: u64,
    property: String,
    property_slot: usize,
    function_identity: u64,
    slot: HostFunctionSlot,
}
impl CallSiteGuard {
    pub fn function_identity(&self) -> u64 {
        self.function_identity
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ogma_host_execution_scope_proof::LibrarySyncMode;
    use std::slice;

    unsafe extern "C-unwind" fn generic_add(
        runtime: *const OgmaRuntime,
        context: OgmaContextHandle,
        args: *const OgmaHandle,
        args_len: usize,
        completion: *mut OgmaCompletion,
        _user_data: *mut c_void,
    ) -> OgmaStatus {
        if args_len != 2 || args.is_null() || completion.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        let args = unsafe { slice::from_raw_parts(args, args_len) };
        let mut left = 0.0;
        let mut right = 0.0;
        let status = unsafe { ogma_context_get_number(runtime, context, args[0], &mut left) };
        if status != OgmaStatus::OK {
            return status;
        }
        let status = unsafe { ogma_context_get_number(runtime, context, args[1], &mut right) };
        if status != OgmaStatus::OK {
            return status;
        }
        let mut result = 0.0;
        let status =
            unsafe { ogma_rust_abi_proof::ogma_primitive_add_f64(left, right, &mut result) };
        if status != OgmaStatus::OK {
            return status;
        }
        let mut handle = OgmaHandle::default();
        let status = unsafe { ogma_context_create_number(runtime, context, result, &mut handle) };
        if status != OgmaStatus::OK {
            return status;
        }
        unsafe {
            completion.write(OgmaCompletion {
                kind: COMPLETION_NORMAL,
                reserved: 0,
                value: handle,
            });
        }
        OgmaStatus::OK
    }

    unsafe extern "C-unwind" fn generic_throw(
        _runtime: *const OgmaRuntime,
        _context: OgmaContextHandle,
        args: *const OgmaHandle,
        args_len: usize,
        completion: *mut OgmaCompletion,
        _user_data: *mut c_void,
    ) -> OgmaStatus {
        if args_len == 0 || args.is_null() || completion.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        let args = unsafe { slice::from_raw_parts(args, args_len) };
        unsafe {
            completion.write(OgmaCompletion {
                kind: COMPLETION_THROW,
                reserved: 0,
                value: args[0],
            });
        }
        OgmaStatus::OK
    }

    unsafe extern "C-unwind" fn generic_failure(
        _runtime: *const OgmaRuntime,
        _context: OgmaContextHandle,
        _args: *const OgmaHandle,
        _args_len: usize,
        _completion: *mut OgmaCompletion,
        _user_data: *mut c_void,
    ) -> OgmaStatus {
        OgmaStatus::CALLBACK_ERROR
    }

    unsafe extern "C-unwind" fn generic_panic(
        _runtime: *const OgmaRuntime,
        _context: OgmaContextHandle,
        _args: *const OgmaHandle,
        _args_len: usize,
        _completion: *mut OgmaCompletion,
        _user_data: *mut c_void,
    ) -> OgmaStatus {
        panic!("deliberate generic callback panic");
    }

    unsafe extern "C-unwind" fn typed_add(
        _runtime: *const OgmaRuntime,
        _context: OgmaContextHandle,
        left: f64,
        right: f64,
        out: *mut f64,
        _user_data: *mut c_void,
    ) -> OgmaStatus {
        if out.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        unsafe { out.write(left + right) };
        OgmaStatus::OK
    }

    unsafe extern "C-unwind" fn typed_handles(
        runtime: *const OgmaRuntime,
        context: OgmaContextHandle,
        left: OgmaHandle,
        right: OgmaHandle,
        out: *mut f64,
        _user_data: *mut c_void,
    ) -> OgmaStatus {
        if out.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        let mut l = 0.0;
        let mut r = 0.0;
        let status = unsafe { ogma_context_get_number(runtime, context, left, &mut l) };
        if status != OgmaStatus::OK {
            return status;
        }
        let status = unsafe { ogma_context_get_number(runtime, context, right, &mut r) };
        if status != OgmaStatus::OK {
            return status;
        }
        unsafe { out.write(l + r) };
        OgmaStatus::OK
    }

    unsafe extern "C-unwind" fn typed_panic(
        _runtime: *const OgmaRuntime,
        _context: OgmaContextHandle,
        _left: f64,
        _right: f64,
        _out: *mut f64,
        _user_data: *mut c_void,
    ) -> OgmaStatus {
        panic!("deliberate typed thunk panic");
    }

    fn scope_context() -> ogma_host_execution_scope_proof::HostExecutionContext {
        ogma_host_execution_scope_proof::HostExecutionContext::new(LibrarySyncMode::None)
    }
    fn register_add(registry: &HostFunctionRegistry, id: u64, name: &str) -> HostFunctionSlot {
        registry
            .register(
                HostFunctionDescriptor::f64_binary(
                    HostFunctionId(id),
                    name,
                    0,
                    0,
                    typed_add,
                    generic_add,
                    ptr::null_mut(),
                )
                .unwrap(),
            )
            .unwrap()
    }

    #[test]
    fn descriptor_layout_and_slot_resolution_are_stable() {
        assert_eq!(std::mem::size_of::<HostFunctionDescriptor>(), 216);
        assert_eq!(std::mem::size_of::<HostFunctionSlot>(), 24);
        let registry = HostFunctionRegistry::new().unwrap();
        let slot = register_add(&registry, 1001, "ogma:math/add");
        assert_eq!(registry.resolve_name("ogma:math/add").unwrap(), slot);
        assert_eq!(registry.resolve_id(HostFunctionId(1001)).unwrap(), slot);
        assert_eq!(registry.table_version(), 1);
        assert_eq!(registry.metrics().active_slots, 1);
    }

    #[test]
    fn typed_and_generic_routes_return_the_same_value() {
        let registry = HostFunctionRegistry::new().unwrap();
        let slot = register_add(&registry, 1001, "ogma:math/add");
        let host = scope_context();
        let scope = host.enter_scope().unwrap();
        let args = [DispatchValue::Number(2.0), DispatchValue::Number(3.0)];
        assert_eq!(
            registry.invoke_slot(&scope, slot, 0, &args).unwrap(),
            JsCompletion::Normal(DispatchValue::Number(5.0))
        );
        assert_eq!(
            registry
                .invoke_generic_slot(&scope, slot, 0, &args)
                .unwrap(),
            JsCompletion::Normal(DispatchValue::Number(5.0))
        );
        let metrics = registry.metrics();
        assert_eq!(metrics.typed_calls, 1);
        assert_eq!(metrics.generic_calls, 1);
        assert_eq!(metrics.generic_fallback_calls, 1);
    }

    #[test]
    fn typed_object_handles_use_the_opaque_abi_boundary() {
        let registry = HostFunctionRegistry::new().unwrap();
        let descriptor = HostFunctionDescriptor::number_handle_binary(
            HostFunctionId(1002),
            "ogma:math/add-handles",
            0,
            0,
            typed_handles,
            generic_add,
            ptr::null_mut(),
        )
        .unwrap();
        let slot = registry.register(descriptor).unwrap();
        let left = registry.create_number(4.0).unwrap();
        let right = registry.create_number(6.0).unwrap();
        let host = scope_context();
        let scope = host.enter_scope().unwrap();
        assert_eq!(
            registry
                .invoke_slot(
                    &scope,
                    slot,
                    0,
                    &[DispatchValue::Handle(left), DispatchValue::Handle(right)]
                )
                .unwrap(),
            JsCompletion::Normal(DispatchValue::Number(10.0))
        );
    }

    #[test]
    fn capability_is_checked_on_every_invocation() {
        let registry = HostFunctionRegistry::new().unwrap();
        let descriptor = HostFunctionDescriptor::f64_binary(
            HostFunctionId(1001),
            "ogma:protected/add",
            0b0100,
            0,
            typed_add,
            generic_add,
            ptr::null_mut(),
        )
        .unwrap();
        let slot = registry.register(descriptor).unwrap();
        let host = scope_context();
        let scope = host.enter_scope().unwrap();
        let args = [DispatchValue::Number(1.0), DispatchValue::Number(2.0)];
        let mut object = OrdinaryHostObject::new();
        object.define("secureAdd", JsCallable::host(slot));
        let guard = registry.resolve_call_site(&object, "secureAdd").unwrap();
        let allowed = registry
            .call_property(
                &scope,
                PropertyReceiver::Ordinary(&object),
                "secureAdd",
                Some(&guard),
                0b0100,
                &args,
            )
            .unwrap();
        assert_eq!(allowed, JsCompletion::Normal(DispatchValue::Number(3.0)));
        let denied = registry
            .call_property(
                &scope,
                PropertyReceiver::Ordinary(&object),
                "secureAdd",
                Some(&guard),
                0,
                &args,
            )
            .unwrap();
        assert_eq!(
            denied,
            JsCompletion::Throw(JsException::CapabilityDenied { missing: 0b0100 })
        );
        let metrics = registry.metrics();
        assert_eq!(metrics.typed_calls, 1);
        assert_eq!(metrics.capability_denials, 1);
        assert_eq!(metrics.cache_hits, 2);
        assert_eq!(metrics.property_lookups, 0);
        assert_eq!(metrics.guarded_property_loads, 2);
    }

    #[test]
    fn signature_mismatch_is_a_js_type_error() {
        let registry = HostFunctionRegistry::new().unwrap();
        let slot = register_add(&registry, 1001, "ogma:math/add");
        let boolean = registry.create_boolean(true).unwrap();
        let host = scope_context();
        let scope = host.enter_scope().unwrap();
        assert_eq!(
            registry
                .invoke_slot(
                    &scope,
                    slot,
                    0,
                    &[DispatchValue::Handle(boolean), DispatchValue::Number(1.0)]
                )
                .unwrap(),
            JsCompletion::Throw(JsException::TypeError)
        );
        assert_eq!(registry.metrics().signature_mismatches, 1);
    }

    #[test]
    fn replacing_a_function_invalidates_guard_even_when_shape_is_unchanged() {
        let registry = HostFunctionRegistry::new().unwrap();
        let slot = register_add(&registry, 1001, "ogma:math/add");
        let mut object = OrdinaryHostObject::new();
        object.define("run", JsCallable::host(slot));
        let guard = registry.resolve_call_site(&object, "run").unwrap();
        let shape = object.shape_id;
        assert!(object.replace("run", JsCallable::script(DispatchValue::Number(77.0))));
        assert_eq!(object.shape_id, shape);
        let host = scope_context();
        let scope = host.enter_scope().unwrap();
        assert_eq!(
            registry
                .call_property(
                    &scope,
                    PropertyReceiver::Ordinary(&object),
                    "run",
                    Some(&guard),
                    0,
                    &[DispatchValue::Number(2.0), DispatchValue::Number(3.0)]
                )
                .unwrap(),
            JsCompletion::Normal(DispatchValue::Number(77.0))
        );
        let metrics = registry.metrics();
        assert_eq!(metrics.typed_calls, 0);
        assert_eq!(metrics.guard_misses, 1);
        assert_eq!(metrics.property_lookups, 0);
        assert_eq!(metrics.guarded_property_loads, 1);
    }

    #[test]
    fn shape_change_invalidates_guard_and_uses_dynamic_property_lookup() {
        let registry = HostFunctionRegistry::new().unwrap();
        let slot = register_add(&registry, 1001, "ogma:math/add");
        let mut object = OrdinaryHostObject::new();
        object.define("run", JsCallable::host(slot));
        let guard = registry.resolve_call_site(&object, "run").unwrap();
        object.define("other", JsCallable::script(DispatchValue::Number(9.0)));

        let host = scope_context();
        let scope = host.enter_scope().unwrap();
        assert_eq!(
            registry.call_property(
                &scope,
                PropertyReceiver::Ordinary(&object),
                "run",
                Some(&guard),
                0,
                &[DispatchValue::Number(2.0), DispatchValue::Number(3.0)],
            ).unwrap(),
            JsCompletion::Normal(DispatchValue::Number(5.0)),
        );
        let metrics = registry.metrics();
        assert_eq!(metrics.guard_misses, 1);
        assert_eq!(metrics.property_lookups, 1);
        assert_eq!(metrics.guarded_property_loads, 0);
        assert_eq!(metrics.generic_calls, 1);
    }

    #[test]
    fn proxy_get_trap_is_never_skipped_by_a_cached_guard() {
        let registry = HostFunctionRegistry::new().unwrap();
        let slot = register_add(&registry, 1001, "ogma:math/add");
        let mut target = OrdinaryHostObject::new();
        target.define("run", JsCallable::host(slot));
        let guard = registry.resolve_call_site(&target, "run").unwrap();
        let proxy = ProxyHostObject::new(target);
        let host = scope_context();
        let scope = host.enter_scope().unwrap();
        assert_eq!(
            registry
                .call_property(
                    &scope,
                    PropertyReceiver::Proxy(&proxy),
                    "run",
                    Some(&guard),
                    0,
                    &[DispatchValue::Number(2.0), DispatchValue::Number(3.0)]
                )
                .unwrap(),
            JsCompletion::Normal(DispatchValue::Number(5.0))
        );
        assert_eq!(proxy.get_trap_calls(), 1);
        assert_eq!(registry.metrics().proxy_get_traps, 1);
        assert_eq!(registry.metrics().property_lookups, 1);
        assert_eq!(registry.metrics().guarded_property_loads, 0);
        assert_eq!(registry.metrics().typed_calls, 0);
        assert_eq!(registry.metrics().generic_calls, 1);
    }

    #[test]
    fn unregister_reuse_increments_generation_and_rejects_old_slot() {
        let registry = HostFunctionRegistry::new().unwrap();
        let old = register_add(&registry, 1001, "ogma:math/add");
        registry.unregister(old).unwrap();
        let new = register_add(&registry, 1001, "ogma:math/add");
        assert_eq!(old.slot_index, new.slot_index);
        assert_ne!(old.slot_generation, new.slot_generation);
        assert_eq!(registry.table_version(), 3);
        let host = scope_context();
        let scope = host.enter_scope().unwrap();
        let args = [DispatchValue::Number(1.0), DispatchValue::Number(2.0)];
        assert_eq!(
            registry.invoke_slot(&scope, old, 0, &args),
            Err(DispatchError::StaleSlot)
        );
        assert_eq!(
            registry.invoke_slot(&scope, new, 0, &args).unwrap(),
            JsCompletion::Normal(DispatchValue::Number(3.0))
        );
    }

    #[test]
    fn js_throw_completion_is_distinct_from_abi_callback_error() {
        let registry = HostFunctionRegistry::new().unwrap();
        let throw = HostFunctionDescriptor::generic(
            HostFunctionId(2001),
            "ogma:test/throw",
            0,
            EFFECT_MAY_THROW,
            generic_throw,
            ptr::null_mut(),
        )
        .unwrap();
        let fail = HostFunctionDescriptor::generic(
            HostFunctionId(2002),
            "ogma:test/fail",
            0,
            0,
            generic_failure,
            ptr::null_mut(),
        )
        .unwrap();
        let throw_slot = registry.register(throw).unwrap();
        let fail_slot = registry.register(fail).unwrap();
        let input = registry.create_number(13.0).unwrap();
        let host = scope_context();
        let scope = host.enter_scope().unwrap();
        assert_eq!(
            registry
                .invoke_slot(&scope, throw_slot, 0, &[DispatchValue::Handle(input)])
                .unwrap(),
            JsCompletion::Throw(JsException::Thrown(DispatchValue::Number(13.0)))
        );
        assert_eq!(
            registry.invoke_slot(&scope, fail_slot, 0, &[]),
            Err(DispatchError::Abi(OgmaStatus::CALLBACK_ERROR))
        );
    }

    #[test]
    fn panic_is_contained_at_typed_and_generic_boundaries() {
        let registry = HostFunctionRegistry::new().unwrap();
        let generic = HostFunctionDescriptor::generic(
            HostFunctionId(2001),
            "ogma:test/panic",
            0,
            0,
            generic_panic,
            ptr::null_mut(),
        )
        .unwrap();
        let typed = HostFunctionDescriptor::f64_binary(
            HostFunctionId(2002),
            "ogma:test/typed-panic",
            0,
            0,
            typed_panic,
            generic_add,
            ptr::null_mut(),
        )
        .unwrap();
        let generic_slot = registry.register(generic).unwrap();
        let typed_slot = registry.register(typed).unwrap();
        let host = scope_context();
        let scope = host.enter_scope().unwrap();
        assert_eq!(
            registry.invoke_slot(&scope, generic_slot, 0, &[]),
            Err(DispatchError::Abi(OgmaStatus::PANIC))
        );
        assert_eq!(
            registry.invoke_slot(
                &scope,
                typed_slot,
                0,
                &[DispatchValue::Number(1.0), DispatchValue::Number(2.0)]
            ),
            Err(DispatchError::HostExecution(HostError::CallbackPanicked))
        );
    }

    #[test]
    fn descriptors_reject_wrong_abi_and_unsupported_async_contracts() {
        let registry = HostFunctionRegistry::new().unwrap();
        let mut wrong = HostFunctionDescriptor::generic(
            HostFunctionId(3001),
            "ogma:test/version",
            0,
            0,
            generic_add,
            ptr::null_mut(),
        )
        .unwrap();
        wrong.abi_major = 99;
        assert_eq!(
            registry.register(wrong),
            Err(DispatchError::InvalidDescriptor)
        );
        let mut bad_name = HostFunctionDescriptor::generic(
            HostFunctionId(3002),
            "ogma:test/name",
            0,
            0,
            generic_add,
            ptr::null_mut(),
        )
        .unwrap();
        bad_name.name_len = (MAX_NAME_BYTES + 1) as u16;
        assert_eq!(
            registry.register(bad_name),
            Err(DispatchError::InvalidDescriptor)
        );
        let async_descriptor = HostFunctionDescriptor::generic(
            HostFunctionId(3003),
            "ogma:test/async",
            0,
            EFFECT_MAY_SUSPEND,
            generic_add,
            ptr::null_mut(),
        )
        .unwrap();
        assert_eq!(
            registry.register(async_descriptor),
            Err(DispatchError::InvalidDescriptor)
        );
    }

    #[test]
    fn unrelated_table_mutation_does_not_invalidate_live_slot() {
        let registry = HostFunctionRegistry::new().unwrap();
        let first = register_add(&registry, 4001, "ogma:math/first");
        let second = HostFunctionDescriptor::generic(
            HostFunctionId(4002),
            "ogma:math/second",
            0,
            0,
            generic_throw,
            ptr::null_mut(),
        )
        .unwrap();
        registry.register(second).unwrap();
        let host = scope_context();
        let scope = host.enter_scope().unwrap();
        assert_eq!(
            registry
                .invoke_slot(
                    &scope,
                    first,
                    0,
                    &[DispatchValue::Number(5.0), DispatchValue::Number(6.0)]
                )
                .unwrap(),
            JsCompletion::Normal(DispatchValue::Number(11.0))
        );
        assert_eq!(registry.table_version(), 2);
    }
    #[test]
    fn proxy_get_trap_override_controls_the_observed_function_value() {
        let registry = HostFunctionRegistry::new().unwrap();
        let slot = register_add(&registry, 1001, "ogma:math/add");
        let mut target = OrdinaryHostObject::new();
        target.define("run", JsCallable::host(slot));
        let guard = registry.resolve_call_site(&target, "run").unwrap();
        let mut proxy = ProxyHostObject::new(target);
        proxy.set_get_result("run", JsCallable::script(DispatchValue::Number(91.0)));
        let host = scope_context();
        let scope = host.enter_scope().unwrap();

        let result = registry
            .call_property(
                &scope,
                PropertyReceiver::Proxy(&proxy),
                "run",
                Some(&guard),
                0,
                &[DispatchValue::Number(2.0), DispatchValue::Number(3.0)],
            )
            .unwrap();

        assert_eq!(result, JsCompletion::Normal(DispatchValue::Number(91.0)));
        assert_eq!(proxy.get_trap_calls(), 1);
        assert_eq!(registry.metrics().typed_calls, 0);
        assert_eq!(registry.metrics().generic_calls, 0);
    }
}
