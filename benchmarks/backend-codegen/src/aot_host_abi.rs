//! OGM-32 M0 proof: generated native code dispatches through a versioned C ABI table.
//!
//! This is an integration harness over the OGM-19/21/27 prototypes. The table
//! contains only fixed-width metadata, opaque OGM handles and C-compatible
//! function pointers; Rust registry/runtime state is reachable only through
//! host-owned opaque user_data pointers and is never interpreted by native code.

use ogma_host_execution_scope_proof::{HostExecutionContext, HostExecutionScope, LibrarySyncMode};
use ogma_native_runtime_boundary_proof::CancellationToken;
use ogma_rust_abi_proof::{
    COMPLETION_NORMAL, COMPLETION_THROW, OgmaCompletion, OgmaContextHandle, OgmaHandle,
    OgmaRuntime, OgmaStatus, ogma_context_collect_unrooted, ogma_context_create_number,
    ogma_context_get_number,
};
use ogma_typed_host_dispatch_proof::{
    DispatchError, DispatchValue, EFFECT_MAY_REENTER, HostFunctionDescriptor, HostFunctionId,
    HostFunctionRegistry, HostFunctionSlot, JsCompletion, JsException, SIGNATURE_F64_BINARY,
    SIGNATURE_NUMBER_HANDLE_BINARY,
};
use std::cell::Cell;
use std::ffi::c_void;
use std::mem::size_of;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::slice;

pub const AOT_ABI_MAJOR: u16 = 1;
pub const AOT_ABI_MINOR: u16 = 0;
pub const AOT_DESCRIPTOR_VERSION: u16 = 1;

pub const AOT_VALUE_UNDEFINED: u32 = 0;
pub const AOT_VALUE_NUMBER: u32 = 1;
pub const AOT_VALUE_BOOLEAN: u32 = 2;
pub const AOT_VALUE_HANDLE: u32 = 3;

pub const AOT_COMPLETION_NORMAL: u32 = 0;
pub const AOT_COMPLETION_THROW: u32 = 1;

pub const AOT_ERROR_NONE: u32 = 0;
pub const AOT_ERROR_TYPE: u32 = 1;
pub const AOT_ERROR_RANGE: u32 = 2;
pub const AOT_ERROR_CAPABILITY: u32 = 3;
pub const AOT_ERROR_THROWN: u32 = 4;
pub const AOT_ERROR_CANCELLED: u32 = 5;
pub const AOT_ERROR_ASYNC_UNSUPPORTED: u32 = 6;
pub const AOT_ERROR_REFERENCE: u32 = 7;
pub const AOT_MAX_ARGUMENTS: usize = 4096;

pub const AOT_CAPABILITY_MATH: u64 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
pub union AotValuePayloadV1 {
    pub number: f64,
    pub boolean: u8,
    pub handle: OgmaHandle,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AotValueV1 {
    pub tag: u32,
    pub reserved: u32,
    pub payload: AotValuePayloadV1,
}

impl Default for AotValueV1 {
    fn default() -> Self {
        Self {
            tag: AOT_VALUE_UNDEFINED,
            reserved: 0,
            payload: AotValuePayloadV1 { number: 0.0 },
        }
    }
}

impl AotValueV1 {
    pub fn number(value: f64) -> Self {
        Self {
            tag: AOT_VALUE_NUMBER,
            reserved: 0,
            payload: AotValuePayloadV1 { number: value },
        }
    }

    pub fn handle(value: OgmaHandle) -> Self {
        Self {
            tag: AOT_VALUE_HANDLE,
            reserved: 0,
            payload: AotValuePayloadV1 { handle: value },
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AotCompletionV1 {
    pub kind: u32,
    pub error_kind: u32,
    pub details: u64,
    pub value: AotValueV1,
}

impl Default for AotCompletionV1 {
    fn default() -> Self {
        Self {
            kind: AOT_COMPLETION_NORMAL,
            error_kind: AOT_ERROR_NONE,
            details: 0,
            value: AotValueV1::default(),
        }
    }
}

pub type AotPrimitiveF64Thunk = unsafe extern "C" fn(
    user_data: *mut c_void,
    capabilities: u64,
    left: f64,
    right: f64,
    out_completion: *mut AotCompletionV1,
) -> OgmaStatus;

pub type AotNumberHandleF64Thunk = unsafe extern "C" fn(
    user_data: *mut c_void,
    capabilities: u64,
    left: OgmaHandle,
    right: OgmaHandle,
    out_completion: *mut AotCompletionV1,
) -> OgmaStatus;

pub type AotGenericThunk = unsafe extern "C" fn(
    user_data: *mut c_void,
    capabilities: u64,
    args: *const AotValueV1,
    args_len: usize,
    out_completion: *mut AotCompletionV1,
) -> OgmaStatus;

/// Versioned entry in the native AOT thunk/function table.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AotHostFunctionEntryV1 {
    pub struct_size: u32,
    pub descriptor_version: u16,
    pub abi_major: u16,
    pub abi_minor: u16,
    pub reserved: u16,
    pub function_id: u64,
    pub signature: u32,
    pub effects: u32,
    pub signature_id: u64,
    pub required_capabilities: u64,
    pub primitive_f64_thunk: Option<AotPrimitiveF64Thunk>,
    pub number_handle_f64_thunk: Option<AotNumberHandleF64Thunk>,
    pub generic_thunk: Option<AotGenericThunk>,
    pub user_data: *mut c_void,
}

/// Versioned table passed to generated native functions. Entries and opaque
/// user_data pointers are host-owned and must outlive every native call using it.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AotHostFunctionTableV1 {
    pub struct_size: u32,
    pub abi_major: u16,
    pub abi_minor: u16,
    pub entry_size: u32,
    pub entry_count: u32,
    pub entries: *const AotHostFunctionEntryV1,
    pub capabilities: u64,
    pub table_version: u64,
    pub reserved: u64,
}

struct CallbackState {
    registry: Cell<*const HostFunctionRegistry>,
    active_scope: Cell<*const ()>,
    active_capabilities: Cell<u64>,
    nested_slot: Cell<Option<HostFunctionSlot>>,
    generic_calls: Cell<u64>,
    nested_calls: Cell<u64>,
    cancellation: CancellationToken,
}

struct AotBinding {
    registry: *const HostFunctionRegistry,
    execution: *const HostExecutionContext,
    callback_state: *const CallbackState,
    slot: HostFunctionSlot,
}

struct ActiveScopeGuard<'a> {
    state: &'a CallbackState,
    previous_scope: *const (),
    previous_capabilities: u64,
}

impl<'a> ActiveScopeGuard<'a> {
    fn install(
        state: &'a CallbackState,
        scope: *const HostExecutionScope<'_>,
        capabilities: u64,
    ) -> Self {
        let previous_scope = state.active_scope.replace(scope.cast::<()>());
        let previous_capabilities = state.active_capabilities.replace(capabilities);
        Self {
            state,
            previous_scope,
            previous_capabilities,
        }
    }
}

impl Drop for ActiveScopeGuard<'_> {
    fn drop(&mut self) {
        self.state.active_scope.set(self.previous_scope);
        self.state
            .active_capabilities
            .set(self.previous_capabilities);
    }
}

pub struct AotHostFunctionTable {
    registry: Box<HostFunctionRegistry>,
    execution: Box<HostExecutionContext>,
    callback_state: Box<CallbackState>,
    // Stable heap addresses are published in the C ABI table as opaque user_data.
    #[allow(clippy::vec_box)]
    bindings: Vec<Box<AotBinding>>,
    entries: Vec<AotHostFunctionEntryV1>,
    table: AotHostFunctionTableV1,
}

impl AotHostFunctionTable {
    pub fn new(cancellation: CancellationToken) -> Result<Self, DispatchError> {
        let registry = Box::new(HostFunctionRegistry::new()?);
        let execution = Box::new(HostExecutionContext::new(LibrarySyncMode::None));
        let callback_state = Box::new(CallbackState {
            registry: Cell::new((&*registry) as *const HostFunctionRegistry),
            active_scope: Cell::new(std::ptr::null()),
            active_capabilities: Cell::new(AOT_CAPABILITY_MATH),
            nested_slot: Cell::new(None),
            generic_calls: Cell::new(0),
            nested_calls: Cell::new(0),
            cancellation,
        });
        let callback_user_data = (&*callback_state as *const CallbackState)
            .cast_mut()
            .cast::<c_void>();

        let typed_descriptor = HostFunctionDescriptor::f64_binary(
            HostFunctionId(32_001),
            "ogma:aot/math/typed-add",
            AOT_CAPABILITY_MATH,
            0,
            typed_f64_add,
            generic_add_callback,
            callback_user_data,
        )?;
        let typed_slot = registry.register(typed_descriptor)?;

        let generic_descriptor = HostFunctionDescriptor::f64_binary(
            HostFunctionId(32_002),
            "ogma:aot/math/generic-add",
            AOT_CAPABILITY_MATH,
            EFFECT_MAY_REENTER,
            typed_f64_add,
            generic_add_callback,
            callback_user_data,
        )?;
        let generic_slot = registry.register(generic_descriptor)?;

        let handle_descriptor = HostFunctionDescriptor::number_handle_binary(
            HostFunctionId(32_003),
            "ogma:aot/math/handle-add",
            AOT_CAPABILITY_MATH,
            0,
            typed_handle_add,
            generic_add_callback,
            callback_user_data,
        )?;
        let handle_slot = registry.register(handle_descriptor)?;

        let generic_handle_descriptor = HostFunctionDescriptor::number_handle_binary(
            HostFunctionId(32_004),
            "ogma:aot/math/generic-handle-add",
            AOT_CAPABILITY_MATH,
            EFFECT_MAY_REENTER,
            typed_handle_add,
            generic_add_callback,
            callback_user_data,
        )?;
        let generic_handle_slot = registry.register(generic_handle_descriptor)?;

        let nested_descriptor = HostFunctionDescriptor::f64_binary(
            HostFunctionId(32_099),
            "ogma:aot/internal/reentrant-add",
            AOT_CAPABILITY_MATH,
            0,
            typed_f64_add,
            generic_add_callback,
            callback_user_data,
        )?;
        let nested_slot = registry.register(nested_descriptor)?;
        callback_state.nested_slot.set(Some(nested_slot));

        let mut result = Self {
            registry,
            execution,
            callback_state,
            bindings: Vec::new(),
            entries: Vec::new(),
            table: AotHostFunctionTableV1 {
                struct_size: size_of::<AotHostFunctionTableV1>() as u32,
                abi_major: AOT_ABI_MAJOR,
                abi_minor: AOT_ABI_MINOR,
                entry_size: size_of::<AotHostFunctionEntryV1>() as u32,
                entry_count: 0,
                entries: std::ptr::null(),
                capabilities: AOT_CAPABILITY_MATH,
                table_version: 0,
                reserved: 0,
            },
        };
        result.push_entry(typed_descriptor, typed_slot);
        result.push_entry(generic_descriptor, generic_slot);
        result.push_entry(handle_descriptor, handle_slot);
        result.push_entry(generic_handle_descriptor, generic_handle_slot);
        result.table.entry_count = result.entries.len() as u32;
        result.table.entries = result.entries.as_ptr();
        result.table.table_version = result.registry.table_version();
        result.validate()?;
        Ok(result)
    }

    fn push_entry(&mut self, descriptor: HostFunctionDescriptor, slot: HostFunctionSlot) {
        let binding = Box::new(AotBinding {
            registry: (&*self.registry) as *const HostFunctionRegistry,
            execution: (&*self.execution) as *const HostExecutionContext,
            callback_state: (&*self.callback_state) as *const CallbackState,
            slot,
        });
        let signature = descriptor.signature;
        let binding_ptr = (&*binding as *const AotBinding).cast_mut().cast::<c_void>();
        self.bindings.push(binding);
        self.entries.push(AotHostFunctionEntryV1 {
            struct_size: size_of::<AotHostFunctionEntryV1>() as u32,
            descriptor_version: AOT_DESCRIPTOR_VERSION,
            abi_major: descriptor.abi_major,
            abi_minor: descriptor.abi_minor,
            reserved: 0,
            function_id: descriptor.function_id,
            signature,
            effects: descriptor.effects,
            signature_id: descriptor.signature_id,
            required_capabilities: descriptor.required_capabilities,
            primitive_f64_thunk: if signature == 1 {
                Some(aot_primitive_f64_thunk)
            } else {
                None
            },
            number_handle_f64_thunk: if signature == 2 {
                Some(aot_number_handle_f64_thunk)
            } else {
                None
            },
            generic_thunk: Some(aot_generic_thunk),
            user_data: binding_ptr,
        });
    }

    pub fn as_abi(&self) -> &AotHostFunctionTableV1 {
        &self.table
    }

    pub fn registry(&self) -> &HostFunctionRegistry {
        &self.registry
    }

    pub fn callback_stats(&self) -> (u64, u64) {
        (
            self.callback_state.generic_calls.get(),
            self.callback_state.nested_calls.get(),
        )
    }

    /// Interpreter-side oracle using the same OGM-21 registry and descriptor slot.
    /// This is used to compare the native AOT thunk with the existing dispatch path.
    pub fn invoke_interpreter(
        &self,
        entry_index: usize,
        capabilities: u64,
        args: &[DispatchValue],
        force_generic: bool,
    ) -> Result<JsCompletion, DispatchError> {
        let binding = self
            .bindings
            .get(entry_index)
            .ok_or(DispatchError::InvalidDescriptor)?;
        let scope = self
            .execution
            .enter_scope()
            .map_err(DispatchError::HostExecution)?;
        let _active_scope = force_generic.then(|| {
            ActiveScopeGuard::install(
                &self.callback_state,
                &scope as *const HostExecutionScope<'_>,
                capabilities,
            )
        });
        if force_generic {
            self.registry
                .invoke_generic_slot(&scope, binding.slot, capabilities, args)
        } else {
            self.registry
                .invoke_slot(&scope, binding.slot, capabilities, args)
        }
    }

    pub fn validate(&self) -> Result<(), DispatchError> {
        validate_table_header(&self.table)?;
        if self.table.entry_count as usize != self.entries.len()
            || self.table.entries != self.entries.as_ptr()
            || self.table.table_version != self.registry.table_version()
        {
            return Err(DispatchError::InvalidDescriptor);
        }
        for entry in &self.entries {
            validate_entry(entry)?;
        }
        Ok(())
    }
}

pub fn validate_table_header(table: &AotHostFunctionTableV1) -> Result<(), DispatchError> {
    if table.struct_size as usize != size_of::<AotHostFunctionTableV1>()
        || table.abi_major != AOT_ABI_MAJOR
        || table.abi_minor > AOT_ABI_MINOR
        || table.entry_size as usize != size_of::<AotHostFunctionEntryV1>()
        || table.entry_count == 0
        || table.entry_count > 4096
        || table.entries.is_null()
        || table.table_version == 0
        || table.reserved != 0
    {
        return Err(DispatchError::InvalidDescriptor);
    }
    Ok(())
}

fn validate_entry(entry: &AotHostFunctionEntryV1) -> Result<(), DispatchError> {
    if entry.struct_size as usize != size_of::<AotHostFunctionEntryV1>()
        || entry.descriptor_version != AOT_DESCRIPTOR_VERSION
        || entry.abi_major != AOT_ABI_MAJOR
        || entry.abi_minor > AOT_ABI_MINOR
        || entry.reserved != 0
        || entry.function_id == 0
        || entry.user_data.is_null()
        || entry.generic_thunk.is_none()
    {
        return Err(DispatchError::InvalidDescriptor);
    }
    match entry.signature {
        1 if entry.signature_id == SIGNATURE_F64_BINARY
            && entry.primitive_f64_thunk.is_some()
            && entry.number_handle_f64_thunk.is_none() => {}
        2 if entry.signature_id == SIGNATURE_NUMBER_HANDLE_BINARY
            && entry.primitive_f64_thunk.is_none()
            && entry.number_handle_f64_thunk.is_some() => {}
        _ => return Err(DispatchError::InvalidDescriptor),
    }
    Ok(())
}

fn map_dispatch_error(error: DispatchError) -> OgmaStatus {
    match error {
        DispatchError::Abi(status) | DispatchError::CleanupFailure(status) => status,
        DispatchError::HostExecution(ogma_host_execution_scope_proof::HostError::WrongThread)
        | DispatchError::WrongThread => OgmaStatus::WRONG_THREAD,
        DispatchError::HostExecution(
            ogma_host_execution_scope_proof::HostError::ScopeDepthExceeded,
        )
        | DispatchError::Busy => OgmaStatus::BUSY,
        DispatchError::HostExecution(
            ogma_host_execution_scope_proof::HostError::ScopeAlreadyActive,
        )
        | DispatchError::HostExecution(ogma_host_execution_scope_proof::HostError::NoActiveScope)
        | DispatchError::HostExecution(
            ogma_host_execution_scope_proof::HostError::BorrowConflict,
        )
        | DispatchError::HostExecution(
            ogma_host_execution_scope_proof::HostError::CallbackPanicked,
        ) => OgmaStatus::CALLBACK_ERROR,
        DispatchError::StaleSlot | DispatchError::WrongRegistry => OgmaStatus::STALE_HANDLE,
        DispatchError::TooManyArguments => OgmaStatus::OUT_OF_RANGE,
        DispatchError::UnsupportedValue => OgmaStatus::TYPE_MISMATCH,
        _ => OgmaStatus::INTERNAL_ERROR,
    }
}

fn encode_value(value: DispatchValue) -> AotValueV1 {
    match value {
        DispatchValue::Undefined => AotValueV1::default(),
        DispatchValue::Number(number) => AotValueV1::number(number),
        DispatchValue::Boolean(boolean) => AotValueV1 {
            tag: AOT_VALUE_BOOLEAN,
            reserved: 0,
            payload: AotValuePayloadV1 {
                boolean: u8::from(boolean),
            },
        },
        DispatchValue::Handle(handle) => AotValueV1::handle(handle),
    }
}

fn encode_completion(completion: JsCompletion) -> AotCompletionV1 {
    match completion {
        JsCompletion::Normal(value) => AotCompletionV1 {
            kind: AOT_COMPLETION_NORMAL,
            error_kind: AOT_ERROR_NONE,
            details: 0,
            value: encode_value(value),
        },
        JsCompletion::Throw(exception) => {
            let (error_kind, details, value) = match exception {
                JsException::TypeError => (AOT_ERROR_TYPE, 0, AotValueV1::default()),
                JsException::RangeError => (AOT_ERROR_RANGE, 0, AotValueV1::default()),
                JsException::CapabilityDenied { missing } => {
                    (AOT_ERROR_CAPABILITY, missing, AotValueV1::default())
                }
                JsException::Thrown(value) => (AOT_ERROR_THROWN, 0, encode_value(value)),
                JsException::AsyncNotSupported => {
                    (AOT_ERROR_ASYNC_UNSUPPORTED, 0, AotValueV1::default())
                }
                JsException::ReferenceError => (AOT_ERROR_REFERENCE, 0, AotValueV1::default()),
            };
            AotCompletionV1 {
                kind: AOT_COMPLETION_THROW,
                error_kind,
                details,
                value,
            }
        }
    }
}

fn decode_value(value: AotValueV1) -> Result<DispatchValue, OgmaStatus> {
    if value.reserved != 0 {
        return Err(OgmaStatus::INVALID_ARGUMENT);
    }
    match value.tag {
        AOT_VALUE_UNDEFINED => Ok(DispatchValue::Undefined),
        AOT_VALUE_NUMBER => Ok(DispatchValue::Number(unsafe { value.payload.number })),
        AOT_VALUE_BOOLEAN => match unsafe { value.payload.boolean } {
            0 => Ok(DispatchValue::Boolean(false)),
            1 => Ok(DispatchValue::Boolean(true)),
            _ => Err(OgmaStatus::INVALID_ARGUMENT),
        },
        AOT_VALUE_HANDLE => Ok(DispatchValue::Handle(unsafe { value.payload.handle })),
        _ => Err(OgmaStatus::INVALID_ARGUMENT),
    }
}

fn invoke_binding(
    binding: *mut c_void,
    capabilities: u64,
    args: &[DispatchValue],
    force_generic: bool,
    out_completion: *mut AotCompletionV1,
) -> OgmaStatus {
    if out_completion.is_null() {
        return OgmaStatus::INVALID_ARGUMENT;
    }
    unsafe { out_completion.write(AotCompletionV1::default()) };
    if binding.is_null() {
        return OgmaStatus::INVALID_ARGUMENT;
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: C receives these opaque pointers only from this table's entries.
        // The table retains every Box until all generated calls have completed.
        let binding = unsafe { &*binding.cast::<AotBinding>() };
        // SAFETY: AotBinding captures stable pointers to Box-owned host state; the
        // table and its registry/execution/callback state outlive the native call.
        let registry = unsafe { &*binding.registry };
        // SAFETY: same lifetime invariant as the registry pointer above.
        let execution = unsafe { &*binding.execution };
        // SAFETY: same lifetime invariant as the registry pointer above.
        let callback_state = unsafe { &*binding.callback_state };
        let scope = execution
            .enter_scope()
            .map_err(DispatchError::HostExecution)?;
        let _active_scope = force_generic.then(|| {
            // The pointer is published only during this synchronous callback
            // and is cleared by Drop on normal return, error or panic.
            ActiveScopeGuard::install(
                callback_state,
                &scope as *const HostExecutionScope<'_>,
                capabilities,
            )
        });
        let completion = if force_generic {
            registry.invoke_generic_slot(&scope, binding.slot, capabilities, args)?
        } else {
            registry.invoke_slot(&scope, binding.slot, capabilities, args)?
        };
        if callback_state.cancellation.is_cancelled() {
            unsafe {
                out_completion.write(AotCompletionV1 {
                    kind: AOT_COMPLETION_THROW,
                    error_kind: AOT_ERROR_CANCELLED,
                    details: 0,
                    value: AotValueV1::default(),
                });
            }
        } else {
            unsafe { out_completion.write(encode_completion(completion)) };
        }
        Ok::<(), DispatchError>(())
    }));
    match result {
        Ok(Ok(())) => OgmaStatus::OK,
        Ok(Err(error)) => map_dispatch_error(error),
        Err(_) => OgmaStatus::PANIC,
    }
}

unsafe extern "C" fn aot_primitive_f64_thunk(
    user_data: *mut c_void,
    capabilities: u64,
    left: f64,
    right: f64,
    out_completion: *mut AotCompletionV1,
) -> OgmaStatus {
    invoke_binding(
        user_data,
        capabilities,
        &[DispatchValue::Number(left), DispatchValue::Number(right)],
        false,
        out_completion,
    )
}

unsafe extern "C" fn aot_number_handle_f64_thunk(
    user_data: *mut c_void,
    capabilities: u64,
    left: OgmaHandle,
    right: OgmaHandle,
    out_completion: *mut AotCompletionV1,
) -> OgmaStatus {
    invoke_binding(
        user_data,
        capabilities,
        &[DispatchValue::Handle(left), DispatchValue::Handle(right)],
        false,
        out_completion,
    )
}

unsafe extern "C" fn aot_generic_thunk(
    user_data: *mut c_void,
    capabilities: u64,
    args: *const AotValueV1,
    args_len: usize,
    out_completion: *mut AotCompletionV1,
) -> OgmaStatus {
    if out_completion.is_null() {
        return OgmaStatus::INVALID_ARGUMENT;
    }
    unsafe { out_completion.write(AotCompletionV1::default()) };
    if user_data.is_null()
        || args_len > AOT_MAX_ARGUMENTS
        || (args_len > 0 && args.is_null())
    {
        return OgmaStatus::INVALID_ARGUMENT;
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        let decoded = if args_len == 0 {
            Vec::new()
        } else {
            let raw = unsafe { slice::from_raw_parts(args, args_len) };
            let mut decoded = Vec::with_capacity(raw.len());
            for value in raw {
                match decode_value(*value) {
                    Ok(value) => decoded.push(value),
                    Err(status) => return status,
                }
            }
            decoded
        };
        invoke_binding(user_data, capabilities, &decoded, true, out_completion)
    }));
    result.unwrap_or(OgmaStatus::PANIC)
}

unsafe extern "C-unwind" fn typed_f64_add(
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

unsafe extern "C-unwind" fn typed_handle_add(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    left: OgmaHandle,
    right: OgmaHandle,
    out: *mut f64,
    _user_data: *mut c_void,
) -> OgmaStatus {
    if runtime.is_null() || out.is_null() {
        return OgmaStatus::INVALID_ARGUMENT;
    }
    let mut left_number = 0.0;
    let mut right_number = 0.0;
    let left_status = unsafe { ogma_context_get_number(runtime, context, left, &mut left_number) };
    if left_status != OgmaStatus::OK {
        return left_status;
    }
    let right_status =
        unsafe { ogma_context_get_number(runtime, context, right, &mut right_number) };
    if right_status != OgmaStatus::OK {
        return right_status;
    }
    unsafe { out.write(left_number + right_number) };
    OgmaStatus::OK
}

unsafe extern "C-unwind" fn generic_add_callback(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    args: *const OgmaHandle,
    args_len: usize,
    completion: *mut OgmaCompletion,
    user_data: *mut c_void,
) -> OgmaStatus {
    if runtime.is_null()
        || args.is_null()
        || completion.is_null()
        || user_data.is_null()
        || args_len != 2
    {
        return OgmaStatus::INVALID_ARGUMENT;
    }
    let result = catch_unwind(AssertUnwindSafe(|| unsafe {
        let state = &*user_data.cast::<CallbackState>();
        let args = slice::from_raw_parts(args, args_len);
        let mut left = 0.0;
        let mut right = 0.0;
        let status = ogma_context_get_number(runtime, context, args[0], &mut left);
        if status != OgmaStatus::OK {
            return status;
        }
        let status = ogma_context_get_number(runtime, context, args[1], &mut right);
        if status != OgmaStatus::OK {
            return status;
        }

        // Collect while OGM-19 owns the callback's borrowed argument roots.
        let status = ogma_context_collect_unrooted(runtime, context);
        if status != OgmaStatus::OK {
            return status;
        }
        let mut left_after = 0.0;
        let mut right_after = 0.0;
        let status = ogma_context_get_number(runtime, context, args[0], &mut left_after);
        if status != OgmaStatus::OK || left_after.to_bits() != left.to_bits() {
            return if status == OgmaStatus::OK {
                OgmaStatus::INTERNAL_ERROR
            } else {
                status
            };
        }
        let status = ogma_context_get_number(runtime, context, args[1], &mut right_after);
        if status != OgmaStatus::OK || right_after.to_bits() != right.to_bits() {
            return if status == OgmaStatus::OK {
                OgmaStatus::INTERNAL_ERROR
            } else {
                status
            };
        }
        state
            .generic_calls
            .set(state.generic_calls.get().saturating_add(1));

        if left == 99.0 {
            state.cancellation.cancel();
        }

        let value = if left == 12.0 {
            let scope_pointer = state.active_scope.get();
            if scope_pointer.is_null() {
                return OgmaStatus::INTERNAL_ERROR;
            }
            let registry_pointer = state.registry.get();
            let nested_slot = match state.nested_slot.get() {
                Some(slot) => slot,
                None => return OgmaStatus::INTERNAL_ERROR,
            };
            if registry_pointer.is_null() {
                return OgmaStatus::INTERNAL_ERROR;
            }
            // SAFETY: ActiveScopeGuard installs this pointer only for the
            // synchronous generic dispatch. It remains alive through this callback,
            // on the HostExecutionContext owner thread, and is restored before the
            // outer scope is dropped. The nested scope cannot escape this callback.
            let scope = &*scope_pointer.cast::<HostExecutionScope<'static>>();
            let nested_scope = match scope.nested_scope() {
                Ok(scope) => scope,
                Err(_) => return OgmaStatus::BUSY,
            };
            let registry = &*registry_pointer;
            let nested = match registry.invoke_slot(
                &nested_scope,
                nested_slot,
                state.active_capabilities.get(),
                &[DispatchValue::Number(left), DispatchValue::Number(right)],
            ) {
                Ok(JsCompletion::Normal(DispatchValue::Number(value))) => value,
                _ => return OgmaStatus::INTERNAL_ERROR,
            };
            state
                .nested_calls
                .set(state.nested_calls.get().saturating_add(1));
            nested
        } else {
            left + right
        };

        if left < 0.0 {
            completion.write(OgmaCompletion {
                kind: COMPLETION_THROW,
                reserved: 0,
                value: args[0],
            });
            return OgmaStatus::OK;
        }

        let mut value_handle = OgmaHandle::default();
        let status = ogma_context_create_number(runtime, context, value, &mut value_handle);
        if status != OgmaStatus::OK {
            return status;
        }
        completion.write(OgmaCompletion {
            kind: COMPLETION_NORMAL,
            reserved: 0,
            value: value_handle,
        });
        OgmaStatus::OK
    }));
    match result {
        Ok(status) => status,
        Err(_) => OgmaStatus::PANIC,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invoke_f64(
        entry: &AotHostFunctionEntryV1,
        capabilities: u64,
        left: f64,
        right: f64,
    ) -> (OgmaStatus, AotCompletionV1) {
        let mut completion = AotCompletionV1::default();
        let thunk = entry.primitive_f64_thunk.expect("typed f64 thunk");
        let status = unsafe { thunk(entry.user_data, capabilities, left, right, &mut completion) };
        (status, completion)
    }

    fn invoke_handles(
        entry: &AotHostFunctionEntryV1,
        capabilities: u64,
        left: OgmaHandle,
        right: OgmaHandle,
    ) -> (OgmaStatus, AotCompletionV1) {
        let mut completion = AotCompletionV1::default();
        let thunk = entry.number_handle_f64_thunk.expect("typed handle thunk");
        let status = unsafe { thunk(entry.user_data, capabilities, left, right, &mut completion) };
        (status, completion)
    }

    fn invoke_generic(
        entry: &AotHostFunctionEntryV1,
        capabilities: u64,
        args: &[AotValueV1],
    ) -> (OgmaStatus, AotCompletionV1) {
        let mut completion = AotCompletionV1::default();
        let thunk = entry.generic_thunk.expect("generic fallback thunk");
        let status = unsafe {
            thunk(
                entry.user_data,
                capabilities,
                if args.is_empty() {
                    std::ptr::null()
                } else {
                    args.as_ptr()
                },
                args.len(),
                &mut completion,
            )
        };
        (status, completion)
    }

    fn number(completion: AotCompletionV1) -> f64 {
        assert_eq!(completion.kind, AOT_COMPLETION_NORMAL);
        assert_eq!(completion.error_kind, AOT_ERROR_NONE);
        assert_eq!(completion.value.tag, AOT_VALUE_NUMBER);
        unsafe { completion.value.payload.number }
    }

    #[test]
    fn c_abi_layout_is_fixed_and_versioned() {
        assert_eq!(std::mem::size_of::<AotValuePayloadV1>(), 40);
        assert_eq!(std::mem::size_of::<AotValueV1>(), 48);
        assert_eq!(std::mem::offset_of!(AotValueV1, payload), 8);
        assert_eq!(std::mem::size_of::<AotCompletionV1>(), 64);
        assert_eq!(std::mem::offset_of!(AotCompletionV1, value), 16);
        assert_eq!(std::mem::size_of::<AotHostFunctionEntryV1>(), 80);
        assert_eq!(
            std::mem::offset_of!(AotHostFunctionEntryV1, function_id),
            16
        );
        assert_eq!(
            std::mem::offset_of!(AotHostFunctionEntryV1, signature_id),
            32
        );
        assert_eq!(
            std::mem::offset_of!(AotHostFunctionEntryV1, primitive_f64_thunk),
            48
        );
        assert_eq!(std::mem::offset_of!(AotHostFunctionEntryV1, user_data), 72);
        assert_eq!(std::mem::size_of::<AotHostFunctionTableV1>(), 48);
        assert_eq!(std::mem::offset_of!(AotHostFunctionTableV1, entries), 16);
        assert_eq!(
            std::mem::offset_of!(AotHostFunctionTableV1, table_version),
            32
        );
    }

    #[test]
    fn table_and_entry_metadata_are_consistent_and_reject_version_mismatch() {
        let table = AotHostFunctionTable::new(CancellationToken::default()).unwrap();
        table.validate().unwrap();
        assert_eq!(table.as_abi().abi_major, AOT_ABI_MAJOR);
        assert_eq!(table.as_abi().abi_minor, AOT_ABI_MINOR);
        assert_eq!(table.as_abi().entry_count, 4);
        assert_eq!(table.registry().table_version(), 5);
        assert_eq!(table.entries[0].signature_id, SIGNATURE_F64_BINARY);
        assert_eq!(table.entries[1].effects, EFFECT_MAY_REENTER);
        assert_eq!(
            table.entries[2].signature_id,
            SIGNATURE_NUMBER_HANDLE_BINARY
        );

        let mut wrong_version = *table.as_abi();
        wrong_version.abi_major = AOT_ABI_MAJOR + 1;
        assert_eq!(
            validate_table_header(&wrong_version),
            Err(DispatchError::InvalidDescriptor)
        );

        let mut malformed = *table.as_abi();
        malformed.reserved = 1;
        assert_eq!(
            validate_table_header(&malformed),
            Err(DispatchError::InvalidDescriptor)
        );

        let mut entry = table.entries[0];
        entry.signature_id += 1;
        assert_eq!(
            validate_entry(&entry),
            Err(DispatchError::InvalidDescriptor)
        );
    }

    #[test]
    fn interpreter_dispatch_oracle_preserves_typed_generic_and_throw_semantics() {
        let table = AotHostFunctionTable::new(CancellationToken::default()).unwrap();

        assert_eq!(
            table.invoke_interpreter(
                0,
                AOT_CAPABILITY_MATH,
                &[DispatchValue::Number(31.0), DispatchValue::Number(11.0)],
                false,
            ),
            Ok(JsCompletion::Normal(DispatchValue::Number(42.0)))
        );
        assert_eq!(
            table.invoke_interpreter(
                1,
                AOT_CAPABILITY_MATH,
                &[DispatchValue::Number(12.0), DispatchValue::Number(30.0)],
                true,
            ),
            Ok(JsCompletion::Normal(DispatchValue::Number(42.0)))
        );
        assert_eq!(
            table.invoke_interpreter(
                1,
                AOT_CAPABILITY_MATH,
                &[DispatchValue::Number(-7.0), DispatchValue::Number(1.0)],
                true,
            ),
            Ok(JsCompletion::Throw(JsException::Thrown(
                DispatchValue::Number(-7.0)
            )))
        );
    }

    #[test]
    fn generic_thunk_initializes_completion_when_rejecting_invalid_arguments() {
        let table = AotHostFunctionTable::new(CancellationToken::default()).unwrap();
        let entry = &table.entries[1];
        let thunk = entry.generic_thunk.expect("generic fallback thunk");
        let mut completion = AotCompletionV1 {
            kind: AOT_COMPLETION_THROW,
            error_kind: AOT_ERROR_TYPE,
            details: u64::MAX,
            value: AotValueV1::number(-1.0),
        };
        let status = unsafe {
            thunk(
                entry.user_data,
                AOT_CAPABILITY_MATH,
                std::ptr::null(),
                AOT_MAX_ARGUMENTS + 1,
                &mut completion,
            )
        };
        assert_eq!(status, OgmaStatus::INVALID_ARGUMENT);
        assert_eq!(completion.kind, AOT_COMPLETION_NORMAL);
        assert_eq!(completion.error_kind, AOT_ERROR_NONE);
        assert_eq!(completion.details, 0);
        assert_eq!(completion.value.tag, AOT_VALUE_UNDEFINED);
        assert_eq!(completion.value.reserved, 0);
    }

    #[test]
    fn native_primitive_and_opaque_handle_thunks_preserve_results_and_roots() {
        let table = AotHostFunctionTable::new(CancellationToken::default()).unwrap();
        let (status, primitive) = invoke_f64(&table.entries[0], AOT_CAPABILITY_MATH, 19.0, 23.0);
        assert_eq!(status, OgmaStatus::OK);
        assert_eq!(number(primitive), 42.0);

        let left = table.registry().create_number(19.0).unwrap();
        let right = table.registry().create_number(23.0).unwrap();
        let _left_root = table.registry().root_handle(left).unwrap();
        let _right_root = table.registry().root_handle(right).unwrap();
        table.registry().collect_unrooted().unwrap();
        assert_eq!(table.registry().read_number(left).unwrap(), 19.0);
        assert_eq!(table.registry().read_number(right).unwrap(), 23.0);

        let (status, handles) = invoke_handles(&table.entries[2], AOT_CAPABILITY_MATH, left, right);
        assert_eq!(status, OgmaStatus::OK);
        assert_eq!(number(handles), 42.0);
    }

    #[test]
    fn generic_fallback_preserves_throw_completion_and_callback_reentrancy() {
        let table = AotHostFunctionTable::new(CancellationToken::default()).unwrap();
        let (status, normal) = invoke_generic(
            &table.entries[1],
            AOT_CAPABILITY_MATH,
            &[AotValueV1::number(12.0), AotValueV1::number(30.0)],
        );
        assert_eq!(status, OgmaStatus::OK);
        assert_eq!(number(normal), 42.0);
        let (generic_calls, nested_calls) = table.callback_stats();
        assert_eq!(generic_calls, 1);
        assert_eq!(nested_calls, 1);

        let (status, thrown) = invoke_generic(
            &table.entries[1],
            AOT_CAPABILITY_MATH,
            &[AotValueV1::number(-7.0), AotValueV1::number(1.0)],
        );
        assert_eq!(status, OgmaStatus::OK);
        assert_eq!(thrown.kind, AOT_COMPLETION_THROW);
        assert_eq!(thrown.error_kind, AOT_ERROR_THROWN);
        assert_eq!(thrown.value.tag, AOT_VALUE_NUMBER);
        assert_eq!(unsafe { thrown.value.payload.number }, -7.0);
    }

    #[test]
    fn signature_mismatch_and_capability_denial_are_script_completions() {
        let table = AotHostFunctionTable::new(CancellationToken::default()).unwrap();
        let boolean = table.registry().create_boolean(true).unwrap();
        let number = table.registry().create_number(1.0).unwrap();
        let _boolean_root = table.registry().root_handle(boolean).unwrap();
        let _number_root = table.registry().root_handle(number).unwrap();

        let (status, mismatch) =
            invoke_handles(&table.entries[2], AOT_CAPABILITY_MATH, boolean, number);
        assert_eq!(status, OgmaStatus::OK);
        assert_eq!(mismatch.kind, AOT_COMPLETION_THROW);
        assert_eq!(mismatch.error_kind, AOT_ERROR_TYPE);

        let (status, denied) = invoke_f64(&table.entries[0], 0, 1.0, 2.0);
        assert_eq!(status, OgmaStatus::OK);
        assert_eq!(denied.kind, AOT_COMPLETION_THROW);
        assert_eq!(denied.error_kind, AOT_ERROR_CAPABILITY);
        assert_eq!(denied.details, AOT_CAPABILITY_MATH);
    }

    #[test]
    fn cancellation_is_propagated_through_the_native_thunk_completion() {
        let cancellation = CancellationToken::default();
        let table = AotHostFunctionTable::new(cancellation.clone()).unwrap();
        let (status, completion) = invoke_generic(
            &table.entries[1],
            AOT_CAPABILITY_MATH,
            &[AotValueV1::number(99.0), AotValueV1::number(1.0)],
        );
        assert_eq!(status, OgmaStatus::OK);
        assert!(cancellation.is_cancelled());
        assert_eq!(completion.kind, AOT_COMPLETION_THROW);
        assert_eq!(completion.error_kind, AOT_ERROR_CANCELLED);
    }
}
