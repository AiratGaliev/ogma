//! Safe Rust embedding boundary for the production Ogma Runtime.
//!
//! Applications compile source to verified OGMB artifacts with the compiler
//! crate, then load those artifacts here. This crate deliberately does not
//! expose Heap, GC cells, VM frames, object layouts or Rust-ABI function
//! pointers as an embedding contract.

#![deny(missing_docs)]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use ogma_interpreter::{
    CancellationToken, ExecutableModule, ExecutionCompletion, ExecutionInterruption,
    ExecutionOptions, ExecutionReport, Host, HostCallCompletion, HostCallContext, HostError,
    InstructionLocation, InterpreterError,
};
use ogma_runtime::{
    CollectionReport, Heap, HeapConfig, HeapError, HeapStats, HostHandle, ScopeToken, Value,
    ValueType,
};

/// ABI major version implemented by this crate.
pub const ABI_VERSION: u16 = 1;
/// ABI minor version implemented by this crate.
pub const ABI_MINOR: u16 = 0;
/// ABI patch version implemented by this crate.
pub const ABI_PATCH: u16 = 0;

/// Feature bit for opaque generation-checked value handles.
pub const FEATURE_OPAQUE_HANDLES: u64 = 1 << 0;
/// Feature bit for explicit temporary root scopes.
pub const FEATURE_ROOT_SCOPES: u64 = 1 << 1;
/// Feature bit for persistent host roots.
pub const FEATURE_PERSISTENT_HANDLES: u64 = 1 << 2;
/// Feature bit for synchronous host callbacks.
pub const FEATURE_CALLBACKS: u64 = 1 << 3;
/// Feature bit for explicit Normal and Throw completions.
pub const FEATURE_COMPLETIONS: u64 = 1 << 4;

/// All production ABI-v1 features implemented by the embedding boundary.
pub const ABI_FEATURES: u64 = FEATURE_OPAQUE_HANDLES
    | FEATURE_ROOT_SCOPES
    | FEATURE_PERSISTENT_HANDLES
    | FEATURE_CALLBACKS
    | FEATURE_COMPLETIONS;

const DESCRIPTOR_VERSION: u16 = 1;
const MAX_HOST_NAME_BYTES: usize = 128;
const MAX_HOST_ARGUMENTS: usize = 4096;
const KNOWN_EFFECTS: u32 = EFFECT_MAY_ALLOCATE
    | EFFECT_MAY_GC
    | EFFECT_MAY_THROW
    | EFFECT_MAY_SUSPEND
    | EFFECT_MAY_REENTER;
const COMPLEX_EFFECTS: u32 =
    EFFECT_MAY_ALLOCATE | EFFECT_MAY_GC | EFFECT_MAY_THROW | EFFECT_MAY_REENTER;
static NEXT_RUNTIME_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_REGISTRY_ID: AtomicU64 = AtomicU64::new(1);

/// Host-function effect indicating that the callback may allocate.
pub const EFFECT_MAY_ALLOCATE: u32 = 1 << 0;
/// Host-function effect indicating that the callback may trigger collection.
pub const EFFECT_MAY_GC: u32 = 1 << 1;
/// Host-function effect indicating that the callback may throw.
pub const EFFECT_MAY_THROW: u32 = 1 << 2;
/// Host-function effect indicating that the callback may suspend.
pub const EFFECT_MAY_SUSPEND: u32 = 1 << 3;
/// Host-function effect indicating that the callback may re-enter Ogma.
pub const EFFECT_MAY_REENTER: u32 = 1 << 4;

/// Stable host-function signature tag for dynamic argument conversion.
pub const SIGNATURE_GENERIC: u32 = 0;
/// Stable host-function signature tag for (f64, f64) -> f64.
pub const SIGNATURE_F64_BINARY: u32 = 0x1001;

/// Fixed-layout ABI-v1 version descriptor.
///
/// The record is exactly 24 bytes. Future metadata must use a separate
/// size-negotiated descriptor rather than appending fields to this structure.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OgmaAbiVersion {
    /// ABI major version.
    pub major: u16,
    /// ABI minor version.
    pub minor: u16,
    /// ABI patch version.
    pub patch: u16,
    /// Reserved; must be zero.
    pub reserved: u16,
    /// Exact structure size, not caller buffer capacity.
    pub struct_size: u32,
    /// Reserved; must be zero.
    pub reserved2: u32,
    /// Supported feature bitset.
    pub features: u64,
}

impl OgmaAbiVersion {
    /// Returns the version and feature set implemented by this build.
    #[must_use]
    pub const fn current() -> Self {
        Self {
            major: ABI_VERSION,
            minor: ABI_MINOR,
            patch: ABI_PATCH,
            reserved: 0,
            struct_size: std::mem::size_of::<Self>() as u32,
            reserved2: 0,
            features: ABI_FEATURES,
        }
    }
}

/// Stable status values for ABI transport and descriptor validation.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OgmaStatus(pub u32);

impl OgmaStatus {
    /// The operation succeeded.
    pub const OK: Self = Self(0);
    /// An argument or output pointer is invalid.
    pub const INVALID_ARGUMENT: Self = Self(1);
    /// The requested ABI version is incompatible.
    pub const INCOMPATIBLE_ABI: Self = Self(2);
    /// A required feature is not supported.
    pub const UNSUPPORTED_FEATURE: Self = Self(3);
    /// The operation was attempted from a non-owner thread.
    pub const WRONG_THREAD: Self = Self(4);
    /// A handle or function slot is stale or belongs to another Runtime.
    pub const STALE_HANDLE: Self = Self(5);
    /// A value does not match the requested type or signature.
    pub const TYPE_MISMATCH: Self = Self(6);
    /// A checked conversion is out of range.
    pub const OUT_OF_RANGE: Self = Self(7);
    /// Re-entrant dispatch is not supported for the requested operation.
    pub const REENTRANCY_LIMIT: Self = Self(8);
    /// A host callback reported a transport failure.
    pub const CALLBACK_ERROR: Self = Self(9);
    /// A callback returned an invalid completion.
    pub const INVALID_COMPLETION: Self = Self(10);
    /// The resource is busy during active execution or dispatch.
    pub const BUSY: Self = Self(11);
    /// A Rust panic was caught at the ABI boundary.
    pub const PANIC: Self = Self(12);
    /// An unexpected internal error occurred.
    pub const INTERNAL_ERROR: Self = Self(13);
}

/// Result of ABI-v1 negotiation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbiNegotiationError {
    /// The required major differs or the requested minor is newer.
    IncompatibleVersion,
    /// The consumer requires feature bits not implemented by this Runtime.
    UnsupportedFeatures { missing: u64 },
}

/// Negotiates ABI major/minor and required feature bits.
pub fn negotiate_abi(
    required_major: u16,
    minimum_minor: u16,
    required_features: u64,
) -> Result<OgmaAbiVersion, AbiNegotiationError> {
    let version = OgmaAbiVersion::current();
    if required_major != version.major || minimum_minor > version.minor {
        return Err(AbiNegotiationError::IncompatibleVersion);
    }
    let missing = required_features & !version.features;
    if missing != 0 {
        return Err(AbiNegotiationError::UnsupportedFeatures { missing });
    }
    Ok(version)
}

/// Returns the current ABI descriptor through the C-compatible boundary.
#[unsafe(no_mangle)]
pub extern "C" fn ogma_abi_version() -> OgmaAbiVersion {
    OgmaAbiVersion::current()
}

/// Negotiates the production ABI through a C-compatible boundary.
///
/// The output record must be writable and have the exact ABI-v1 layout.
///
/// # Safety
///
/// A non-null out_version must point to writable, aligned storage for one
/// complete OgmaAbiVersion.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ogma_abi_negotiate(
    required_major: u16,
    minimum_minor: u16,
    required_features: u64,
    out_version: *mut OgmaAbiVersion,
) -> OgmaStatus {
    if out_version.is_null() {
        return OgmaStatus::INVALID_ARGUMENT;
    }
    match catch_unwind(AssertUnwindSafe(|| {
        negotiate_abi(required_major, minimum_minor, required_features)
    })) {
        Ok(Ok(version)) => {
            // SAFETY: guaranteed by the caller contract above.
            unsafe { out_version.write(version) };
            OgmaStatus::OK
        }
        Ok(Err(AbiNegotiationError::IncompatibleVersion)) => OgmaStatus::INCOMPATIBLE_ABI,
        Ok(Err(AbiNegotiationError::UnsupportedFeatures { .. })) => OgmaStatus::UNSUPPORTED_FEATURE,
        Err(_) => OgmaStatus::PANIC,
    }
}

/// Performs binary64 addition through a C-compatible primitive ABI.
///
/// # Safety
///
/// out_result must point to writable storage for one f64.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ogma_primitive_add_f64(
    left: f64,
    right: f64,
    out_result: *mut f64,
) -> OgmaStatus {
    if out_result.is_null() {
        return OgmaStatus::INVALID_ARGUMENT;
    }
    match catch_unwind(AssertUnwindSafe(|| left + right)) {
        Ok(value) => {
            // SAFETY: guaranteed by the caller contract above.
            unsafe { out_result.write(value) };
            OgmaStatus::OK
        }
        Err(_) => OgmaStatus::PANIC,
    }
}

/// Validated resource configuration shared by every Runtime created by an Engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineConfig {
    /// Aggregate managed heap quota in bytes.
    pub heap_quota_bytes: usize,
    /// Maximum size of one managed allocation in bytes.
    pub max_allocation_bytes: usize,
    /// Maximum dispatched interpreter instructions per top-level execution.
    pub instruction_budget: u64,
    /// Maximum nested interpreter/host execution depth.
    pub max_call_depth: u32,
    /// Maximum ordinary instruction dispatches between cooperative polls.
    pub interrupt_poll_interval: u32,
    /// Capacity reserved for the host-driven pending-job scheduler.
    ///
    /// M1 synchronous execution does not enqueue jobs. OGM-48 supplies the
    /// queue/pump and consumes this already-validated resource policy.
    pub max_pending_jobs: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            heap_quota_bytes: 64 * 1024 * 1024,
            max_allocation_bytes: 16 * 1024 * 1024,
            instruction_budget: 1_000_000,
            max_call_depth: 256,
            interrupt_poll_interval: 64,
            max_pending_jobs: 1024,
        }
    }
}

impl EngineConfig {
    /// Validates all resource limits before any Runtime is allocated.
    pub fn validate(&self) -> Result<(), ConfigurationError> {
        if self.heap_quota_bytes == 0 {
            return Err(ConfigurationError::ZeroHeapQuota);
        }
        if self.max_allocation_bytes == 0 || self.max_allocation_bytes > self.heap_quota_bytes {
            return Err(ConfigurationError::InvalidAllocationLimit);
        }
        if self.instruction_budget == 0 {
            return Err(ConfigurationError::ZeroInstructionBudget);
        }
        if self.max_call_depth == 0 {
            return Err(ConfigurationError::ZeroCallDepth);
        }
        if self.interrupt_poll_interval == 0 {
            return Err(ConfigurationError::ZeroPollInterval);
        }
        if self.max_pending_jobs > 1_048_576 {
            return Err(ConfigurationError::PendingJobLimitTooLarge);
        }
        Ok(())
    }
}

/// Invalid Engine or per-execution resource configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigurationError {
    /// Heap quota must be positive.
    ZeroHeapQuota,
    /// Maximum allocation must be positive and no greater than the heap quota.
    InvalidAllocationLimit,
    /// Instruction budget must be positive.
    ZeroInstructionBudget,
    /// Call-depth limit must be positive.
    ZeroCallDepth,
    /// Cooperative poll interval must be positive.
    ZeroPollInterval,
    /// Pending-job capacity exceeds the supported configuration ceiling.
    PendingJobLimitTooLarge,
    /// Per-call instruction budget must be positive when supplied.
    InvalidExecutionFuel,
    /// Per-call call-depth limit must be positive when supplied.
    InvalidExecutionCallDepth,
}

/// Immutable Engine configuration and Runtime factory.
#[derive(Clone, Debug)]
pub struct Engine {
    config: EngineConfig,
}

impl Engine {
    /// Creates an Engine after validating its complete resource policy.
    pub fn new(config: EngineConfig) -> Result<Self, ConfigurationError> {
        config.validate()?;
        Ok(Self { config })
    }

    /// Returns the validated configuration.
    #[must_use]
    pub const fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// Creates an independent owner-thread Runtime with its own heap and GC.
    pub fn create_runtime(&self) -> Result<Runtime, RuntimeError> {
        let id = NEXT_RUNTIME_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| RuntimeError::IdentitySpaceExhausted)?;
        let heap = Heap::new(HeapConfig::new(
            self.config.heap_quota_bytes,
            self.config.max_allocation_bytes,
        ))
        .map_err(RuntimeError::Heap)?;
        Ok(Runtime {
            id,
            owner_thread: thread::current().id(),
            config: self.config.clone(),
            heap,
            _thread_affinity: PhantomData,
        })
    }
}

/// Owner-thread Runtime and managed heap.
///
/// Runtime is neither Send nor Sync. Independent instances may live on
/// separate host threads; one instance is never concurrently accessed.
pub struct Runtime {
    id: u64,
    owner_thread: ThreadId,
    config: EngineConfig,
    heap: Heap,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl Runtime {
    /// Returns the unique identity of this Runtime for diagnostics.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// Returns the immutable resource configuration.
    #[must_use]
    pub const fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// Returns current managed heap and root accounting.
    #[must_use]
    pub fn heap_stats(&self) -> HeapStats {
        self.heap.stats()
    }

    /// Creates a scoped execution Context borrowing this Runtime.
    ///
    /// The exclusive borrow prevents Runtime destruction or another Context
    /// from being created while this Context is alive.
    pub fn create_context(&mut self) -> Result<Context<'_>, RuntimeError> {
        self.ensure_owner_thread()?;
        Ok(Context {
            runtime: self,
            module: None,
            host_functions: HostFunctionRegistry::new().map_err(RuntimeError::HostRegistry)?,
        })
    }

    fn ensure_owner_thread(&self) -> Result<(), RuntimeError> {
        if thread::current().id() == self.owner_thread {
            Ok(())
        } else {
            Err(RuntimeError::WrongThread)
        }
    }
}

/// Runtime lifecycle failure.
#[derive(Debug)]
pub enum RuntimeError {
    /// The Runtime was accessed from a non-owner thread.
    WrongThread,
    /// Managed heap initialization failed.
    Heap(HeapError),
    /// Unique Runtime identity space was exhausted.
    IdentitySpaceExhausted,
    /// Host registry initialization failed.
    HostRegistry(HostRegistrationError),
}

/// Per-execution overrides. Unspecified limits inherit Engine configuration.
#[derive(Clone, Debug, Default)]
pub struct ExecutionRequest {
    /// Optional per-call instruction budget override.
    pub instruction_budget: Option<u64>,
    /// Optional per-call nested-call depth override.
    pub max_call_depth: Option<u32>,
    /// Optional thread-safe cancellation token.
    pub cancellation: Option<CancellationToken>,
    /// Optional absolute monotonic deadline.
    pub deadline: Option<Instant>,
    /// Optional relative execution time budget.
    pub time_budget: Option<Duration>,
    /// Host-supplied global object for the invocation.
    pub global_this: Option<Value>,
}

impl ExecutionRequest {
    fn options(&self, config: &EngineConfig) -> Result<ExecutionOptions, ConfigurationError> {
        if self.instruction_budget == Some(0) {
            return Err(ConfigurationError::InvalidExecutionFuel);
        }
        if self.max_call_depth == Some(0) {
            return Err(ConfigurationError::InvalidExecutionCallDepth);
        }
        Ok(ExecutionOptions {
            instruction_budget: self.instruction_budget.unwrap_or(config.instruction_budget),
            max_call_depth: self.max_call_depth.unwrap_or(config.max_call_depth),
            interrupt_poll_interval: config.interrupt_poll_interval,
            cancellation: self.cancellation.clone(),
            deadline: self.deadline,
            time_budget: self.time_budget,
            global_this: self.global_this.unwrap_or_else(Value::undefined),
        })
    }
}

/// A loaded execution Context borrowing one Runtime.
///
/// Module metadata, host-function registration and capabilities are scoped to
/// this Context. Managed values remain owned by the parent Runtime.
pub struct Context<'runtime> {
    runtime: &'runtime mut Runtime,
    module: Option<ExecutableModule>,
    host_functions: HostFunctionRegistry,
}

impl Context<'_> {
    /// Loads and verifies serialized OGMB artifacts into this Context.
    pub fn load_module<I, B>(&mut self, artifacts: I) -> Result<(), InterpreterError>
    where
        I: IntoIterator<Item = B>,
        B: AsRef<[u8]>,
    {
        self.module = Some(ExecutableModule::load(artifacts)?);
        Ok(())
    }

    /// Returns the number of verified functions in the loaded module.
    #[must_use]
    pub fn function_count(&self) -> usize {
        self.module
            .as_ref()
            .map_or(0, ExecutableModule::function_count)
    }

    /// Sets the capability bitset granted to host functions in this Context.
    pub fn set_capabilities(&self, capabilities: u64) {
        self.host_functions.set_capabilities(capabilities);
    }

    /// Returns a cloneable owner-thread host registry for nested callbacks.
    #[must_use]
    pub fn host_function_registry(&self) -> HostFunctionRegistry {
        self.host_functions.clone()
    }

    /// Registers a generic Rust host function.
    pub fn register_host_function<F>(
        &self,
        descriptor: HostFunctionDescriptor,
        callback: F,
    ) -> Result<HostFunctionSlot, HostRegistrationError>
    where
        F: for<'a> Fn(&mut HostCallContext<'a>, &[Value]) -> Result<HostCallCompletion, HostError>
            + 'static,
    {
        self.host_functions.register(descriptor, callback)
    }

    /// Registers a fixed-signature C-compatible host function.
    ///
    /// The generic callback is retained as the fallback for signature/effect
    /// paths that cannot use the primitive thunk.
    ///
    /// # Safety
    ///
    /// The descriptor and its name pointer must be valid for the duration of
    /// this call. user_data must remain valid until the returned slot is
    /// successfully unregistered. Callback pointers must obey their declared
    /// C-unwind ABI and must not unwind from non-Rust code.
    pub unsafe fn register_c_host_function(
        &self,
        descriptor: &OgmaHostFunctionDescriptor,
    ) -> Result<HostFunctionSlot, HostRegistrationError> {
        // SAFETY: the caller upholds the descriptor pointer and user_data
        // lifetime requirements documented on this method.
        unsafe { self.host_functions.register_c(descriptor) }
    }

    /// Unregisters a host function and invalidates its generation-checked slot.
    pub fn unregister_host_function(
        &self,
        slot: HostFunctionSlot,
    ) -> Result<(), HostRegistrationError> {
        self.host_functions.unregister(slot)
    }

    /// Executes an exported function from the loaded, verified module.
    pub fn execute(
        &mut self,
        entry_function: &str,
        arguments: &[Value],
        request: ExecutionRequest,
    ) -> Result<ExecutionReport, ContextError> {
        let options = request
            .options(&self.runtime.config)
            .map_err(ContextError::Configuration)?;
        let module = self.module.as_ref().ok_or(ContextError::ModuleNotLoaded)?;
        let mut host = self.host_functions.clone();
        module
            .execute(
                &mut self.runtime.heap,
                entry_function,
                arguments,
                &mut host,
                options,
            )
            .map_err(ContextError::Interpreter)
    }

    /// Executes a function and immediately promotes a Return/Throw value.
    ///
    /// This API avoids exposing a transient GC-managed result that could be
    /// collected by the next allocation.
    pub fn execute_persistent(
        &mut self,
        entry_function: &str,
        arguments: &[Value],
        request: ExecutionRequest,
    ) -> Result<PersistentExecutionReport, ContextError> {
        let report = self.execute(entry_function, arguments, request)?;
        let completion = match report.completion {
            ExecutionCompletion::Return(value) => {
                PersistentCompletion::Return(self.persist(value).map_err(ContextError::Heap)?)
            }
            ExecutionCompletion::Throw(value) => {
                PersistentCompletion::Throw(self.persist(value).map_err(ContextError::Heap)?)
            }
            ExecutionCompletion::Interrupted(reason) => PersistentCompletion::Interrupted(reason),
        };
        Ok(PersistentExecutionReport {
            completion,
            instructions_executed: report.instructions_executed,
            fuel_remaining: report.fuel_remaining,
            max_call_depth_reached: report.max_call_depth_reached,
            last_location: report.last_location,
        })
    }

    /// Allocates an ECMAScript string from UTF-8 host text.
    pub fn create_string(&mut self, text: &str) -> Result<Value, HeapError> {
        self.runtime.heap.allocate_string(text)
    }

    /// Allocates a plain ECMAScript object in this Runtime.
    pub fn create_object(&mut self) -> Result<Value, HeapError> {
        self.runtime.heap.allocate_object()
    }

    /// Reads a String as UTF-16 code units without losing unpaired surrogates.
    pub fn read_string_utf16(&self, value: Value) -> Result<Vec<u16>, HeapError> {
        self.runtime.heap.string_utf16(value).map(ToOwned::to_owned)
    }

    /// Promotes a value to an opaque, generation-checked persistent host root.
    pub fn persist(&mut self, value: Value) -> Result<PersistentHandle, HeapError> {
        self.runtime
            .heap
            .retain_host_value(value)
            .map(PersistentHandle)
    }

    /// Resolves a persistent handle to a transient Runtime value.
    pub fn resolve(&self, handle: PersistentHandle) -> Result<Value, HeapError> {
        self.runtime.heap.resolve_host_handle(handle.0)
    }

    /// Releases a persistent root and invalidates its handle.
    pub fn release(&mut self, handle: PersistentHandle) -> Result<(), HeapError> {
        self.runtime.heap.release_host_handle(handle.0)
    }

    /// Opens a strict-LIFO temporary root scope.
    pub fn open_root_scope(&mut self) -> Result<RootScope<'_>, HeapError> {
        let token = self.runtime.heap.open_scope()?;
        Ok(RootScope {
            heap: &mut self.runtime.heap,
            token: Some(token),
        })
    }

    /// Performs a full tracing collection and returns its accounting report.
    pub fn collect(&mut self) -> Result<CollectionReport, HeapError> {
        self.runtime.heap.collect()
    }

    /// Returns heap and root statistics for this Context's Runtime.
    #[must_use]
    pub fn heap_stats(&self) -> HeapStats {
        self.runtime.heap.stats()
    }

    /// Returns the configured pending-job capacity.
    ///
    /// The synchronous M1 interpreter has no job queue; OGM-48 supplies the
    /// queue/pump and consumes this already-validated limit.
    #[must_use]
    pub const fn pending_job_limit(&self) -> usize {
        self.runtime.config.max_pending_jobs
    }
}

/// Opaque persistent host root. It contains no Value bits or heap pointer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PersistentHandle(HostHandle);

impl std::fmt::Debug for PersistentHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PersistentHandle(<opaque>)")
    }
}

/// RAII temporary root scope. It closes on Drop and cannot outlive its Runtime.
pub struct RootScope<'context> {
    heap: &'context mut Heap,
    token: Option<ScopeToken>,
}

impl RootScope<'_> {
    /// Adds a transient value to the active scope's precise root set.
    pub fn root(&mut self, value: Value) -> Result<(), HeapError> {
        let token = self.token.ok_or(HeapError::InvalidScope)?;
        self.heap.scope_root(token, value)
    }

    /// Closes the scope explicitly. Drop closes it if this method is not called.
    pub fn close(mut self) -> Result<(), HeapError> {
        let token = self.token.take().ok_or(HeapError::InvalidScope)?;
        self.heap.close_scope(token)
    }
}

impl Drop for RootScope<'_> {
    fn drop(&mut self) {
        if let Some(token) = self.token.take() {
            let _ = self.heap.close_scope(token);
        }
    }
}

/// Persistent completion returned by Context execution with root promotion.
#[derive(Clone, Copy, Debug)]
pub enum PersistentCompletion {
    /// The function returned a value retained by the Runtime.
    Return(PersistentHandle),
    /// The function threw a value retained by the Runtime.
    Throw(PersistentHandle),
    /// The execution was cooperatively interrupted.
    Interrupted(ExecutionInterruption),
}

/// Execution accounting with managed Return/Throw values.
#[derive(Clone, Debug)]
pub struct PersistentExecutionReport {
    /// Managed completion with explicitly persistent values.
    pub completion: PersistentCompletion,
    /// Number of dispatched instructions, including nested host executions.
    pub instructions_executed: u64,
    /// Remaining shared instruction fuel.
    pub fuel_remaining: u64,
    /// Maximum nested activation depth reached.
    pub max_call_depth_reached: u32,
    /// Source location of the last dispatched instruction.
    pub last_location: Option<InstructionLocation>,
}

/// Failure while executing a Context.
#[derive(Debug)]
pub enum ContextError {
    /// No verified module has been loaded.
    ModuleNotLoaded,
    /// Per-execution configuration is invalid.
    Configuration(ConfigurationError),
    /// Interpreter rejected loading or execution.
    Interpreter(InterpreterError),
    /// Heap operation failed while promoting a completion.
    Heap(HeapError),
}

/// Rust host-function descriptor. IDs are stable within ABI major 1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostFunctionDescriptor {
    /// Non-zero stable ID chosen by the embedding application.
    pub function_id: u64,
    /// UTF-8 capability/export name used by verified bytecode.
    pub name: String,
    /// Capability bits required at each invocation.
    pub required_capabilities: u64,
    /// Declared host-function effects.
    pub effects: u32,
}

impl HostFunctionDescriptor {
    /// Creates a generic host-function descriptor with no required capabilities.
    #[must_use]
    pub fn new(function_id: u64, name: impl Into<String>) -> Self {
        Self {
            function_id,
            name: name.into(),
            required_capabilities: 0,
            effects: 0,
        }
    }

    /// Sets the capability mask checked on every invocation.
    #[must_use]
    pub const fn requiring_capabilities(mut self, capabilities: u64) -> Self {
        self.required_capabilities = capabilities;
        self
    }

    /// Sets the declared callback effects.
    #[must_use]
    pub const fn with_effects(mut self, effects: u32) -> Self {
        self.effects = effects;
        self
    }
}

/// Opaque generation-checked host-function table slot.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct HostFunctionSlot {
    registry_id: u64,
    slot_index: u32,
    slot_generation: u32,
    abi_major: u16,
    abi_minor: u16,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl std::fmt::Debug for HostFunctionSlot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HostFunctionSlot(<opaque>)")
    }
}

/// Registration or lifecycle failure for a host function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostRegistrationError {
    /// The call originated from a non-owner thread.
    WrongThread,
    /// Descriptor fields, effects, signature or name are invalid.
    InvalidDescriptor,
    /// Function ID is already registered in this table.
    DuplicateFunctionId,
    /// Function name is already registered in this table.
    DuplicateName,
    /// Slot is stale or belongs to another registry.
    StaleSlot,
    /// A callback is active; unregister may be retried after it returns.
    Busy,
    /// A slot generation cannot be advanced safely.
    GenerationExhausted,
    /// Registry identity space was exhausted.
    RegistryIdentityExhausted,
}

/// C-compatible M1 host value representation for primitive values.
///
/// Managed strings, objects and functions remain opaque Runtime values and are
/// not smuggled through this POD representation.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OgmaHostValue {
    /// One of the OGMA_VALUE_* constants.
    pub kind: u32,
    /// Reserved; must be zero.
    pub reserved: u32,
    /// Boolean bit or binary64 bits, depending on kind.
    pub payload: u64,
}

/// Host value kind for JavaScript undefined.
pub const OGMA_VALUE_UNDEFINED: u32 = 0;
/// Host value kind for JavaScript null.
pub const OGMA_VALUE_NULL: u32 = 1;
/// Host value kind for JavaScript Boolean.
pub const OGMA_VALUE_BOOLEAN: u32 = 2;
/// Host value kind for JavaScript Number.
pub const OGMA_VALUE_NUMBER: u32 = 3;

/// Normal completion tag for OgmaCompletion.
pub const COMPLETION_NORMAL: u32 = 0;
/// Throw completion tag for OgmaCompletion.
pub const COMPLETION_THROW: u32 = 1;

/// C-compatible host completion. Transport status remains separate.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OgmaCompletion {
    /// COMPLETION_NORMAL or COMPLETION_THROW.
    pub kind: u32,
    /// Reserved; must be zero.
    pub reserved: u32,
    /// Primitive value payload.
    pub value: OgmaHostValue,
}

/// Generic C-compatible synchronous host callback.
pub type OgmaGenericHostCallback = unsafe extern "C-unwind" fn(
    arguments: *const OgmaHostValue,
    arguments_len: usize,
    out_completion: *mut OgmaCompletion,
    user_data: *mut c_void,
) -> OgmaStatus;

/// Fixed-signature C-compatible primitive thunk.
///
/// It is eligible only for a descriptor without allocation, GC, throw,
/// suspension or re-entry effects. Generic fallback remains mandatory.
pub type OgmaPrimitiveF64Thunk = unsafe extern "C-unwind" fn(
    left: f64,
    right: f64,
    out_result: *mut f64,
    user_data: *mut c_void,
) -> OgmaStatus;

/// C-compatible host-function registration descriptor v1.
///
/// The name is copied during registration. user_data remains host-owned and
/// must remain valid until the registration is successfully unregistered.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OgmaHostFunctionDescriptor {
    /// Must equal size_of::<OgmaHostFunctionDescriptor>().
    pub struct_size: u32,
    /// Descriptor schema version; currently 1.
    pub descriptor_version: u16,
    /// Required ABI major.
    pub abi_major: u16,
    /// Minimum ABI minor required by the descriptor.
    pub abi_minor: u16,
    /// Reserved; must be zero.
    pub reserved: u16,
    /// Stable non-zero host function identity.
    pub function_id: u64,
    /// Borrowed UTF-8 function name.
    pub name: *const u8,
    /// Name byte length.
    pub name_len: usize,
    /// SIGNATURE_GENERIC or SIGNATURE_F64_BINARY.
    pub signature: u32,
    /// Host effect bitset.
    pub effects: u32,
    /// Capability bitset checked on every invocation.
    pub required_capabilities: u64,
    /// Required generic callback fallback.
    pub generic_callback: Option<OgmaGenericHostCallback>,
    /// Optional fixed-signature primitive entry point.
    pub primitive_f64_thunk: Option<OgmaPrimitiveF64Thunk>,
    /// Host-owned callback data.
    pub user_data: *mut c_void,
}

#[derive(Clone)]
enum HostHandler {
    Rust(
        Rc<
            dyn for<'a> Fn(
                    &mut HostCallContext<'a>,
                    &[Value],
                ) -> Result<HostCallCompletion, HostError>
                + 'static,
        >,
    ),
    C(CHostHandler),
}

#[derive(Clone, Copy)]
struct CHostHandler {
    signature: u32,
    effects: u32,
    generic_callback: OgmaGenericHostCallback,
    primitive_f64_thunk: Option<OgmaPrimitiveF64Thunk>,
    user_data: *mut c_void,
}

#[derive(Clone)]
struct RegisteredHostFunction {
    descriptor: HostFunctionDescriptor,
    handler: HostHandler,
}

struct HostSlotCell {
    generation: u32,
    registered: Option<RegisteredHostFunction>,
}

#[derive(Default)]
struct RegistryState {
    slots: Vec<HostSlotCell>,
    by_id: HashMap<u64, usize>,
    by_name: HashMap<String, usize>,
    table_version: u64,
}

struct RegistryInner {
    id: u64,
    owner_thread: ThreadId,
    state: RefCell<RegistryState>,
    active_dispatches: Cell<u32>,
    capabilities: Cell<u64>,
    _thread_affinity: PhantomData<Rc<()>>,
}

/// Cloneable, owner-thread typed host-function registry.
///
/// Registry lookups are snapshotted and all table borrows are released before
/// user code executes.
#[derive(Clone)]
pub struct HostFunctionRegistry {
    inner: Rc<RegistryInner>,
}

impl HostFunctionRegistry {
    /// Creates an independent empty host-function table.
    pub fn new() -> Result<Self, HostRegistrationError> {
        let id = NEXT_REGISTRY_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| HostRegistrationError::RegistryIdentityExhausted)?;
        Ok(Self {
            inner: Rc::new(RegistryInner {
                id,
                owner_thread: thread::current().id(),
                state: RefCell::new(RegistryState::default()),
                active_dispatches: Cell::new(0),
                capabilities: Cell::new(0),
                _thread_affinity: PhantomData,
            }),
        })
    }

    /// Updates the capabilities checked on every call through this registry.
    pub fn set_capabilities(&self, capabilities: u64) {
        self.inner.capabilities.set(capabilities);
    }

    /// Registers a generic Rust callback.
    ///
    /// The callback is cloned out of the registry before invocation. It may
    /// safely execute a nested module using HostCallContext::execute_nested.
    pub fn register<F>(
        &self,
        descriptor: HostFunctionDescriptor,
        callback: F,
    ) -> Result<HostFunctionSlot, HostRegistrationError>
    where
        F: for<'a> Fn(&mut HostCallContext<'a>, &[Value]) -> Result<HostCallCompletion, HostError>
            + 'static,
    {
        self.validate_rust_descriptor(&descriptor)?;
        self.insert(descriptor, HostHandler::Rust(Rc::new(callback)))
    }

    fn validate_rust_descriptor(
        &self,
        descriptor: &HostFunctionDescriptor,
    ) -> Result<(), HostRegistrationError> {
        self.ensure_owner_thread()?;
        if descriptor.function_id == 0
            || descriptor.name.is_empty()
            || descriptor.name.len() > MAX_HOST_NAME_BYTES
            || descriptor.name.as_bytes().contains(&0)
            || descriptor.effects & !KNOWN_EFFECTS != 0
            || descriptor.effects & EFFECT_MAY_SUSPEND != 0
        {
            return Err(HostRegistrationError::InvalidDescriptor);
        }
        Ok(())
    }

    /// Registers a C-compatible generic callback and optional primitive thunk.
    ///
    /// # Safety
    ///
    /// descriptor.name must be readable for name_len bytes for this call.
    /// user_data and callback pointers must remain valid until successful
    /// unregistration. Callbacks must obey their declared ABI.
    pub unsafe fn register_c(
        &self,
        descriptor: &OgmaHostFunctionDescriptor,
    ) -> Result<HostFunctionSlot, HostRegistrationError> {
        self.ensure_owner_thread()?;
        if descriptor.struct_size != std::mem::size_of::<OgmaHostFunctionDescriptor>() as u32
            || descriptor.descriptor_version != DESCRIPTOR_VERSION
            || descriptor.abi_major != ABI_VERSION
            || descriptor.abi_minor > ABI_MINOR
            || descriptor.reserved != 0
            || descriptor.function_id == 0
            || descriptor.name.is_null()
            || descriptor.name_len == 0
            || descriptor.name_len > MAX_HOST_NAME_BYTES
            || descriptor.effects & !KNOWN_EFFECTS != 0
            || descriptor.effects & EFFECT_MAY_SUSPEND != 0
            || descriptor.generic_callback.is_none()
        {
            return Err(HostRegistrationError::InvalidDescriptor);
        }
        // SAFETY: the caller contract guarantees a readable name buffer.
        let name_bytes =
            unsafe { std::slice::from_raw_parts(descriptor.name, descriptor.name_len) };
        let name = std::str::from_utf8(name_bytes)
            .map_err(|_| HostRegistrationError::InvalidDescriptor)?;
        if name.as_bytes().contains(&0) {
            return Err(HostRegistrationError::InvalidDescriptor);
        }
        let signature_valid = match descriptor.signature {
            SIGNATURE_GENERIC => descriptor.primitive_f64_thunk.is_none(),
            SIGNATURE_F64_BINARY => {
                descriptor.primitive_f64_thunk.is_some()
                    && descriptor.effects & COMPLEX_EFFECTS == 0
            }
            _ => false,
        };
        if !signature_valid {
            return Err(HostRegistrationError::InvalidDescriptor);
        }
        let rust_descriptor = HostFunctionDescriptor {
            function_id: descriptor.function_id,
            name: name.to_owned(),
            required_capabilities: descriptor.required_capabilities,
            effects: descriptor.effects,
        };
        let handler = HostHandler::C(CHostHandler {
            signature: descriptor.signature,
            effects: descriptor.effects,
            generic_callback: descriptor
                .generic_callback
                .ok_or(HostRegistrationError::InvalidDescriptor)?,
            primitive_f64_thunk: descriptor.primitive_f64_thunk,
            user_data: descriptor.user_data,
        });
        self.insert(rust_descriptor, handler)
    }

    fn insert(
        &self,
        descriptor: HostFunctionDescriptor,
        handler: HostHandler,
    ) -> Result<HostFunctionSlot, HostRegistrationError> {
        self.ensure_owner_thread()?;
        let mut state = self.inner.state.borrow_mut();
        if state.by_id.contains_key(&descriptor.function_id) {
            return Err(HostRegistrationError::DuplicateFunctionId);
        }
        if state.by_name.contains_key(&descriptor.name) {
            return Err(HostRegistrationError::DuplicateName);
        }
        let index = match state
            .slots
            .iter()
            .position(|slot| slot.registered.is_none() && slot.generation != u32::MAX)
        {
            Some(index) => index,
            None => {
                state.slots.push(HostSlotCell {
                    generation: 1,
                    registered: None,
                });
                state.slots.len() - 1
            }
        };
        let generation = state.slots[index].generation;
        state.slots[index].registered = Some(RegisteredHostFunction {
            descriptor: descriptor.clone(),
            handler,
        });
        state.by_id.insert(descriptor.function_id, index);
        state.by_name.insert(descriptor.name, index);
        state.table_version = state.table_version.saturating_add(1);
        Ok(HostFunctionSlot {
            registry_id: self.inner.id,
            slot_index: index as u32,
            slot_generation: generation,
            abi_major: ABI_VERSION,
            abi_minor: ABI_MINOR,
            _thread_affinity: PhantomData,
        })
    }

    /// Resolves a function name to its opaque table slot.
    pub fn resolve_name(&self, name: &str) -> Result<HostFunctionSlot, HostRegistrationError> {
        self.ensure_owner_thread()?;
        let state = self.inner.state.borrow();
        let index = *state
            .by_name
            .get(name)
            .ok_or(HostRegistrationError::StaleSlot)?;
        self.slot_for_index(&state, index)
    }

    /// Resolves a stable host function ID to its opaque table slot.
    pub fn resolve_id(&self, function_id: u64) -> Result<HostFunctionSlot, HostRegistrationError> {
        self.ensure_owner_thread()?;
        let state = self.inner.state.borrow();
        let index = *state
            .by_id
            .get(&function_id)
            .ok_or(HostRegistrationError::StaleSlot)?;
        self.slot_for_index(&state, index)
    }

    /// Removes a registration and invalidates the slot generation.
    ///
    /// Unregistration returns Busy while any callback from this registry is
    /// active, so host-owned user data cannot be freed during a callback.
    pub fn unregister(&self, slot: HostFunctionSlot) -> Result<(), HostRegistrationError> {
        self.ensure_owner_thread()?;
        if slot.registry_id != self.inner.id
            || slot.abi_major != ABI_VERSION
            || slot.abi_minor > ABI_MINOR
        {
            return Err(HostRegistrationError::StaleSlot);
        }
        if self.inner.active_dispatches.get() != 0 {
            return Err(HostRegistrationError::Busy);
        }
        let mut state = self.inner.state.borrow_mut();
        let cell = state
            .slots
            .get_mut(slot.slot_index as usize)
            .ok_or(HostRegistrationError::StaleSlot)?;
        if cell.generation != slot.slot_generation || cell.registered.is_none() {
            return Err(HostRegistrationError::StaleSlot);
        }
        let next_generation = cell
            .generation
            .checked_add(1)
            .ok_or(HostRegistrationError::GenerationExhausted)?;
        let entry = cell
            .registered
            .take()
            .ok_or(HostRegistrationError::StaleSlot)?;
        cell.generation = next_generation;
        state.by_id.remove(&entry.descriptor.function_id);
        state.by_name.remove(&entry.descriptor.name);
        state.table_version = state.table_version.saturating_add(1);
        Ok(())
    }

    /// Returns the structural version incremented by registration changes.
    #[must_use]
    pub fn table_version(&self) -> u64 {
        self.inner.state.borrow().table_version
    }

    fn ensure_owner_thread(&self) -> Result<(), HostRegistrationError> {
        if thread::current().id() == self.inner.owner_thread {
            Ok(())
        } else {
            Err(HostRegistrationError::WrongThread)
        }
    }

    fn slot_for_index(
        &self,
        state: &RegistryState,
        index: usize,
    ) -> Result<HostFunctionSlot, HostRegistrationError> {
        let cell = state
            .slots
            .get(index)
            .ok_or(HostRegistrationError::StaleSlot)?;
        if cell.registered.is_none() {
            return Err(HostRegistrationError::StaleSlot);
        }
        Ok(HostFunctionSlot {
            registry_id: self.inner.id,
            slot_index: index as u32,
            slot_generation: cell.generation,
            abi_major: ABI_VERSION,
            abi_minor: ABI_MINOR,
            _thread_affinity: PhantomData,
        })
    }

    fn snapshot(&self, name: &str) -> Result<RegisteredHostFunction, HostRegistrationError> {
        self.ensure_owner_thread()?;
        let state = self.inner.state.borrow();
        let index = *state
            .by_name
            .get(name)
            .ok_or(HostRegistrationError::StaleSlot)?;
        state.slots[index]
            .registered
            .clone()
            .ok_or(HostRegistrationError::StaleSlot)
    }

    fn enter_dispatch(&self) -> Result<DispatchGuard, HostRegistrationError> {
        self.ensure_owner_thread()?;
        let active = self.inner.active_dispatches.get();
        let next = active.checked_add(1).ok_or(HostRegistrationError::Busy)?;
        self.inner.active_dispatches.set(next);
        Ok(DispatchGuard {
            inner: self.inner.clone(),
        })
    }
}

struct DispatchGuard {
    inner: Rc<RegistryInner>,
}

impl Drop for DispatchGuard {
    fn drop(&mut self) {
        self.inner
            .active_dispatches
            .set(self.inner.active_dispatches.get().saturating_sub(1));
    }
}

impl Host for HostFunctionRegistry {
    fn call(
        &mut self,
        _heap: &mut Heap,
        _capability: &str,
        _arguments: &[Value],
    ) -> Result<Value, HostError> {
        Err(HostError::Denied)
    }

    fn call_with_context(
        &mut self,
        context: &mut HostCallContext<'_>,
        capability: &str,
        arguments: &[Value],
    ) -> Result<HostCallCompletion, HostError> {
        if arguments.len() > MAX_HOST_ARGUMENTS {
            return throw_host_error(context, "RangeError: too many host arguments");
        }
        let _active = self.enter_dispatch().map_err(|_| HostError::Denied)?;
        let entry = self.snapshot(capability).map_err(|_| HostError::Denied)?;
        let missing = entry.descriptor.required_capabilities & !self.inner.capabilities.get();
        if missing != 0 {
            return throw_host_error(context, "Error: host capability denied");
        }

        match entry.handler {
            HostHandler::Rust(callback) => {
                match catch_unwind(AssertUnwindSafe(|| callback(context, arguments))) {
                    Ok(result) => result,
                    Err(_) => Err(HostError::Panicked),
                }
            }
            HostHandler::C(handler) => invoke_c_handler(context, handler, arguments),
        }
    }
}

fn throw_host_error(
    context: &mut HostCallContext<'_>,
    message: &str,
) -> Result<HostCallCompletion, HostError> {
    let value = context
        .heap()
        .allocate_string(message)
        .map_err(HostError::Runtime)?;
    Ok(HostCallCompletion::Throw(value))
}

fn invoke_c_handler(
    context: &mut HostCallContext<'_>,
    handler: CHostHandler,
    arguments: &[Value],
) -> Result<HostCallCompletion, HostError> {
    if handler.signature == SIGNATURE_F64_BINARY
        && handler.effects & COMPLEX_EFFECTS == 0
        && let (Some(thunk), [left, right]) = (handler.primitive_f64_thunk, arguments)
        && let (Some(left), Some(right)) = (left.as_number(), right.as_number())
    {
        let mut result = 0.0;
        let status = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: thunk validity and user_data lifetime are guaranteed by
            // the unsafe registration contract.
            unsafe { thunk(left, right, &mut result, handler.user_data) }
        }))
        .map_err(|_| HostError::Panicked)?;
        if status == OgmaStatus::OK {
            return Ok(HostCallCompletion::Normal(Value::number(result)));
        }
        if status == OgmaStatus::TYPE_MISMATCH {
            return throw_host_error(context, "TypeError: host thunk argument mismatch");
        }
        if status == OgmaStatus::OUT_OF_RANGE {
            return throw_host_error(context, "RangeError: host thunk conversion failed");
        }
        return Err(HostError::Panicked);
    }

    let converted = arguments
        .iter()
        .copied()
        .map(encode_host_value)
        .collect::<Result<Vec<_>, _>>()?;
    let mut completion = OgmaCompletion::default();
    let status = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: callback validity and user_data lifetime are guaranteed by
        // the unsafe registration contract. The slices and output pointer
        // remain valid for the duration of this synchronous call.
        unsafe {
            (handler.generic_callback)(
                converted.as_ptr(),
                converted.len(),
                &mut completion,
                handler.user_data,
            )
        }
    }))
    .map_err(|_| HostError::Panicked)?;
    if status != OgmaStatus::OK {
        return Err(HostError::Panicked);
    }
    if completion.reserved != 0 || !matches!(completion.kind, COMPLETION_NORMAL | COMPLETION_THROW)
    {
        return Err(HostError::Panicked);
    }
    let value = decode_host_value(completion.value)?;
    Ok(if completion.kind == COMPLETION_THROW {
        HostCallCompletion::Throw(value)
    } else {
        HostCallCompletion::Normal(value)
    })
}

fn encode_host_value(value: Value) -> Result<OgmaHostValue, HostError> {
    let encoded = match value.value_type() {
        ValueType::Undefined => OgmaHostValue {
            kind: OGMA_VALUE_UNDEFINED,
            reserved: 0,
            payload: 0,
        },
        ValueType::Null => OgmaHostValue {
            kind: OGMA_VALUE_NULL,
            reserved: 0,
            payload: 0,
        },
        ValueType::Boolean => OgmaHostValue {
            kind: OGMA_VALUE_BOOLEAN,
            reserved: 0,
            payload: u64::from(value.as_boolean().ok_or(HostError::Denied)?),
        },
        ValueType::Number => OgmaHostValue {
            kind: OGMA_VALUE_NUMBER,
            reserved: 0,
            payload: value.as_number().ok_or(HostError::Denied)?.to_bits(),
        },
        ValueType::String | ValueType::Object | ValueType::Function => {
            return Err(HostError::Runtime(HeapError::UnsupportedCoercion));
        }
    };
    Ok(encoded)
}

fn decode_host_value(value: OgmaHostValue) -> Result<Value, HostError> {
    if value.reserved != 0 {
        return Err(HostError::Denied);
    }
    match value.kind {
        OGMA_VALUE_UNDEFINED if value.payload == 0 => Ok(Value::undefined()),
        OGMA_VALUE_NULL if value.payload == 0 => Ok(Value::null()),
        OGMA_VALUE_BOOLEAN if value.payload <= 1 => Ok(Value::boolean(value.payload == 1)),
        OGMA_VALUE_NUMBER => Ok(Value::number(f64::from_bits(value.payload))),
        _ => Err(HostError::Denied),
    }
}

/// Validated, loaded Rust execution facade for one Runtime.
#[cfg(test)]
mod tests {
    use super::*;
    use ogma_compiler::compile_to_bytecode;
    use ogma_core_ir::FunctionDocument;
    use ogma_frontend::{ParseRequestOptions, SourceFile, parse};
    use oxc_allocator::Allocator;
    use serde_json::from_str;

    fn compile_source(source_text: &str) -> Vec<Vec<u8>> {
        let allocator = Allocator::default();
        let source = SourceFile::new(
            "embedding-test",
            "memory:///embedding-test.js",
            None,
            source_text,
        )
        .expect("valid source");
        let parsed =
            parse(&allocator, &source, ParseRequestOptions::default()).expect("source parses");
        compile_to_bytecode(&parsed)
            .expect("source compiles to M1 OGMB")
            .functions
            .into_iter()
            .map(|function| function.artifact.bytes)
            .collect()
    }

    fn host_call_artifacts() -> Vec<Vec<u8>> {
        let document: FunctionDocument = from_str(include_str!(
            "../../../tests/core-ir/fixtures/positive-host-call.json"
        ))
        .expect("canonical host-call fixture");
        ogma_compiler::bytecode::compile_functions(&[document])
            .expect("Core IR fixture compiles to OGMB")
            .into_iter()
            .map(|function| function.artifact.bytes)
            .collect()
    }

    #[test]
    fn abi_v1_layout_and_negotiation_are_fixed_and_feature_checked() {
        assert_eq!(std::mem::size_of::<OgmaAbiVersion>(), 24);
        assert_eq!(OgmaAbiVersion::current().struct_size, 24);
        assert_eq!(
            negotiate_abi(1, 0, FEATURE_OPAQUE_HANDLES | FEATURE_CALLBACKS),
            Ok(OgmaAbiVersion::current())
        );
        assert_eq!(
            negotiate_abi(2, 0, 0),
            Err(AbiNegotiationError::IncompatibleVersion)
        );
        assert_eq!(
            negotiate_abi(1, 1, 0),
            Err(AbiNegotiationError::IncompatibleVersion)
        );
        assert_eq!(
            negotiate_abi(1, 0, 1 << 63),
            Err(AbiNegotiationError::UnsupportedFeatures { missing: 1 << 63 })
        );
    }

    #[test]
    fn runtime_context_executes_compiler_produced_ogmb_and_persists_results() {
        let engine = Engine::new(EngineConfig::default()).expect("valid Engine config");
        let mut runtime = engine.create_runtime().expect("independent Runtime");
        let mut context = runtime.create_context().expect("Context lifecycle");
        context
            .load_module(compile_source("function add(a, b) { return a + b; }"))
            .expect("verified module");
        let report = context
            .execute_persistent(
                "add",
                &[Value::number(19.0), Value::number(23.0)],
                ExecutionRequest::default(),
            )
            .expect("compiled JavaScript executes");
        let PersistentCompletion::Return(result) = report.completion else {
            panic!("expected persistent Return completion");
        };
        assert_eq!(
            context.resolve(result).expect("live root").as_number(),
            Some(42.0)
        );
        context.collect().expect("forced mark-sweep collection");
        assert_eq!(
            context
                .resolve(result)
                .expect("persistent root survives GC")
                .as_number(),
            Some(42.0)
        );
        context.release(result).expect("release root");
        assert!(context.resolve(result).is_err());
    }

    #[test]
    fn registered_rust_host_function_dispatches_from_the_real_interpreter() {
        let engine = Engine::new(EngineConfig::default()).expect("valid config");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        context
            .load_module(host_call_artifacts())
            .expect("verified OGMB");
        context
            .register_host_function(
                HostFunctionDescriptor::new(1, "benchmark.host.echo"),
                |_call, arguments| {
                    let value = arguments[0].as_number().ok_or(HostError::Denied)?;
                    Ok(HostCallCompletion::Normal(Value::number(value + 11.0)))
                },
            )
            .expect("register host capability");
        let report = context
            .execute(
                "fn_host_call",
                &[Value::number(31.0)],
                ExecutionRequest::default(),
            )
            .expect("interpreter calls real Rust host service");
        assert!(matches!(
            report.completion,
            ExecutionCompletion::Return(value) if value.as_number() == Some(42.0)
        ));
    }

    #[test]
    fn host_panics_are_contained_and_converted_to_interpreter_errors() {
        let engine = Engine::new(EngineConfig::default()).expect("valid config");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        context
            .load_module(host_call_artifacts())
            .expect("verified OGMB");
        context
            .register_host_function(
                HostFunctionDescriptor::new(3, "benchmark.host.echo"),
                |_call, _arguments| -> Result<HostCallCompletion, HostError> {
                    panic!("host callback panic must be contained")
                },
            )
            .expect("register host function");
        let result = context.execute(
            "fn_host_call",
            &[Value::number(1.0)],
            ExecutionRequest::default(),
        );
        assert!(matches!(
            result,
            Err(ContextError::Interpreter(
                InterpreterError::HostCallbackPanicked
            ))
        ));
    }

    #[test]
    fn capability_denial_is_a_javascript_throw_completion() {
        let engine = Engine::new(EngineConfig::default()).expect("valid config");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        context
            .load_module(host_call_artifacts())
            .expect("verified OGMB");
        context
            .register_host_function(
                HostFunctionDescriptor::new(2, "benchmark.host.echo").requiring_capabilities(0b10),
                |_call, _arguments| Ok(HostCallCompletion::Normal(Value::undefined())),
            )
            .expect("register capability");
        context.set_capabilities(0b01);
        let report = context
            .execute(
                "fn_host_call",
                &[Value::number(1.0)],
                ExecutionRequest::default(),
            )
            .expect("denial is script completion");
        let ExecutionCompletion::Throw(message) = report.completion else {
            panic!("capability denial must propagate as Throw");
        };
        assert!(
            !context
                .read_string_utf16(message)
                .expect("throw string")
                .is_empty()
        );
    }

    unsafe extern "C-unwind" fn c_generic_echo(
        arguments: *const OgmaHostValue,
        arguments_len: usize,
        out_completion: *mut OgmaCompletion,
        _user_data: *mut c_void,
    ) -> OgmaStatus {
        if arguments.is_null() || out_completion.is_null() || arguments_len != 1 {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        // SAFETY: the host dispatcher supplies a readable slice for arguments_len.
        let argument = unsafe { *arguments };
        if argument.kind != OGMA_VALUE_NUMBER || argument.reserved != 0 {
            return OgmaStatus::TYPE_MISMATCH;
        }
        let result = f64::from_bits(argument.payload) + 11.0;
        let completion = OgmaCompletion {
            kind: COMPLETION_NORMAL,
            reserved: 0,
            value: OgmaHostValue {
                kind: OGMA_VALUE_NUMBER,
                reserved: 0,
                payload: result.to_bits(),
            },
        };
        // SAFETY: the host dispatcher supplies a writable completion pointer.
        unsafe { out_completion.write(completion) };
        OgmaStatus::OK
    }

    #[test]
    fn c_compatible_generic_thunk_runs_against_the_production_interpreter() {
        let engine = Engine::new(EngineConfig::default()).expect("valid config");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        context
            .load_module(host_call_artifacts())
            .expect("verified OGMB");
        let name = b"benchmark.host.echo";
        let descriptor = OgmaHostFunctionDescriptor {
            struct_size: std::mem::size_of::<OgmaHostFunctionDescriptor>() as u32,
            descriptor_version: DESCRIPTOR_VERSION,
            abi_major: ABI_VERSION,
            abi_minor: ABI_MINOR,
            reserved: 0,
            function_id: 42,
            name: name.as_ptr(),
            name_len: name.len(),
            signature: SIGNATURE_GENERIC,
            effects: 0,
            required_capabilities: 0,
            generic_callback: Some(c_generic_echo),
            primitive_f64_thunk: None,
            user_data: std::ptr::null_mut(),
        };
        // SAFETY: name has static storage and the callback does not retain user_data.
        let slot = unsafe {
            context
                .register_c_host_function(&descriptor)
                .expect("valid C ABI descriptor")
        };
        let report = context
            .execute(
                "fn_host_call",
                &[Value::number(31.0)],
                ExecutionRequest::default(),
            )
            .expect("C-compatible callback executes");
        assert!(matches!(
            report.completion,
            ExecutionCompletion::Return(value) if value.as_number() == Some(42.0)
        ));
        context
            .unregister_host_function(slot)
            .expect("release C callback registration");
    }

    #[test]
    fn c_abi_primitive_entrypoint_checks_null_outputs_and_computes() {
        let status = unsafe { ogma_primitive_add_f64(1.5, 2.5, std::ptr::null_mut()) };
        assert_eq!(status, OgmaStatus::INVALID_ARGUMENT);
        let mut result = 0.0;
        let status = unsafe { ogma_primitive_add_f64(1.5, 2.5, &mut result) };
        assert_eq!(status, OgmaStatus::OK);
        assert_eq!(result, 4.0);
        assert_eq!(std::mem::size_of::<HostFunctionSlot>(), 24);
    }

    unsafe extern "C-unwind" fn c_generic_echo(
        arguments: *const OgmaHostValue,
        arguments_len: usize,
        out_completion: *mut OgmaCompletion,
        _user_data: *mut c_void,
    ) -> OgmaStatus {
        if arguments.is_null() || out_completion.is_null() || arguments_len != 1 {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        // SAFETY: the host dispatcher supplies a readable slice for arguments_len.
        let argument = unsafe { *arguments };
        if argument.kind != OGMA_VALUE_NUMBER || argument.reserved != 0 {
            return OgmaStatus::TYPE_MISMATCH;
        }
        let result = f64::from_bits(argument.payload) + 11.0;
        let completion = OgmaCompletion {
            kind: COMPLETION_NORMAL,
            reserved: 0,
            value: OgmaHostValue {
                kind: OGMA_VALUE_NUMBER,
                reserved: 0,
                payload: result.to_bits(),
            },
        };
        // SAFETY: the host dispatcher supplies a writable completion pointer.
        unsafe { out_completion.write(completion) };
        OgmaStatus::OK
    }

    #[test]
    fn c_compatible_generic_thunk_runs_against_the_production_interpreter() {
        let engine = Engine::new(EngineConfig::default()).expect("valid config");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        context
            .load_module(host_call_artifacts())
            .expect("verified OGMB");
        let name = b"benchmark.host.echo";
        let descriptor = OgmaHostFunctionDescriptor {
            struct_size: std::mem::size_of::<OgmaHostFunctionDescriptor>() as u32,
            descriptor_version: DESCRIPTOR_VERSION,
            abi_major: ABI_VERSION,
            abi_minor: ABI_MINOR,
            reserved: 0,
            function_id: 42,
            name: name.as_ptr(),
            name_len: name.len(),
            signature: SIGNATURE_GENERIC,
            effects: 0,
            required_capabilities: 0,
            generic_callback: Some(c_generic_echo),
            primitive_f64_thunk: None,
            user_data: std::ptr::null_mut(),
        };
        // SAFETY: name has static storage and the callback does not retain user_data.
        let slot = unsafe {
            context
                .register_c_host_function(&descriptor)
                .expect("valid C ABI descriptor")
        };
        let report = context
            .execute(
                "fn_host_call",
                &[Value::number(31.0)],
                ExecutionRequest::default(),
            )
            .expect("C-compatible callback executes");
        assert!(matches!(
            report.completion,
            ExecutionCompletion::Return(value) if value.as_number() == Some(42.0)
        ));
        context
            .unregister_host_function(slot)
            .expect("release C callback registration");
    }

    #[test]
    fn c_abi_primitive_entrypoint_checks_null_outputs_and_computes() {
        let status = unsafe { ogma_primitive_add_f64(1.5, 2.5, std::ptr::null_mut()) };
        assert_eq!(status, OgmaStatus::INVALID_ARGUMENT);
        let mut result = 0.0;
        let status = unsafe { ogma_primitive_add_f64(1.5, 2.5, &mut result) };
        assert_eq!(status, OgmaStatus::OK);
        assert_eq!(result, 4.0);
        assert_eq!(std::mem::size_of::<HostFunctionSlot>(), 24);
    }

    #[test]
    fn host_registry_rejects_stale_slots_and_busy_unregistration() {
        let registry = HostFunctionRegistry::new().expect("registry");
        let descriptor = HostFunctionDescriptor::new(10, "host.test");
        let slot = registry
            .register(descriptor, |_call, _arguments| {
                Ok(HostCallCompletion::Normal(Value::undefined()))
            })
            .expect("registration");
        assert_eq!(registry.table_version(), 1);
        registry.unregister(slot).expect("unregister");
        assert_eq!(registry.table_version(), 2);
        assert_eq!(
            registry.unregister(slot),
            Err(HostRegistrationError::StaleSlot)
        );
    }

    #[test]
    fn engine_rejects_invalid_resource_configuration_before_runtime_creation() {
        let invalid = EngineConfig {
            max_call_depth: 0,
            ..EngineConfig::default()
        };
        assert!(matches!(
            Engine::new(invalid),
            Err(ConfigurationError::ZeroCallDepth)
        ));
        let invalid = EngineConfig {
            max_allocation_bytes: 2 * 1024 * 1024,
            heap_quota_bytes: 1024 * 1024,
            ..EngineConfig::default()
        };
        assert!(matches!(
            Engine::new(invalid),
            Err(ConfigurationError::InvalidAllocationLimit)
        ));
    }

    #[test]
    fn independent_runtimes_have_independent_heap_identity_and_roots() {
        let engine = Engine::new(EngineConfig::default()).expect("valid config");
        let mut first = engine.create_runtime().expect("first Runtime");
        let mut second = engine.create_runtime().expect("second Runtime");
        assert_ne!(first.id(), second.id());
        let mut first_context = first.create_context().expect("first Context");
        let mut second_context = second.create_context().expect("second Context");
        let value = first_context
            .create_string("first")
            .expect("String allocation");
        let handle = first_context.persist(value).expect("persistent root");
        assert!(second_context.resolve(handle).is_err());
        first_context.release(handle).expect("release");
    }
}
