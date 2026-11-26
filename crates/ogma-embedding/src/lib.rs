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

mod jobs;
use jobs::JobScheduler;
pub use jobs::{JobPumpOutcome, JobPumpReport, JobQueueError, JobQueueState};

use ogma_interpreter::ExecutionOptions;
pub use ogma_interpreter::{
    CancellationToken, ExecutableFunctionInfo, ExecutableModule, ExecutionCompletion,
    ExecutionInterruption, ExecutionReport, Host, HostCallCompletion, HostCallContext,
    HostCallRoot, HostError, InstructionLocation, InterpreterError, NoHost,
};
pub use ogma_runtime::{CollectionReport, HeapError, HeapStats, Value};
use ogma_runtime::{Heap, HeapConfig, HostHandle, ScopeToken};

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
const MAX_C_CALLBACK_HANDLES: usize = 4096;
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
    UnsupportedFeatures {
        /// Bitset of required feature flags not provided by the current Runtime.
        missing: u64,
    },
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
    /// Maximum estimated active VM frame/register stack size per Runtime execution.
    pub max_stack_bytes: usize,
    /// Maximum dispatched interpreter instructions per top-level execution.
    pub instruction_budget: u64,
    /// Maximum nested interpreter/host execution depth.
    pub max_call_depth: u32,
    /// Maximum ordinary instruction dispatches between cooperative polls.
    pub interrupt_poll_interval: u32,
    /// Maximum number of verified executable modules retained by one Context.
    pub max_loaded_modules: usize,
    /// Capacity reserved for the host-driven pending-job scheduler.
    ///
    /// M1 synchronous execution does not enqueue jobs. OGM-48 supplies the
    /// queue/pump and consumes this already-validated resource policy.
    pub max_pending_jobs: usize,
    /// Maximum number of callbacks dispatched by one host pump call.
    pub max_jobs_per_pump: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            heap_quota_bytes: 64 * 1024 * 1024,
            max_allocation_bytes: 16 * 1024 * 1024,
            max_stack_bytes: 8 * 1024 * 1024,
            instruction_budget: 1_000_000,
            max_call_depth: 256,
            interrupt_poll_interval: 64,
            max_loaded_modules: 256,
            max_pending_jobs: 1024,
            max_jobs_per_pump: 64,
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
        if self.max_stack_bytes == 0 {
            return Err(ConfigurationError::ZeroStackQuota);
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
        if self.max_loaded_modules == 0 {
            return Err(ConfigurationError::ZeroLoadedModuleLimit);
        }
        if self.max_loaded_modules > 4096 {
            return Err(ConfigurationError::LoadedModuleLimitTooLarge);
        }
        if self.max_pending_jobs > 1_048_576 {
            return Err(ConfigurationError::PendingJobLimitTooLarge);
        }
        if self.max_jobs_per_pump == 0 || self.max_jobs_per_pump > 1_048_576 {
            return Err(ConfigurationError::InvalidJobPumpBudget);
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
    /// Per-Context executable-module capacity must be positive.
    ZeroLoadedModuleLimit,
    /// Per-Context executable-module capacity exceeds 4,096.
    LoadedModuleLimitTooLarge,
    /// Pending-job capacity exceeds the supported configuration ceiling.
    PendingJobLimitTooLarge,
    /// The per-pump job budget must be within 1..=1,048,576.
    InvalidJobPumpBudget,
    /// Stack quota must be positive.
    ZeroStackQuota,
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
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
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
            abi_runtime: Rc::new(OgmaRuntime::new(id)),
            next_context_id: 1,
            jobs: JobScheduler::default(),
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
    abi_runtime: Rc<OgmaRuntime>,
    next_context_id: u64,
    jobs: JobScheduler,
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
        let context_id = self.next_context_id;
        self.next_context_id = context_id
            .checked_add(1)
            .ok_or(RuntimeError::IdentitySpaceExhausted)?;
        let context_handle = self.abi_runtime.context_handle(context_id);
        let host_functions = HostFunctionRegistry::new(
            Rc::clone(&self.abi_runtime),
            context_handle,
            self.heap.identity(),
        )
        .map_err(RuntimeError::HostRegistry)?;
        Ok(Context {
            runtime: self,
            context_id,
            module: None,
            loaded_modules: Vec::new(),
            host_functions,
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
    /// Optional per-call stack quota override.
    pub max_stack_bytes: Option<usize>,
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
        if self.max_stack_bytes == Some(0) {
            return Err(ConfigurationError::ZeroStackQuota);
        }
        if self.instruction_budget == Some(0) {
            return Err(ConfigurationError::InvalidExecutionFuel);
        }
        if self.max_call_depth == Some(0) {
            return Err(ConfigurationError::InvalidExecutionCallDepth);
        }
        Ok(ExecutionOptions {
            instruction_budget: self.instruction_budget.unwrap_or(config.instruction_budget),
            max_call_depth: self.max_call_depth.unwrap_or(config.max_call_depth),
            max_stack_bytes: self.max_stack_bytes.unwrap_or(config.max_stack_bytes),
            interrupt_poll_interval: config.interrupt_poll_interval,
            cancellation: self.cancellation.clone(),
            deadline: self.deadline,
            time_budget: self.time_budget,
            global_this: self.global_this.unwrap_or_else(Value::undefined),
        })
    }
}

/// Stable identity of one verified module retained by a Context.
///
/// IDs are local to their Context, start at 1 and are never reused during that
/// Context's lifetime. Use this token to execute an older loaded module after
/// another module has become the active convenience target.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LoadedModuleId {
    runtime_id: u64,
    context_id: u64,
    sequence: u32,
}

impl LoadedModuleId {
    /// Returns the one-based numeric identity for diagnostics and host maps.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.sequence
    }

    /// Returns the owning Runtime identity.
    #[must_use]
    pub const fn runtime_id(self) -> u64 {
        self.runtime_id
    }

    /// Returns the owning Context identity within the Runtime.
    #[must_use]
    pub const fn context_id(self) -> u64 {
        self.context_id
    }
}

struct LoadedModuleEntry {
    id: LoadedModuleId,
    executable: Rc<ExecutableModule>,
}

/// A loaded execution Context borrowing one Runtime.
///
/// Module metadata, host-function registration and capabilities are scoped to
/// this Context. The most recently loaded module is the direct execution target;
/// all verified modules remain registered as callback owners until the Context
/// is dropped. Managed values remain owned by the parent Runtime.
pub struct Context<'runtime> {
    runtime: &'runtime mut Runtime,
    context_id: u64,
    module: Option<Rc<ExecutableModule>>,
    loaded_modules: Vec<LoadedModuleEntry>,
    host_functions: HostFunctionRegistry,
}

impl Context<'_> {
    /// Loads and verifies another OGMB module into this Context.
    ///
    /// The most recently loaded module becomes the direct execution target.
    /// Previously loaded modules remain registered as owners for queued
    /// callbacks, up to EngineConfig.max_loaded_modules, until this Context
    /// is dropped. Pending jobs retain their own module owner after that.
    pub fn load_module<I, B>(&mut self, artifacts: I) -> Result<(), InterpreterError>
    where
        I: IntoIterator<Item = B>,
        B: AsRef<[u8]>,
    {
        self.load_module_with_id(artifacts).map(|_| ())
    }

    /// Loads a verified module and returns its stable Context-local identity.
    ///
    /// The module becomes the active target for `execute()` and remains
    /// addressable through `execute_in_module()` after later modules are
    /// loaded. The registry is bounded by EngineConfig.max_loaded_modules.
    pub fn load_module_with_id<I, B>(
        &mut self,
        artifacts: I,
    ) -> Result<LoadedModuleId, InterpreterError>
    where
        I: IntoIterator<Item = B>,
        B: AsRef<[u8]>,
    {
        let limit = self.runtime.config.max_loaded_modules;
        if self.loaded_modules.len() >= limit {
            return Err(InterpreterError::ModuleLimitExceeded { limit });
        }
        self.loaded_modules
            .try_reserve(1)
            .map_err(|_| InterpreterError::Runtime(HeapError::AllocationFailed))?;
        let numeric_id = u32::try_from(self.loaded_modules.len() + 1)
            .map_err(|_| InterpreterError::ModuleLimitExceeded { limit })?;
        let id = LoadedModuleId {
            runtime_id: self.runtime.id,
            context_id: self.context_id,
            sequence: numeric_id,
        };
        let module = Rc::new(ExecutableModule::load(artifacts)?);
        self.module = Some(Rc::clone(&module));
        self.loaded_modules.push(LoadedModuleEntry {
            id,
            executable: module,
        });
        Ok(id)
    }

    /// Returns the number of verified modules retained by this Context.
    #[must_use]
    pub fn loaded_module_count(&self) -> usize {
        self.loaded_modules.len()
    }

    /// Returns the identity of the most recently loaded module, if any.
    #[must_use]
    pub fn latest_loaded_module_id(&self) -> Option<LoadedModuleId> {
        self.loaded_modules.last().map(|entry| entry.id)
    }

    fn loaded_module_entry(
        &self,
        module_id: LoadedModuleId,
    ) -> Result<&LoadedModuleEntry, ContextError> {
        self.loaded_modules
            .iter()
            .find(|entry| entry.id == module_id)
            .ok_or(ContextError::LoadedModuleNotFound(module_id))
    }

    /// Returns the number of verified functions in a retained module.
    pub fn function_count_in_module(
        &self,
        module_id: LoadedModuleId,
    ) -> Result<usize, ContextError> {
        Ok(self.loaded_module_entry(module_id)?.executable.function_count())
    }

    /// Lists verified functions in any retained module in deterministic stable-ID order.
    ///
    /// Use the returned function ID with execute_in_module(); names are descriptive only.
    pub fn functions_in_module(
        &self,
        module_id: LoadedModuleId,
    ) -> Result<Vec<ExecutableFunctionInfo>, ContextError> {
        Ok(self.loaded_module_entry(module_id)?.executable.functions())
    }

    /// Returns the number of verified functions in the active module.
    #[must_use]
    pub fn function_count(&self) -> usize {
        self.module
            .as_ref()
            .map_or(0, |module| module.function_count())
    }

    /// Lists the verified functions in deterministic stable-ID order.
    ///
    /// The returned source-level names are descriptive only; use each
    /// `ExecutableFunctionInfo.id` as the entry identifier for execution.
    pub fn functions(&self) -> Result<Vec<ExecutableFunctionInfo>, ContextError> {
        self.module
            .as_ref()
            .map(|module| module.functions())
            .ok_or(ContextError::ModuleNotLoaded)
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

    /// Executes a function from the loaded, verified module by stable OGMB function ID.
    ///
    /// Obtain the ID from `functions()`; the human-readable source-level name
    /// is not necessarily the executable function identity.
    pub fn execute(
        &mut self,
        entry_function_id: &str,
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
                entry_function_id,
                arguments,
                &mut host,
                options,
            )
            .map_err(ContextError::Interpreter)
    }

    /// Executes a function from any retained module by stable module and function IDs.
    ///
    /// This uses the same Runtime heap and host-function registry as the active
    /// module. The module ID is returned by load_module_with_id().
    pub fn execute_in_module(
        &mut self,
        module_id: LoadedModuleId,
        entry_function_id: &str,
        arguments: &[Value],
        request: ExecutionRequest,
    ) -> Result<ExecutionReport, ContextError> {
        let options = request
            .options(&self.runtime.config)
            .map_err(ContextError::Configuration)?;
        let module = Rc::clone(&self.loaded_module_entry(module_id)?.executable);
        let mut host = self.host_functions.clone();
        module
            .execute(
                &mut self.runtime.heap,
                entry_function_id,
                arguments,
                &mut host,
                options,
            )
            .map_err(ContextError::Interpreter)
    }

    /// Executes a function by stable OGMB ID and immediately promotes its completion.
    ///
    /// Use the function ID returned by `functions()`, not its source-level name.
    /// This API avoids exposing a transient GC-managed result that could be
    /// collected by the next allocation.
    pub fn execute_persistent(
        &mut self,
        entry_function_id: &str,
        arguments: &[Value],
        request: ExecutionRequest,
    ) -> Result<PersistentExecutionReport, ContextError> {
        let report = self.execute(entry_function_id, arguments, request)?;
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
            max_stack_bytes_reached: report.max_stack_bytes_reached,
            last_location: report.last_location,
        })
    }

    /// Executes a function in a retained module and promotes its completion.
    ///
    /// The returned values remain rooted until Context::release() is called.
    pub fn execute_persistent_in_module(
        &mut self,
        module_id: LoadedModuleId,
        entry_function_id: &str,
        arguments: &[Value],
        request: ExecutionRequest,
    ) -> Result<PersistentExecutionReport, ContextError> {
        let report = self.execute_in_module(module_id, entry_function_id, arguments, request)?;
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
            max_stack_bytes_reached: report.max_stack_bytes_reached,
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

    /// Runs a full tracing collection while this root scope remains active.
    pub fn collect(&mut self) -> Result<CollectionReport, HeapError> {
        self.heap.collect()
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
    /// Peak estimated active VM frame/register-stack bytes.
    pub max_stack_bytes_reached: usize,
    /// Source location of the last dispatched instruction.
    pub last_location: Option<InstructionLocation>,
}

/// Failure while executing a Context.
#[derive(Debug)]
pub enum ContextError {
    /// No verified module has been loaded.
    ModuleNotLoaded,
    /// The requested module ID is not retained by this Context.
    LoadedModuleNotFound(LoadedModuleId),
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
    /// Identity of the owning host registry.
    pub registry_id: u64,
    /// Zero-based index in the registry slot table.
    pub slot_index: u32,
    /// Generation used to reject stale registrations.
    pub slot_generation: u32,
    /// ABI major version of the registration token.
    pub abi_major: u16,
    /// Minimum ABI minor version of the registration token.
    pub abi_minor: u16,
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

/// Opaque owner-thread Runtime pointer supplied to C host callbacks.
pub struct OgmaRuntime {
    runtime_id: u64,
    owner_thread: ThreadId,
    active_context_ptr: Cell<*mut c_void>,
    active_context: Cell<OgmaContextHandle>,
    next_handle_id: Cell<u64>,
    active_handles: RefCell<HashMap<OgmaHandle, HostCallRoot>>,
    handle_frames: RefCell<Vec<Vec<OgmaHandle>>>,
}

// SAFETY: OgmaRuntime is intentionally owner-thread-affine. All methods that
// touch Cell/RefCell state and all exported callback helpers validate the
// immutable owner_thread field before accessing that state. Non-owner callers
// return WrongThread without touching the interior-mutability fields. The
// unsafe C API requires the runtime pointer to remain alive during each call.
unsafe impl Sync for OgmaRuntime {}

/// Fixed-size opaque handle for one Context in a Runtime.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct OgmaContextHandle {
    opaque: [u64; 5],
}

/// Fixed-size opaque transient value handle.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct OgmaHandle {
    opaque: [u64; 5],
}

/// Fixed-size opaque temporary root-scope handle.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct OgmaRootScopeHandle {
    opaque: [u64; 5],
}

/// Fixed-size opaque persistent value handle.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct OgmaPersistentHandle {
    opaque: [u64; 5],
}

/// Fixed-size opaque host callback registration handle.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct OgmaCallbackHandle {
    opaque: [u64; 5],
}

/// Completion kind for a normal JavaScript return.
pub const COMPLETION_NORMAL: u32 = 0;
/// Completion kind for a JavaScript throw.
pub const COMPLETION_THROW: u32 = 1;
/// Completion kind for a JavaScript return completion.
pub const COMPLETION_RETURN: u32 = 2;
/// Completion kind for a JavaScript break completion.
pub const COMPLETION_BREAK: u32 = 3;
/// Completion kind for a JavaScript continue completion.
pub const COMPLETION_CONTINUE: u32 = 4;

/// C-compatible completion carrying an opaque Runtime value.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OgmaCompletion {
    /// One of the COMPLETION_* tags.
    pub kind: u32,
    /// Reserved; must be zero.
    pub reserved: u32,
    /// Opaque completion value.
    pub value: OgmaHandle,
}

/// Generic host callback from the canonical ABI-v1 host-call contract.
pub type OgmaHostCallback = unsafe extern "C-unwind" fn(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    arguments: *const OgmaHandle,
    arguments_len: usize,
    completion: *mut OgmaCompletion,
    user_data: *mut c_void,
) -> OgmaStatus;

/// Typed primitive (f64, f64) -> f64 callback.
pub type OgmaPrimitiveF64Thunk = unsafe extern "C-unwind" fn(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    left: f64,
    right: f64,
    out_result: *mut f64,
    user_data: *mut c_void,
) -> OgmaStatus;

/// Typed (OgmaHandle, OgmaHandle) -> f64 callback.
pub type OgmaNumberHandleF64Thunk = unsafe extern "C-unwind" fn(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    left: OgmaHandle,
    right: OgmaHandle,
    out_result: *mut f64,
    user_data: *mut c_void,
) -> OgmaStatus;

/// Generic host signature tag from the canonical ABI-v1 descriptor.
pub const SIGNATURE_GENERIC: u32 = 0;
/// Primitive f64 binary signature tag.
pub const SIGNATURE_F64_BINARY: u32 = 1;
/// Opaque number-handle binary signature tag.
pub const SIGNATURE_NUMBER_HANDLE_BINARY: u32 = 2;
/// Stable signature identity for generic host functions.
pub const SIGNATURE_ID_GENERIC: u64 = 0;
/// Stable signature identity for (f64, f64) -> f64.
pub const SIGNATURE_ID_F64_BINARY: u64 = 0x1001;
/// Stable signature identity for (OgmaHandle, OgmaHandle) -> f64.
pub const SIGNATURE_ID_NUMBER_HANDLE_BINARY: u64 = 0x1002;

/// Host-function descriptor with the exact canonical ABI-v1 field layout.
///
/// The inline name buffer and reserved fields intentionally match the M0
/// ogma_host_dispatch.h contract; do not reorder or append fields.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OgmaHostFunctionDescriptor {
    /// Must equal size_of::<OgmaHostFunctionDescriptor>().
    pub struct_size: u32,
    /// Descriptor schema version; currently 1.
    pub descriptor_version: u16,
    /// Required ABI major.
    pub abi_major: u16,
    /// Minimum ABI minor.
    pub abi_minor: u16,
    /// Reserved; must be zero.
    pub reserved: u16,
    /// Stable non-zero host function identity.
    pub function_id: u64,
    /// Inline UTF-8 function name.
    pub name: [u8; 128],
    /// Number of valid bytes in name.
    pub name_len: u16,
    /// Reserved; must be zero.
    pub reserved_name: u16,
    /// One of the SIGNATURE_* tags.
    pub signature: u32,
    /// Stable signature ID matching the signature tag.
    pub signature_id: u64,
    /// Declared host effect bitset.
    pub effects: u32,
    /// Reserved; must be zero.
    pub reserved_effects: u32,
    /// Capabilities required for every invocation.
    pub required_capabilities: u64,
    /// Optional primitive f64 thunk.
    pub primitive_f64_thunk: Option<OgmaPrimitiveF64Thunk>,
    /// Optional opaque number-handle thunk.
    pub number_handle_f64_thunk: Option<OgmaNumberHandleF64Thunk>,
    /// Required generic callback fallback.
    pub generic_callback: Option<OgmaHostCallback>,
    /// Host-owned callback data, valid until unregistration succeeds.
    pub user_data: *mut c_void,
}

impl OgmaRuntime {
    fn new(runtime_id: u64) -> Self {
        Self {
            runtime_id,
            owner_thread: thread::current().id(),
            active_context_ptr: Cell::new(std::ptr::null_mut()),
            active_context: Cell::new(OgmaContextHandle::default()),
            next_handle_id: Cell::new(1),
            active_handles: RefCell::new(HashMap::new()),
            handle_frames: RefCell::new(Vec::new()),
        }
    }

    fn context_handle(&self, context_id: u64) -> OgmaContextHandle {
        OgmaContextHandle {
            opaque: [
                self.runtime_id,
                context_id,
                ABI_VERSION as u64,
                ABI_MINOR as u64,
                0,
            ],
        }
    }

    fn enter_call(
        self: &Rc<Self>,
        host_context: *mut c_void,
        context: OgmaContextHandle,
    ) -> Result<ActiveCContextGuard, HostError> {
        if thread::current().id() != self.owner_thread || host_context.is_null() {
            return Err(HostError::Denied);
        }
        let mut frames = self.handle_frames.borrow_mut();
        frames.try_reserve(1).map_err(|_| HostError::Denied)?;
        frames.push(Vec::new());
        drop(frames);
        let previous_context_ptr = self.active_context_ptr.replace(host_context);
        let previous_context = self.active_context.replace(context);
        Ok(ActiveCContextGuard {
            runtime: Rc::clone(self),
            host_context,
            previous_context_ptr,
            previous_context,
        })
    }

    fn create_temp_handle(
        &self,
        host_context: &mut HostCallContext<'_>,
        context: OgmaContextHandle,
        value: Value,
    ) -> Result<OgmaHandle, HostError> {
        if self.active_context.get() != context {
            return Err(HostError::Denied);
        }
        let id = self.next_handle_id.get();
        let next_id = id.checked_add(1).ok_or(HostError::Denied)?;
        {
            let mut frames = self.handle_frames.borrow_mut();
            let Some(frame) = frames.last_mut() else {
                return Err(HostError::Denied);
            };
            if frame.len() >= MAX_C_CALLBACK_HANDLES {
                return Err(HostError::Denied);
            }
            frame.try_reserve(1).map_err(|_| HostError::Denied)?;
        }
        self.active_handles
            .borrow_mut()
            .try_reserve(1)
            .map_err(|_| HostError::Denied)?;
        let root = host_context
            .retain_value(value)
            .map_err(HostError::Runtime)?;
        let handle = OgmaHandle {
            opaque: [self.runtime_id, context.opaque[1], id, 1, 0],
        };
        self.active_handles.borrow_mut().insert(handle, root);
        self.handle_frames
            .borrow_mut()
            .last_mut()
            .ok_or(HostError::Denied)?
            .push(handle);
        self.next_handle_id.set(next_id);
        Ok(handle)
    }

    fn resolve_temp_handle(
        &self,
        host_context: &HostCallContext<'_>,
        context: OgmaContextHandle,
        handle: OgmaHandle,
    ) -> Result<Value, HostError> {
        if handle == OgmaHandle::default() {
            return Ok(Value::undefined());
        }
        if self.active_context.get() != context {
            return Err(HostError::Denied);
        }
        let root = self
            .active_handles
            .borrow()
            .get(&handle)
            .copied()
            .ok_or(HostError::Denied)?;
        host_context.resolve_root(root).map_err(HostError::Runtime)
    }
}

struct ActiveCContextGuard {
    runtime: Rc<OgmaRuntime>,
    host_context: *mut c_void,
    previous_context_ptr: *mut c_void,
    previous_context: OgmaContextHandle,
}

impl Drop for ActiveCContextGuard {
    fn drop(&mut self) {
        let handles = self
            .runtime
            .handle_frames
            .borrow_mut()
            .pop()
            .unwrap_or_default();
        if !self.host_context.is_null() {
            // SAFETY: enter_call stores a pointer derived from the active
            // HostCallContext mutable borrow. This guard is dropped before
            // that callback invocation returns and before the borrow is used
            // again. The pointer is never retained beyond this scope.
            let host_context = unsafe { &mut *self.host_context.cast::<HostCallContext<'_>>() };
            let mut roots = self.runtime.active_handles.borrow_mut();
            for handle in handles {
                if let Some(root) = roots.remove(&handle) {
                    let _ = host_context.release_root(root);
                }
            }
        }
        self.runtime
            .active_context_ptr
            .set(self.previous_context_ptr);
        self.runtime.active_context.set(self.previous_context);
    }
}

unsafe fn active_c_host_context<'runtime, 'context>(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
) -> Result<
    (
        &'runtime OgmaRuntime,
        &'context mut HostCallContext<'context>,
    ),
    OgmaStatus,
> {
    if runtime.is_null() {
        return Err(OgmaStatus::INVALID_ARGUMENT);
    }
    // SAFETY: the caller supplies the Runtime pointer received in its callback.
    let runtime = unsafe { &*runtime };
    if thread::current().id() != runtime.owner_thread {
        return Err(OgmaStatus::WRONG_THREAD);
    }
    if runtime.active_context.get() != context {
        return Err(OgmaStatus::STALE_HANDLE);
    }
    let host_context = runtime.active_context_ptr.get();
    if host_context.is_null() {
        return Err(OgmaStatus::BUSY);
    }
    // SAFETY: ActiveCContextGuard installs this pointer from the current
    // mutable HostCallContext borrow and resets it before callback dispatch
    // returns. The pointer never escapes the active callback helpers.
    Ok((runtime, unsafe {
        &mut *host_context.cast::<HostCallContext<'context>>()
    }))
}

/// Runs a full Runtime collection during an active C host callback.
///
/// Callback argument handles and values created through the C helpers remain
/// rooted for the callback's lifetime and survive this collection.
///
/// # Safety
///
/// `runtime` must be a live Runtime pointer passed to the current callback,
/// and `context` must be its active Context handle. The call must occur on
/// the Runtime owner thread. The pointer must remain valid for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ogma_context_collect_unrooted(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
) -> OgmaStatus {
    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the callback supplies its active Runtime and Context handles.
        let (_runtime, host_context) = unsafe { active_c_host_context(runtime, context)? };
        host_context
            .collect()
            .map_err(|_| OgmaStatus::INTERNAL_ERROR)?;
        Ok::<_, OgmaStatus>(())
    }));
    match result {
        Ok(Ok(())) => OgmaStatus::OK,
        Ok(Err(status)) => status,
        Err(_) => OgmaStatus::PANIC,
    }
}

/// Creates a JavaScript Number during a C host callback.
///
/// # Safety
///
/// `runtime` must be a live Runtime pointer passed to the current callback,
/// and `context` must be its active Context handle. `out_value` must point
/// to writable, properly aligned storage for one `OgmaHandle`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ogma_context_create_number(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    number: f64,
    out_value: *mut OgmaHandle,
) -> OgmaStatus {
    if out_value.is_null() {
        return OgmaStatus::INVALID_ARGUMENT;
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: runtime/context are supplied by the active C callback.
        let (runtime, host_context) = unsafe { active_c_host_context(runtime, context)? };
        let value = runtime
            .create_temp_handle(host_context, context, Value::number(number))
            .map_err(|_| OgmaStatus::INTERNAL_ERROR)?;
        // SAFETY: caller provided writable output storage.
        unsafe { out_value.write(value) };
        Ok::<_, OgmaStatus>(())
    }));
    match result {
        Ok(Ok(())) => OgmaStatus::OK,
        Ok(Err(status)) => status,
        Err(_) => OgmaStatus::PANIC,
    }
}

/// Reads a JavaScript Number handle during a C host callback.
///
/// # Safety
///
/// `runtime` must be a live Runtime pointer passed to the current callback,
/// and `context`/ `value` must be handles from that callback. `out_number`
/// must point to writable, properly aligned storage for one `f64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ogma_context_get_number(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    value: OgmaHandle,
    out_number: *mut f64,
) -> OgmaStatus {
    if out_number.is_null() {
        return OgmaStatus::INVALID_ARGUMENT;
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: runtime/context are supplied by the active C callback.
        let (runtime, host_context) = unsafe { active_c_host_context(runtime, context)? };
        let value = runtime
            .resolve_temp_handle(host_context, context, value)
            .map_err(|_| OgmaStatus::STALE_HANDLE)?;
        let number = value.as_number().ok_or(OgmaStatus::TYPE_MISMATCH)?;
        // SAFETY: caller provided writable output storage.
        unsafe { out_number.write(number) };
        Ok::<_, OgmaStatus>(())
    }));
    match result {
        Ok(Ok(())) => OgmaStatus::OK,
        Ok(Err(status)) => status,
        Err(_) => OgmaStatus::PANIC,
    }
}

/// Creates a JavaScript Boolean during a C host callback.
///
/// # Safety
///
/// `runtime` must be a live Runtime pointer passed to the current callback,
/// and `context` must be its active Context handle. `out_value` must point
/// to writable, properly aligned storage for one `OgmaHandle`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ogma_context_create_boolean(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    boolean: u8,
    out_value: *mut OgmaHandle,
) -> OgmaStatus {
    if out_value.is_null() || boolean > 1 {
        return OgmaStatus::INVALID_ARGUMENT;
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: runtime/context are supplied by the active C callback.
        let (runtime, host_context) = unsafe { active_c_host_context(runtime, context)? };
        let value = runtime
            .create_temp_handle(host_context, context, Value::boolean(boolean == 1))
            .map_err(|_| OgmaStatus::INTERNAL_ERROR)?;
        // SAFETY: caller provided writable output storage.
        unsafe { out_value.write(value) };
        Ok::<_, OgmaStatus>(())
    }));
    match result {
        Ok(Ok(())) => OgmaStatus::OK,
        Ok(Err(status)) => status,
        Err(_) => OgmaStatus::PANIC,
    }
}

/// Reads a JavaScript Boolean handle during a C host callback.
///
/// # Safety
///
/// `runtime` must be a live Runtime pointer passed to the current callback,
/// and `context`/ `value` must be handles from that callback. `out_boolean`
/// must point to writable, properly aligned storage for one `u8`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ogma_context_get_boolean(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    value: OgmaHandle,
    out_boolean: *mut u8,
) -> OgmaStatus {
    if out_boolean.is_null() {
        return OgmaStatus::INVALID_ARGUMENT;
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: runtime/context are supplied by the active C callback.
        let (runtime, host_context) = unsafe { active_c_host_context(runtime, context)? };
        let value = runtime
            .resolve_temp_handle(host_context, context, value)
            .map_err(|_| OgmaStatus::STALE_HANDLE)?;
        let boolean = value.as_boolean().ok_or(OgmaStatus::TYPE_MISMATCH)?;
        // SAFETY: caller provided writable output storage.
        unsafe { out_boolean.write(u8::from(boolean)) };
        Ok::<_, OgmaStatus>(())
    }));
    match result {
        Ok(Ok(())) => OgmaStatus::OK,
        Ok(Err(status)) => status,
        Err(_) => OgmaStatus::PANIC,
    }
}

/// Converts an integral JavaScript Number to i32 with range and fraction checks.
///
/// # Safety
///
/// `runtime` must be a live Runtime pointer passed to the current callback,
/// and `context`/ `value` must be handles from that callback. `out_integer`
/// must point to writable, properly aligned storage for one `i32`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ogma_context_get_i32(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    value: OgmaHandle,
    out_integer: *mut i32,
) -> OgmaStatus {
    if out_integer.is_null() {
        return OgmaStatus::INVALID_ARGUMENT;
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: runtime/context are supplied by the active C callback.
        let (runtime, host_context) = unsafe { active_c_host_context(runtime, context)? };
        let value = runtime
            .resolve_temp_handle(host_context, context, value)
            .map_err(|_| OgmaStatus::STALE_HANDLE)?;
        let number = value.as_number().ok_or(OgmaStatus::TYPE_MISMATCH)?;
        if !number.is_finite()
            || number.fract() != 0.0
            || number < f64::from(i32::MIN)
            || number > f64::from(i32::MAX)
        {
            return Err(OgmaStatus::OUT_OF_RANGE);
        }
        // SAFETY: caller provided writable output storage.
        unsafe { out_integer.write(number as i32) };
        Ok::<_, OgmaStatus>(())
    }));
    match result {
        Ok(Ok(())) => OgmaStatus::OK,
        Ok(Err(status)) => status,
        Err(_) => OgmaStatus::PANIC,
    }
}

/// Sums a borrowed array of JavaScript Number handles during a C callback.
///
/// # Safety
///
/// `runtime` must be a live Runtime pointer passed to the current callback,
/// and `context` and all non-null `handles` entries must be handles from that
/// callback. When `count` is non-zero, `handles` must be readable for
/// `count` elements. `out_sum` must point to writable, properly aligned
/// storage for one `f64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ogma_context_sum_numbers(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    handles: *const OgmaHandle,
    count: usize,
    out_sum: *mut f64,
) -> OgmaStatus {
    if out_sum.is_null() || (count != 0 && handles.is_null()) || count > MAX_C_CALLBACK_HANDLES {
        return OgmaStatus::INVALID_ARGUMENT;
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: runtime/context are supplied by the active C callback.
        let (runtime, host_context) = unsafe { active_c_host_context(runtime, context)? };
        // SAFETY: the caller supplies a readable array for count elements.
        let handles = if count == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(handles, count) }
        };
        let mut sum = 0.0;
        for handle in handles {
            let value = runtime
                .resolve_temp_handle(host_context, context, *handle)
                .map_err(|_| OgmaStatus::STALE_HANDLE)?;
            sum += value.as_number().ok_or(OgmaStatus::TYPE_MISMATCH)?;
        }
        // SAFETY: caller provided writable output storage.
        unsafe { out_sum.write(sum) };
        Ok::<_, OgmaStatus>(())
    }));
    match result {
        Ok(Ok(())) => OgmaStatus::OK,
        Ok(Err(status)) => status,
        Err(_) => OgmaStatus::PANIC,
    }
}

type RustHostCallback = dyn for<'a> Fn(&mut HostCallContext<'a>, &[Value]) -> Result<HostCallCompletion, HostError>
    + 'static;

#[derive(Clone)]
enum HostHandler {
    Rust(Rc<RustHostCallback>),
    C(CHostHandler),
}

#[derive(Clone, Copy)]
struct CHostHandler {
    signature: u32,
    signature_id: u64,
    effects: u32,
    generic_callback: OgmaHostCallback,
    primitive_f64_thunk: Option<OgmaPrimitiveF64Thunk>,
    number_handle_f64_thunk: Option<OgmaNumberHandleF64Thunk>,
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
    abi_runtime: Rc<OgmaRuntime>,
    context_handle: OgmaContextHandle,
    heap_identity: u64,
}

impl HostFunctionRegistry {
    /// Creates an independent empty host-function table.
    pub fn new(
        abi_runtime: Rc<OgmaRuntime>,
        context_handle: OgmaContextHandle,
        heap_identity: u64,
    ) -> Result<Self, HostRegistrationError> {
        let id = NEXT_REGISTRY_ID
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
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
            abi_runtime,
            context_handle,
            heap_identity,
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

    /// Registers a C-compatible host function using the canonical M0 ABI-v1 descriptor.
    ///
    /// The fixed-size inline name, signature identifiers, opaque callback
    /// handles and descriptor version are validated before publication.
    ///
    /// # Safety
    ///
    /// The generic callback and any selected typed thunk must remain valid
    /// until successful unregistration. user_data must remain valid for the
    /// same period. Callbacks must obey their declared C-unwind ABI.
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
            || descriptor.reserved_name != 0
            || descriptor.reserved_effects != 0
            || descriptor.function_id == 0
            || descriptor.name_len == 0
            || usize::from(descriptor.name_len) > descriptor.name.len()
            || descriptor.effects & !KNOWN_EFFECTS != 0
            || descriptor.effects & EFFECT_MAY_SUSPEND != 0
            || descriptor.generic_callback.is_none()
        {
            return Err(HostRegistrationError::InvalidDescriptor);
        }
        let name_bytes = &descriptor.name[..usize::from(descriptor.name_len)];
        let name = std::str::from_utf8(name_bytes)
            .map_err(|_| HostRegistrationError::InvalidDescriptor)?;
        if name.as_bytes().contains(&0) {
            return Err(HostRegistrationError::InvalidDescriptor);
        }
        let signature_valid = match descriptor.signature {
            SIGNATURE_GENERIC => {
                descriptor.signature_id == SIGNATURE_ID_GENERIC
                    && descriptor.primitive_f64_thunk.is_none()
                    && descriptor.number_handle_f64_thunk.is_none()
            }
            SIGNATURE_F64_BINARY => {
                descriptor.signature_id == SIGNATURE_ID_F64_BINARY
                    && descriptor.primitive_f64_thunk.is_some()
                    && descriptor.number_handle_f64_thunk.is_none()
                    && descriptor.effects & COMPLEX_EFFECTS == 0
            }
            SIGNATURE_NUMBER_HANDLE_BINARY => {
                descriptor.signature_id == SIGNATURE_ID_NUMBER_HANDLE_BINARY
                    && descriptor.primitive_f64_thunk.is_none()
                    && descriptor.number_handle_f64_thunk.is_some()
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
            signature_id: descriptor.signature_id,
            effects: descriptor.effects,
            generic_callback: descriptor
                .generic_callback
                .ok_or(HostRegistrationError::InvalidDescriptor)?,
            primitive_f64_thunk: descriptor.primitive_f64_thunk,
            number_handle_f64_thunk: descriptor.number_handle_f64_thunk,
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
        if context.heap_identity() != self.heap_identity {
            return Err(HostError::WrongRuntime);
        }
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
            HostHandler::C(handler) => {
                let host_context = (context as *mut HostCallContext<'_>).cast::<c_void>();
                let _scope = self
                    .abi_runtime
                    .enter_call(host_context, self.context_handle)?;
                invoke_c_handler(
                    &self.abi_runtime,
                    self.context_handle,
                    context,
                    handler,
                    arguments,
                )
            }
        }
    }
}

fn throw_host_error(
    context: &mut HostCallContext<'_>,
    message: &str,
) -> Result<HostCallCompletion, HostError> {
    let value = context.create_string(message).map_err(HostError::Runtime)?;
    Ok(HostCallCompletion::Throw(value))
}

fn invoke_c_handler(
    runtime: &Rc<OgmaRuntime>,
    context_handle: OgmaContextHandle,
    context: &mut HostCallContext<'_>,
    handler: CHostHandler,
    arguments: &[Value],
) -> Result<HostCallCompletion, HostError> {
    let runtime_ptr = Rc::as_ptr(runtime);

    if handler.signature == SIGNATURE_F64_BINARY
        && handler.signature_id == SIGNATURE_ID_F64_BINARY
        && handler.effects & COMPLEX_EFFECTS == 0
        && let (Some(thunk), [left, right]) = (handler.primitive_f64_thunk, arguments)
        && let (Some(left), Some(right)) = (left.as_number(), right.as_number())
    {
        let mut result = 0.0;
        let status = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: descriptor validation pins callback/user_data lifetime;
            // the ABI runtime and context handles are valid for this scope.
            unsafe {
                thunk(
                    runtime_ptr,
                    context_handle,
                    left,
                    right,
                    &mut result,
                    handler.user_data,
                )
            }
        }))
        .map_err(|_| HostError::Panicked)?;
        if status == OgmaStatus::OK {
            return Ok(HostCallCompletion::Normal(Value::number(result)));
        }
        if status != OgmaStatus::TYPE_MISMATCH {
            return callback_status_to_host_error(context, status);
        }
        // A type mismatch in the typed fast path deliberately falls back to
        // the required generic callback instead of leaking a transport status.
    }

    if handler.signature == SIGNATURE_NUMBER_HANDLE_BINARY
        && handler.signature_id == SIGNATURE_ID_NUMBER_HANDLE_BINARY
        && handler.effects & COMPLEX_EFFECTS == 0
        && let (Some(thunk), [left, right]) = (handler.number_handle_f64_thunk, arguments)
    {
        let left_handle = runtime.create_temp_handle(context, context_handle, *left)?;
        let right_handle = runtime.create_temp_handle(context, context_handle, *right)?;
        let mut result = 0.0;
        let status = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: descriptor validation pins callback/user_data lifetime;
            // handles are rooted until this callback scope exits.
            unsafe {
                thunk(
                    runtime_ptr,
                    context_handle,
                    left_handle,
                    right_handle,
                    &mut result,
                    handler.user_data,
                )
            }
        }))
        .map_err(|_| HostError::Panicked)?;
        if status == OgmaStatus::OK {
            return Ok(HostCallCompletion::Normal(Value::number(result)));
        }
        if status != OgmaStatus::TYPE_MISMATCH {
            return callback_status_to_host_error(context, status);
        }
        // Signature mismatch uses the generic callback as a checked fallback.
    }

    let mut handles = Vec::new();
    handles
        .try_reserve_exact(arguments.len())
        .map_err(|_| HostError::Runtime(HeapError::AllocationFailed))?;
    for argument in arguments {
        handles.push(runtime.create_temp_handle(context, context_handle, *argument)?);
    }
    let mut completion = OgmaCompletion::default();
    let status = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: descriptor validation pins callback/user_data lifetime;
        // the handles and completion remain alive for this synchronous call.
        unsafe {
            (handler.generic_callback)(
                runtime_ptr,
                context_handle,
                handles.as_ptr(),
                handles.len(),
                &mut completion,
                handler.user_data,
            )
        }
    }))
    .map_err(|_| HostError::Panicked)?;
    if status != OgmaStatus::OK {
        return callback_status_to_host_error(context, status);
    }
    if completion.reserved != 0 || !matches!(completion.kind, COMPLETION_NORMAL | COMPLETION_THROW)
    {
        return Err(HostError::InvalidCompletion);
    }
    let value = runtime.resolve_temp_handle(context, context_handle, completion.value)?;
    Ok(if completion.kind == COMPLETION_THROW {
        HostCallCompletion::Throw(value)
    } else {
        HostCallCompletion::Normal(value)
    })
}

fn callback_status_to_host_error(
    context: &mut HostCallContext<'_>,
    status: OgmaStatus,
) -> Result<HostCallCompletion, HostError> {
    match status {
        OgmaStatus::TYPE_MISMATCH => {
            throw_host_error(context, "TypeError: host callback argument mismatch")
        }
        OgmaStatus::OUT_OF_RANGE => {
            throw_host_error(context, "RangeError: host callback conversion failed")
        }
        OgmaStatus::STALE_HANDLE => {
            throw_host_error(context, "ReferenceError: stale or foreign host handle")
        }
        OgmaStatus::WRONG_THREAD | OgmaStatus::BUSY | OgmaStatus::REENTRANCY_LIMIT => {
            Err(HostError::CallbackFailed)
        }
        OgmaStatus::PANIC => Err(HostError::Panicked),
        OgmaStatus::INVALID_COMPLETION => Err(HostError::InvalidCompletion),
        _ => Err(HostError::CallbackFailed),
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
        let add_id = context
            .functions()
            .expect("verified function metadata")
            .into_iter()
            .find(|function| function.name == "add")
            .expect("compiled add function")
            .id;
        let report = context
            .execute_persistent(
                &add_id,
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
    fn loaded_modules_have_stable_ids_and_can_execute_on_the_same_runtime() {
        let engine = Engine::new(EngineConfig::default()).expect("valid config");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");

        let first_id = context
            .load_module_with_id(compile_source("function answer() { return 7; }"))
            .expect("first module");
        let second_id = context
            .load_module_with_id(compile_source("function answer() { return 9; }"))
            .expect("second module");
        let first_functions = context
            .functions_in_module(first_id)
            .expect("first module functions remain addressable");
        let first_function = first_functions
            .iter()
            .find(|function| function.name == "answer")
            .expect("first answer function");
        let second_function = context
            .functions()
            .expect("active module functions")
            .into_iter()
            .find(|function| function.name == "answer")
            .expect("second answer function");

        assert_eq!(
            context
                .function_count_in_module(first_id)
                .expect("first module function count"),
            1
        );
        assert_ne!(first_id, second_id);
        assert_eq!(first_id.get(), 1);
        assert_eq!(second_id.get(), 2);
        assert_eq!(context.latest_loaded_module_id(), Some(second_id));

        let active = context
            .execute(&second_function.id, &[], ExecutionRequest::default())
            .expect("active module execution");
        assert!(matches!(
            active.completion,
            ExecutionCompletion::Return(value) if value.as_number() == Some(9.0)
        ));

        let earlier = context
            .execute_in_module(
                first_id,
                &first_function.id,
                &[],
                ExecutionRequest::default(),
            )
            .expect("execute earlier module in the same Runtime");
        assert!(matches!(
            earlier.completion,
            ExecutionCompletion::Return(value) if value.as_number() == Some(7.0)
        ));

        let persistent = context
            .execute_persistent_in_module(
                first_id,
                &first_function.id,
                &[],
                ExecutionRequest::default(),
            )
            .expect("persistent execution in earlier module");
        let PersistentCompletion::Return(persistent_value) = persistent.completion else {
            panic!("expected a persistent return value");
        };
        assert_eq!(
            context.resolve(persistent_value).expect("persistent value").as_number(),
            Some(7.0)
        );
        context.release(persistent_value).expect("release persistent value");

        let missing_id = LoadedModuleId {
            runtime_id: first_id.runtime_id(),
            context_id: first_id.context_id(),
            sequence: 99,
        };
        assert!(matches!(
            context.execute_in_module(
                missing_id,
                &first_function.id,
                &[],
                ExecutionRequest::default()
            ),
            Err(ContextError::LoadedModuleNotFound(id)) if id == missing_id
        ));
        assert!(matches!(
            context.functions_in_module(missing_id),
            Err(ContextError::LoadedModuleNotFound(id)) if id == missing_id
        ));

        drop(context);
        let mut another_context = runtime.create_context().expect("second Context");
        let other_context_id = another_context
            .load_module_with_id(compile_source("function answer() { return 7; }"))
            .expect("same module in second Context");
        assert_eq!(other_context_id.get(), first_id.get());
        assert_ne!(other_context_id, first_id);
        assert!(matches!(
            another_context.execute_in_module(
                first_id,
                &first_function.id,
                &[],
                ExecutionRequest::default()
            ),
            Err(ContextError::LoadedModuleNotFound(id)) if id == first_id
        ));
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

    unsafe extern "C-unwind" fn c_get_i32(
        runtime: *const OgmaRuntime,
        context: OgmaContextHandle,
        arguments: *const OgmaHandle,
        arguments_len: usize,
        out_completion: *mut OgmaCompletion,
        _user_data: *mut c_void,
    ) -> OgmaStatus {
        if arguments.is_null() || out_completion.is_null() || arguments_len != 1 {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        // SAFETY: the dispatcher passes a readable array of arguments_len handles.
        let argument = unsafe { *arguments };
        let mut integer = 0_i32;
        // SAFETY: Runtime/context are borrowed for this callback invocation.
        let status = unsafe { ogma_context_get_i32(runtime, context, argument, &mut integer) };
        if status != OgmaStatus::OK {
            return status;
        }
        let mut value = OgmaHandle::default();
        // SAFETY: Runtime/context are borrowed for this callback invocation.
        let status =
            unsafe { ogma_context_create_number(runtime, context, f64::from(integer), &mut value) };
        if status != OgmaStatus::OK {
            return status;
        }
        // SAFETY: completion points to writable dispatcher-owned storage.
        unsafe {
            out_completion.write(OgmaCompletion {
                kind: COMPLETION_NORMAL,
                reserved: 0,
                value,
            })
        };
        OgmaStatus::OK
    }

    unsafe extern "C-unwind" fn c_boolean_echo(
        runtime: *const OgmaRuntime,
        context: OgmaContextHandle,
        arguments: *const OgmaHandle,
        arguments_len: usize,
        out_completion: *mut OgmaCompletion,
        _user_data: *mut c_void,
    ) -> OgmaStatus {
        if arguments.is_null() || out_completion.is_null() || arguments_len != 1 {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        // SAFETY: the dispatcher supplies a readable array of argument handles.
        let argument = unsafe { *arguments };
        let mut boolean = 0_u8;
        // SAFETY: the callback receives the active Runtime/Context pair.
        let status = unsafe { ogma_context_get_boolean(runtime, context, argument, &mut boolean) };
        if status != OgmaStatus::OK {
            return status;
        }
        let mut value = OgmaHandle::default();
        // SAFETY: the callback receives the active Runtime/Context pair.
        let status = unsafe { ogma_context_create_boolean(runtime, context, boolean, &mut value) };
        if status != OgmaStatus::OK {
            return status;
        }
        // SAFETY: out_completion is writable dispatcher-owned storage.
        unsafe {
            out_completion.write(OgmaCompletion {
                kind: COMPLETION_NORMAL,
                reserved: 0,
                value,
            })
        };
        OgmaStatus::OK
    }

    unsafe extern "C-unwind" fn c_panics(
        _runtime: *const OgmaRuntime,
        _context: OgmaContextHandle,
        _arguments: *const OgmaHandle,
        _arguments_len: usize,
        _out_completion: *mut OgmaCompletion,
        _user_data: *mut c_void,
    ) -> OgmaStatus {
        panic!("C-unwind host callback panic must be contained");
    }

    #[test]
    fn canonical_c_callback_reads_and_returns_boolean_values() {
        let engine = Engine::new(EngineConfig::default()).expect("valid config");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        context
            .load_module(host_call_artifacts())
            .expect("verified OGMB");
        let descriptor = c_host_descriptor(
            b"benchmark.host.echo",
            84,
            SIGNATURE_GENERIC,
            SIGNATURE_ID_GENERIC,
            None,
            None,
            c_boolean_echo,
            std::ptr::null_mut(),
        );
        // SAFETY: callback has static lifetime and no user data.
        let slot = unsafe {
            context
                .register_c_host_function(&descriptor)
                .expect("register Boolean callback")
        };
        let report = context
            .execute(
                "fn_host_call",
                &[Value::boolean(true)],
                ExecutionRequest::default(),
            )
            .expect("C Boolean conversion roundtrip");
        assert!(matches!(
            report.completion,
            ExecutionCompletion::Return(value) if value.as_boolean() == Some(true)
        ));
        context.unregister_host_function(slot).expect("unregister");
    }

    #[test]
    fn checked_c_number_to_i32_conversion_maps_fractional_values_to_throw() {
        let engine = Engine::new(EngineConfig::default()).expect("valid config");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        context
            .load_module(host_call_artifacts())
            .expect("verified OGMB");
        let descriptor = c_host_descriptor(
            b"benchmark.host.echo",
            82,
            SIGNATURE_GENERIC,
            SIGNATURE_ID_GENERIC,
            None,
            None,
            c_get_i32,
            std::ptr::null_mut(),
        );
        // SAFETY: callback has static lifetime and no user data.
        let slot = unsafe {
            context
                .register_c_host_function(&descriptor)
                .expect("register C callback")
        };
        let report = context
            .execute(
                "fn_host_call",
                &[Value::number(31.5)],
                ExecutionRequest::default(),
            )
            .expect("conversion errors are script Throw completions");
        let ExecutionCompletion::Throw(message) = report.completion else {
            panic!("fractional Number must fail checked i32 conversion");
        };
        let text = String::from_utf16(
            &context
                .read_string_utf16(message)
                .expect("RangeError string"),
        )
        .expect("valid RangeError text");
        assert!(text.starts_with("RangeError:"));
        context.unregister_host_function(slot).expect("unregister");
    }

    #[test]
    fn c_unwind_callback_panics_are_contained_before_returning_to_the_interpreter() {
        let engine = Engine::new(EngineConfig::default()).expect("valid config");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        context
            .load_module(host_call_artifacts())
            .expect("verified OGMB");
        let descriptor = c_host_descriptor(
            b"benchmark.host.echo",
            83,
            SIGNATURE_GENERIC,
            SIGNATURE_ID_GENERIC,
            None,
            None,
            c_panics,
            std::ptr::null_mut(),
        );
        // SAFETY: callback has static lifetime and no user data.
        let slot = unsafe {
            context
                .register_c_host_function(&descriptor)
                .expect("register panicking C callback")
        };
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
        context.unregister_host_function(slot).expect("unregister");
    }

    #[test]
    fn abi_negotiation_c_entrypoint_validates_output_and_features() {
        let null_status = unsafe {
            ogma_abi_negotiate(
                ABI_VERSION,
                ABI_MINOR,
                FEATURE_CALLBACKS,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(null_status, OgmaStatus::INVALID_ARGUMENT);

        let mut version = OgmaAbiVersion::default();
        let accepted =
            unsafe { ogma_abi_negotiate(ABI_VERSION, ABI_MINOR, FEATURE_CALLBACKS, &mut version) };
        assert_eq!(accepted, OgmaStatus::OK);
        assert_eq!(version, OgmaAbiVersion::current());

        let rejected =
            unsafe { ogma_abi_negotiate(ABI_VERSION, ABI_MINOR, 1_u64 << 63, &mut version) };
        assert_eq!(rejected, OgmaStatus::UNSUPPORTED_FEATURE);
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
        runtime: *const OgmaRuntime,
        context: OgmaContextHandle,
        arguments: *const OgmaHandle,
        arguments_len: usize,
        out_completion: *mut OgmaCompletion,
        user_data: *mut c_void,
    ) -> OgmaStatus {
        if arguments.is_null() || out_completion.is_null() || arguments_len != 1 {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        // SAFETY: the host dispatcher supplies a readable slice for arguments_len.
        let argument = unsafe { *arguments };
        if !user_data.is_null() {
            // SAFETY: the test host keeps this output slot alive until unregister.
            unsafe { (user_data.cast::<OgmaHandle>()).write(argument) };
        }
        let mut number = 0.0;
        // Force GC while the argument handle is still callback-scoped.
        // SAFETY: callback receives its active Runtime and Context handles.
        let collection_status = unsafe { ogma_context_collect_unrooted(runtime, context) };
        if collection_status != OgmaStatus::OK {
            return collection_status;
        }
        // SAFETY: callback receives its active Runtime and Context handles.
        let read_status =
            unsafe { ogma_context_get_number(runtime, context, argument, &mut number) };
        if read_status != OgmaStatus::OK {
            return read_status;
        }
        let mut result = OgmaHandle::default();
        // SAFETY: callback receives its active Runtime and Context handles.
        let create_status =
            unsafe { ogma_context_create_number(runtime, context, number + 11.0, &mut result) };
        if create_status != OgmaStatus::OK {
            return create_status;
        }
        let completion = OgmaCompletion {
            kind: COMPLETION_NORMAL,
            reserved: 0,
            value: result,
        };
        // SAFETY: the host dispatcher supplies writable completion storage.
        unsafe { out_completion.write(completion) };
        OgmaStatus::OK
    }

    unsafe extern "C-unwind" fn c_generic_two_args_unexpected(
        _runtime: *const OgmaRuntime,
        _context: OgmaContextHandle,
        _arguments: *const OgmaHandle,
        _arguments_len: usize,
        _out_completion: *mut OgmaCompletion,
        _user_data: *mut c_void,
    ) -> OgmaStatus {
        OgmaStatus::CALLBACK_ERROR
    }

    unsafe extern "C-unwind" fn c_invalid_completion(
        _runtime: *const OgmaRuntime,
        _context: OgmaContextHandle,
        _arguments: *const OgmaHandle,
        _arguments_len: usize,
        out_completion: *mut OgmaCompletion,
        _user_data: *mut c_void,
    ) -> OgmaStatus {
        if out_completion.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        // Return/Break/Continue are script completions, not valid host callback results.
        unsafe {
            out_completion.write(OgmaCompletion {
                kind: COMPLETION_RETURN,
                reserved: 0,
                value: OgmaHandle::default(),
            })
        };
        OgmaStatus::OK
    }

    unsafe extern "C-unwind" fn c_primitive_type_mismatch(
        _runtime: *const OgmaRuntime,
        _context: OgmaContextHandle,
        _left: f64,
        _right: f64,
        _out_result: *mut f64,
        _user_data: *mut c_void,
    ) -> OgmaStatus {
        OgmaStatus::TYPE_MISMATCH
    }

    unsafe extern "C-unwind" fn c_primitive_add(
        _runtime: *const OgmaRuntime,
        _context: OgmaContextHandle,
        left: f64,
        right: f64,
        out_result: *mut f64,
        _user_data: *mut c_void,
    ) -> OgmaStatus {
        if out_result.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        // SAFETY: caller passes a writable f64 output slot.
        unsafe { out_result.write(left + right) };
        OgmaStatus::OK
    }

    unsafe extern "C-unwind" fn c_number_handle_add(
        runtime: *const OgmaRuntime,
        context: OgmaContextHandle,
        left: OgmaHandle,
        right: OgmaHandle,
        out_result: *mut f64,
        _user_data: *mut c_void,
    ) -> OgmaStatus {
        if out_result.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        let mut left_number = 0.0;
        let mut right_number = 0.0;
        // SAFETY: callback receives its active Runtime and Context handles.
        let left_status =
            unsafe { ogma_context_get_number(runtime, context, left, &mut left_number) };
        if left_status != OgmaStatus::OK {
            return left_status;
        }
        // SAFETY: callback receives its active Runtime and Context handles.
        let right_status =
            unsafe { ogma_context_get_number(runtime, context, right, &mut right_number) };
        if right_status != OgmaStatus::OK {
            return right_status;
        }
        // SAFETY: caller passes a writable f64 output slot.
        unsafe { out_result.write(left_number + right_number) };
        OgmaStatus::OK
    }

    fn host_call_two_args_artifacts() -> Vec<Vec<u8>> {
        let document: FunctionDocument = from_str(include_str!(
            "../../../tests/core-ir/fixtures/positive-host-call-two-args.json"
        ))
        .expect("two-argument host-call fixture");
        ogma_compiler::bytecode::compile_functions(&[document])
            .expect("two-argument Core IR fixture compiles to OGMB")
            .into_iter()
            .map(|function| function.artifact.bytes)
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn c_host_descriptor(
        name: &[u8],
        function_id: u64,
        signature: u32,
        signature_id: u64,
        primitive_f64_thunk: Option<OgmaPrimitiveF64Thunk>,
        number_handle_f64_thunk: Option<OgmaNumberHandleF64Thunk>,
        generic_callback: OgmaHostCallback,
        user_data: *mut c_void,
    ) -> OgmaHostFunctionDescriptor {
        let mut inline_name = [0_u8; 128];
        inline_name[..name.len()].copy_from_slice(name);
        OgmaHostFunctionDescriptor {
            struct_size: std::mem::size_of::<OgmaHostFunctionDescriptor>() as u32,
            descriptor_version: DESCRIPTOR_VERSION,
            abi_major: ABI_VERSION,
            abi_minor: ABI_MINOR,
            reserved: 0,
            function_id,
            name: inline_name,
            name_len: name.len() as u16,
            reserved_name: 0,
            signature,
            signature_id,
            effects: 0,
            reserved_effects: 0,
            required_capabilities: 0,
            primitive_f64_thunk,
            number_handle_f64_thunk,
            generic_callback: Some(generic_callback),
            user_data,
        }
    }

    #[test]
    fn canonical_c_abi_layouts_match_the_m0_contract() {
        assert_eq!(std::mem::size_of::<OgmaAbiVersion>(), 24);
        assert_eq!(std::mem::size_of::<OgmaContextHandle>(), 40);
        assert_eq!(std::mem::size_of::<OgmaHandle>(), 40);
        assert_eq!(std::mem::size_of::<OgmaRootScopeHandle>(), 40);
        assert_eq!(std::mem::size_of::<OgmaPersistentHandle>(), 40);
        assert_eq!(std::mem::size_of::<OgmaCallbackHandle>(), 40);
        assert_eq!(std::mem::size_of::<OgmaCompletion>(), 48);
        assert_eq!(std::mem::size_of::<OgmaHostFunctionDescriptor>(), 216);
        assert_eq!(std::mem::size_of::<HostFunctionSlot>(), 24);
    }

    #[test]
    fn canonical_c_callback_dispatches_opaque_handles_and_expires_them_after_return() {
        let engine = Engine::new(EngineConfig::default()).expect("valid config");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        context
            .load_module(host_call_artifacts())
            .expect("verified OGMB");
        let mut saved_argument = OgmaHandle::default();
        let descriptor = c_host_descriptor(
            b"benchmark.host.echo",
            42,
            SIGNATURE_GENERIC,
            SIGNATURE_ID_GENERIC,
            None,
            None,
            c_generic_echo,
            (&mut saved_argument as *mut OgmaHandle).cast(),
        );
        // SAFETY: callback has static lifetime and user_data is alive to unregister.
        let slot = unsafe {
            context
                .register_c_host_function(&descriptor)
                .expect("canonical ABI descriptor")
        };
        let report = context
            .execute(
                "fn_host_call",
                &[Value::number(31.0)],
                ExecutionRequest::default(),
            )
            .expect("C callback executes through the production Host registry");
        assert!(matches!(
            report.completion,
            ExecutionCompletion::Return(value) if value.as_number() == Some(42.0)
        ));

        let mut ignored = 0.0;
        // SAFETY: the Runtime pointer and context token belong to this Context.
        let stale = unsafe {
            ogma_context_get_number(
                Rc::as_ptr(&context.runtime.abi_runtime),
                context.host_functions.context_handle,
                saved_argument,
                &mut ignored,
            )
        };
        assert_eq!(stale, OgmaStatus::STALE_HANDLE);
        context
            .unregister_host_function(slot)
            .expect("release callback registration");
    }

    #[test]
    fn canonical_primitive_thunk_dispatch_uses_the_typed_f64_path() {
        let engine = Engine::new(EngineConfig::default()).expect("valid config");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        context
            .load_module(host_call_two_args_artifacts())
            .expect("verified two-argument OGMB");
        let descriptor = c_host_descriptor(
            b"benchmark.host.echo",
            43,
            SIGNATURE_F64_BINARY,
            SIGNATURE_ID_F64_BINARY,
            Some(c_primitive_add),
            None,
            c_generic_two_args_unexpected,
            std::ptr::null_mut(),
        );
        // SAFETY: static callbacks and null user_data satisfy the ABI contract.
        let slot = unsafe {
            context
                .register_c_host_function(&descriptor)
                .expect("valid typed descriptor")
        };
        let report = context
            .execute(
                "fn_host_call",
                &[Value::number(19.0), Value::number(23.0)],
                ExecutionRequest::default(),
            )
            .expect("typed primitive host call");
        assert!(matches!(
            report.completion,
            ExecutionCompletion::Return(value) if value.as_number() == Some(42.0)
        ));
        context.unregister_host_function(slot).expect("unregister");
    }

    #[test]
    fn c_callback_transport_failure_is_not_reported_as_capability_denial() {
        let engine = Engine::new(EngineConfig::default()).expect("valid config");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        context
            .load_module(host_call_two_args_artifacts())
            .expect("verified two-argument OGMB");
        let descriptor = c_host_descriptor(
            b"benchmark.host.echo",
            45,
            SIGNATURE_F64_BINARY,
            SIGNATURE_ID_F64_BINARY,
            Some(c_primitive_type_mismatch),
            None,
            c_generic_two_args_unexpected,
            std::ptr::null_mut(),
        );
        // SAFETY: both callbacks have static lifetime and user_data is null.
        let slot = unsafe {
            context
                .register_c_host_function(&descriptor)
                .expect("valid typed descriptor")
        };
        let error = context
            .execute(
                "fn_host_call",
                &[Value::number(19.0), Value::number(23.0)],
                ExecutionRequest::default(),
            )
            .expect_err("generic fallback reports its callback transport failure");
        assert!(matches!(
            error,
            ContextError::Interpreter(InterpreterError::HostCallbackFailed)
        ));
        context.unregister_host_function(slot).expect("unregister");
    }

    #[test]
    fn malformed_c_callback_completion_is_rejected_as_an_abi_error() {
        let engine = Engine::new(EngineConfig::default()).expect("valid config");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        context
            .load_module(host_call_artifacts())
            .expect("verified OGMB");
        let descriptor = c_host_descriptor(
            b"benchmark.host.echo",
            46,
            SIGNATURE_GENERIC,
            SIGNATURE_ID_GENERIC,
            None,
            None,
            c_invalid_completion,
            std::ptr::null_mut(),
        );
        // SAFETY: callback has static lifetime and user_data is null.
        let slot = unsafe {
            context
                .register_c_host_function(&descriptor)
                .expect("valid generic descriptor")
        };
        let error = context
            .execute(
                "fn_host_call",
                &[Value::number(19.0)],
                ExecutionRequest::default(),
            )
            .expect_err("script completions are invalid as C host callback results");
        assert!(matches!(
            error,
            ContextError::Interpreter(InterpreterError::InvalidHostCompletion)
        ));
        context.unregister_host_function(slot).expect("unregister");
    }

    #[test]
    fn canonical_handle_thunk_uses_the_same_scoped_runtime_handles() {
        let engine = Engine::new(EngineConfig::default()).expect("valid config");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        context
            .load_module(host_call_two_args_artifacts())
            .expect("verified two-argument OGMB");
        let descriptor = c_host_descriptor(
            b"benchmark.host.echo",
            44,
            SIGNATURE_NUMBER_HANDLE_BINARY,
            SIGNATURE_ID_NUMBER_HANDLE_BINARY,
            None,
            Some(c_number_handle_add),
            c_generic_two_args_unexpected,
            std::ptr::null_mut(),
        );
        // SAFETY: static callbacks and null user_data satisfy the ABI contract.
        let slot = unsafe {
            context
                .register_c_host_function(&descriptor)
                .expect("valid handle-signature descriptor")
        };
        let report = context
            .execute(
                "fn_host_call",
                &[Value::number(19.0), Value::number(23.0)],
                ExecutionRequest::default(),
            )
            .expect("typed opaque-handle host call");
        assert!(matches!(
            report.completion,
            ExecutionCompletion::Return(value) if value.as_number() == Some(42.0)
        ));
        context.unregister_host_function(slot).expect("unregister");
    }

    #[test]
    fn invalid_c_descriptor_signature_and_reserved_fields_are_rejected() {
        let engine = Engine::new(EngineConfig::default()).expect("Engine");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let context = runtime.create_context().expect("Context");
        let mut descriptor = c_host_descriptor(
            b"host.invalid",
            51,
            SIGNATURE_GENERIC,
            SIGNATURE_ID_GENERIC,
            None,
            None,
            c_generic_echo,
            std::ptr::null_mut(),
        );
        descriptor.signature_id = SIGNATURE_ID_F64_BINARY;
        // SAFETY: the descriptor's callback and inline name remain valid.
        assert_eq!(
            unsafe { context.register_c_host_function(&descriptor) },
            Err(HostRegistrationError::InvalidDescriptor)
        );
        descriptor.signature_id = SIGNATURE_ID_GENERIC;
        descriptor.struct_size = descriptor.struct_size.saturating_sub(1);
        // SAFETY: descriptor callbacks and inline name remain valid.
        assert_eq!(
            unsafe { context.register_c_host_function(&descriptor) },
            Err(HostRegistrationError::InvalidDescriptor)
        );
        descriptor.struct_size = std::mem::size_of::<OgmaHostFunctionDescriptor>() as u32;
        descriptor.reserved_effects = 1;
        // SAFETY: the descriptor's callback and inline name remain valid.
        assert_eq!(
            unsafe { context.register_c_host_function(&descriptor) },
            Err(HostRegistrationError::InvalidDescriptor)
        );
    }

    #[test]
    fn registered_rust_host_callback_reenters_the_same_runtime_context() {
        let outer = host_call_artifacts();
        let nested_document: FunctionDocument = from_str(include_str!(
            "../../../tests/core-ir/fixtures/positive-ssa-branch.json"
        ))
        .expect("nested Core IR fixture");
        let nested = ogma_compiler::bytecode::compile_functions(&[nested_document])
            .expect("nested fixture compiles to OGMB")
            .into_iter()
            .map(|function| function.artifact.bytes)
            .collect::<Vec<_>>();
        let nested = ExecutableModule::load(nested).expect("nested module verifies");

        let engine = Engine::new(EngineConfig::default()).expect("valid config");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        context.load_module(outer).expect("outer module verifies");
        context
            .register_host_function(
                HostFunctionDescriptor::new(81, "benchmark.host.echo"),
                move |call, _arguments| {
                    call.collect().map_err(HostError::Runtime)?;
                    let mut no_host = NoHost;
                    match call
                        .execute_nested(
                            &nested,
                            "fn0",
                            &[Value::number(7.0), Value::number(7.0)],
                            &mut no_host,
                        )
                        .map_err(|_| HostError::Panicked)?
                    {
                        ExecutionCompletion::Return(value) => Ok(HostCallCompletion::Normal(value)),
                        ExecutionCompletion::Throw(value) => Ok(HostCallCompletion::Throw(value)),
                        ExecutionCompletion::Interrupted(reason) => {
                            Ok(HostCallCompletion::Interrupted(reason))
                        }
                    }
                },
            )
            .expect("register re-entrant host callback");

        let report = context
            .execute(
                "fn_host_call",
                &[Value::number(31.0)],
                ExecutionRequest::default(),
            )
            .expect("nested execution reuses the active Runtime");
        assert_eq!(report.max_call_depth_reached, 2);
        assert!(report.max_stack_bytes_reached > 0);
        assert!(report.instructions_executed >= 4);
        assert!(matches!(
            report.completion,
            ExecutionCompletion::Return(value) if value.as_number() == Some(7.0)
        ));
    }

    #[test]
    fn cloned_host_registry_rejects_dispatch_on_a_different_runtime_heap() {
        let engine = Engine::new(EngineConfig::default()).expect("valid Engine");
        let mut first_runtime = engine.create_runtime().expect("first Runtime");
        let mut first_context = first_runtime.create_context().expect("first Context");
        first_context
            .load_module(host_call_artifacts())
            .expect("verified OGMB");
        let descriptor = c_host_descriptor(
            b"benchmark.host.echo",
            47,
            SIGNATURE_GENERIC,
            SIGNATURE_ID_GENERIC,
            None,
            None,
            c_generic_echo,
            std::ptr::null_mut(),
        );
        // SAFETY: callback has static lifetime and user_data is null.
        unsafe {
            first_context
                .register_c_host_function(&descriptor)
                .expect("register callback in first Runtime");
        }
        let mut detached_registry = first_context.host_function_registry();
        drop(first_context);
        drop(first_runtime);

        let mut second_runtime = engine.create_runtime().expect("second Runtime");
        let mut second_context = second_runtime.create_context().expect("second Context");
        second_context
            .load_module(host_call_artifacts())
            .expect("verified OGMB");
        let module = second_context.module.as_ref().expect("loaded module");
        let error = module
            .execute(
                &mut second_context.runtime.heap,
                "fn_host_call",
                &[Value::number(31.0)],
                &mut detached_registry,
                ExecutionOptions::default(),
            )
            .expect_err("a cloned registry must not dispatch on another Runtime heap");
        assert!(matches!(error, InterpreterError::HostRuntimeMismatch));
    }

    #[test]
    fn host_registry_rejects_stale_slots_and_busy_unregistration() {
        let engine = Engine::new(EngineConfig::default()).expect("Engine");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let context = runtime.create_context().expect("Context");
        let registry = context.host_function_registry();
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
    fn registry_rejects_unregistration_while_a_callback_is_active() {
        let engine = Engine::new(EngineConfig::default()).expect("Engine");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        context
            .load_module(host_call_artifacts())
            .expect("verified OGMB");
        let registry = context.host_function_registry();
        let slot_cell = Rc::new(RefCell::new(None));
        let callback_result = Rc::new(RefCell::new(None));
        let callback_registry = registry.clone();
        let callback_slot = Rc::clone(&slot_cell);
        let callback_result_slot = Rc::clone(&callback_result);
        let slot = registry
            .register(
                HostFunctionDescriptor::new(90, "benchmark.host.echo"),
                move |_call, _arguments| {
                    let slot = callback_slot
                        .borrow()
                        .expect("published callback slot before execution");
                    *callback_result_slot.borrow_mut() = Some(
                        callback_registry
                            .unregister(slot)
                            .expect_err("active dispatch is busy"),
                    );
                    Ok(HostCallCompletion::Normal(Value::number(42.0)))
                },
            )
            .expect("register callback");
        *slot_cell.borrow_mut() = Some(slot);

        let report = context
            .execute(
                "fn_host_call",
                &[Value::number(1.0)],
                ExecutionRequest::default(),
            )
            .expect("active callback returns normally");
        assert!(matches!(
            report.completion,
            ExecutionCompletion::Return(value) if value.as_number() == Some(42.0)
        ));
        assert_eq!(*callback_result.borrow(), Some(HostRegistrationError::Busy));
        registry
            .unregister(slot)
            .expect("unregister after dispatch");
    }

    #[test]
    fn engine_rejects_invalid_resource_configuration_before_runtime_creation() {
        assert!(EngineConfig::default().validate().is_ok());
        assert_eq!(EngineConfig::default().max_loaded_modules, 256);
        assert_eq!(EngineConfig::default().max_pending_jobs, 1024);
        assert_eq!(EngineConfig::default().max_jobs_per_pump, 64);
        let invalid_heap = EngineConfig {
            heap_quota_bytes: 0,
            ..EngineConfig::default()
        };
        assert!(matches!(
            Engine::new(invalid_heap),
            Err(ConfigurationError::ZeroHeapQuota)
        ));
        let invalid_fuel = EngineConfig {
            instruction_budget: 0,
            ..EngineConfig::default()
        };
        assert!(matches!(
            Engine::new(invalid_fuel),
            Err(ConfigurationError::ZeroInstructionBudget)
        ));
        let invalid_poll = EngineConfig {
            interrupt_poll_interval: 0,
            ..EngineConfig::default()
        };
        assert!(matches!(
            Engine::new(invalid_poll),
            Err(ConfigurationError::ZeroPollInterval)
        ));
        let invalid_module_limit = EngineConfig {
            max_loaded_modules: 0,
            ..EngineConfig::default()
        };
        assert!(matches!(
            Engine::new(invalid_module_limit),
            Err(ConfigurationError::ZeroLoadedModuleLimit)
        ));
        let oversized_module_limit = EngineConfig {
            max_loaded_modules: 4097,
            ..EngineConfig::default()
        };
        assert!(matches!(
            Engine::new(oversized_module_limit),
            Err(ConfigurationError::LoadedModuleLimitTooLarge)
        ));
        let invalid_jobs = EngineConfig {
            max_pending_jobs: 1_048_577,
            ..EngineConfig::default()
        };
        assert!(matches!(
            Engine::new(invalid_jobs),
            Err(ConfigurationError::PendingJobLimitTooLarge)
        ));
        let invalid_job_budget = EngineConfig {
            max_jobs_per_pump: 0,
            ..EngineConfig::default()
        };
        assert!(matches!(
            Engine::new(invalid_job_budget),
            Err(ConfigurationError::InvalidJobPumpBudget)
        ));
        let oversized_job_budget = EngineConfig {
            max_jobs_per_pump: 1_048_577,
            ..EngineConfig::default()
        };
        assert!(matches!(
            Engine::new(oversized_job_budget),
            Err(ConfigurationError::InvalidJobPumpBudget)
        ));
        let invalid = EngineConfig {
            max_call_depth: 0,
            ..EngineConfig::default()
        };
        assert!(matches!(
            Engine::new(invalid),
            Err(ConfigurationError::ZeroCallDepth)
        ));
        let invalid_stack = EngineConfig {
            max_stack_bytes: 0,
            ..EngineConfig::default()
        };
        assert!(matches!(
            Engine::new(invalid_stack),
            Err(ConfigurationError::ZeroStackQuota)
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
    fn rust_host_callback_uses_opaque_roots_across_forced_collection() {
        let engine = Engine::new(EngineConfig::default()).expect("valid Engine");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        context
            .load_module(host_call_artifacts())
            .expect("verified OGMB");
        context
            .register_host_function(
                HostFunctionDescriptor::new(78, "benchmark.host.echo"),
                |call, _arguments| {
                    let value = call
                        .create_string("callback root survives GC")
                        .map_err(HostError::Runtime)?;
                    let root = call.retain_value(value).map_err(HostError::Runtime)?;
                    call.collect().map_err(HostError::Runtime)?;
                    let rooted = call.resolve_root(root).map_err(HostError::Runtime)?;
                    call.release_root(root).map_err(HostError::Runtime)?;
                    Ok(HostCallCompletion::Normal(rooted))
                },
            )
            .expect("register Rust callback");

        let report = context
            .execute_persistent(
                "fn_host_call",
                &[Value::number(1.0)],
                ExecutionRequest::default(),
            )
            .expect("host callback roots its result during GC");
        let PersistentCompletion::Return(handle) = report.completion else {
            panic!("expected callback string Return");
        };
        let value = context.resolve(handle).expect("persistent callback result");
        let units = context
            .read_string_utf16(value)
            .expect("callback string survived collection");
        assert_eq!(
            String::from_utf16(&units).expect("valid callback string"),
            "callback root survives GC"
        );
        context.release(handle).expect("release result root");
    }

    #[test]
    fn temporary_root_scope_keeps_values_alive_during_collection_then_releases_them() {
        let engine = Engine::new(EngineConfig::default()).expect("valid Engine");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        let value = context.create_string("scoped value").expect("String");
        let mut scope = context.open_root_scope().expect("root scope");
        scope.root(value).expect("root the value");
        scope.collect().expect("collect while scoped");
        scope.close().expect("close root scope");

        let text = context
            .read_string_utf16(value)
            .expect("value survived the scoped collection");
        assert_eq!(
            String::from_utf16(&text).expect("valid text"),
            "scoped value"
        );
        context.collect().expect("collect after scope closure");
        assert!(context.read_string_utf16(value).is_err());
    }

    #[test]
    fn host_throw_completion_is_persisted_as_a_distinct_script_completion() {
        let engine = Engine::new(EngineConfig::default()).expect("valid Engine");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        context
            .load_module(host_call_artifacts())
            .expect("verified OGMB");
        context
            .register_host_function(
                HostFunctionDescriptor::new(77, "benchmark.host.echo"),
                |_call, _arguments| Ok(HostCallCompletion::Throw(Value::number(99.0))),
            )
            .expect("register throwing callback");

        let report = context
            .execute_persistent(
                "fn_host_call",
                &[Value::number(1.0)],
                ExecutionRequest::default(),
            )
            .expect("throw is a normal interpreter completion");
        let PersistentCompletion::Throw(handle) = report.completion else {
            panic!("host Throw must remain distinct from Return");
        };
        assert_eq!(
            context
                .resolve(handle)
                .expect("persistent throw value")
                .as_number(),
            Some(99.0)
        );
        context.collect().expect("throw value survives forced GC");
        assert_eq!(
            context
                .resolve(handle)
                .expect("persistent throw after GC")
                .as_number(),
            Some(99.0)
        );
        context
            .release(handle)
            .expect("release persistent thrown value");
    }

    #[test]
    fn c_callback_helpers_reject_wrong_thread_access_before_touching_the_heap() {
        let engine = Engine::new(EngineConfig::default()).expect("valid Engine");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let context = runtime.create_context().expect("Context");
        let runtime_ptr = Rc::as_ptr(&context.runtime.abi_runtime) as usize;
        let context_handle = context.host_functions.context_handle;
        let result = std::thread::spawn(move || {
            let mut number = 0.0;
            // SAFETY: pointer is kept alive by the joining parent thread; the
            // C helper rejects the non-owner thread before accessing Runtime cells.
            unsafe {
                ogma_context_get_number(
                    runtime_ptr as *const OgmaRuntime,
                    context_handle,
                    OgmaHandle::default(),
                    &mut number,
                )
            }
        })
        .join()
        .expect("wrong-thread helper call completes");
        assert_eq!(result, OgmaStatus::WRONG_THREAD);
    }

    #[test]
    fn execution_request_enforces_cancellation_and_deadline_at_entry() {
        let engine = Engine::new(EngineConfig::default()).expect("Engine");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        context
            .load_module(compile_source("function identity(value) { return value; }"))
            .expect("verified compiled source");
        let identity_id = context
            .functions()
            .expect("verified function metadata")
            .into_iter()
            .find(|function| function.name == "identity")
            .expect("compiled identity function")
            .id;

        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let cancelled = context
            .execute(
                &identity_id,
                &[Value::number(42.0)],
                ExecutionRequest {
                    cancellation: Some(cancellation),
                    ..ExecutionRequest::default()
                },
            )
            .expect("cancellation is an execution completion");
        assert!(matches!(
            cancelled.completion,
            ExecutionCompletion::Interrupted(ExecutionInterruption::Cancelled)
        ));
        assert_eq!(cancelled.instructions_executed, 0);

        let deadline = context
            .execute(
                &identity_id,
                &[Value::number(42.0)],
                ExecutionRequest {
                    time_budget: Some(Duration::ZERO),
                    ..ExecutionRequest::default()
                },
            )
            .expect("expired deadline is an execution completion");
        assert!(matches!(
            deadline.completion,
            ExecutionCompletion::Interrupted(ExecutionInterruption::DeadlineExceeded)
        ));
        assert_eq!(deadline.instructions_executed, 0);
    }

    #[test]
    fn execution_request_rejects_zero_per_call_stack_fuel_and_depth_limits() {
        let engine = Engine::new(EngineConfig::default()).expect("Engine");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        context
            .load_module(compile_source("function identity(value) { return value; }"))
            .expect("verified compiled source");
        let identity_id = context
            .functions()
            .expect("verified function metadata")
            .into_iter()
            .find(|function| function.name == "identity")
            .expect("compiled identity function")
            .id;

        for request in [
            ExecutionRequest {
                max_stack_bytes: Some(0),
                ..ExecutionRequest::default()
            },
            ExecutionRequest {
                instruction_budget: Some(0),
                ..ExecutionRequest::default()
            },
            ExecutionRequest {
                max_call_depth: Some(0),
                ..ExecutionRequest::default()
            },
        ] {
            assert!(matches!(
                context.execute(&identity_id, &[Value::number(1.0)], request),
                Err(ContextError::Configuration(_))
            ));
        }
    }

    #[test]
    fn independent_runtimes_have_independent_heap_identity_and_roots() {
        let engine = Engine::new(EngineConfig::default()).expect("valid config");
        let mut first = engine.create_runtime().expect("first Runtime");
        let mut second = engine.create_runtime().expect("second Runtime");
        assert_ne!(first.id(), second.id());
        let mut first_context = first.create_context().expect("first Context");
        let second_context = second.create_context().expect("second Context");
        let value = first_context
            .create_string("first")
            .expect("String allocation");
        let handle = first_context.persist(value).expect("persistent root");
        assert!(second_context.resolve(handle).is_err());
        first_context.release(handle).expect("release");
    }
}
