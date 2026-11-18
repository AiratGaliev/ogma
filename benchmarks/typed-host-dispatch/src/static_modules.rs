//! Declarative M0 proof for statically registered host modules.
//!
//! This layer composes the OGM-19 ABI and OGM-21 function table. It is not a
//! production module loader or ECMAScript module implementation.

use super::{
    HostFunctionDescriptor, HostFunctionRegistry, HostFunctionSignature, HostFunctionSlot,
};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;

pub const HOST_MODULE_DESCRIPTOR_VERSION: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct HostCapabilityRequirement {
    pub name: String,
    pub bit: u8,
}

impl HostCapabilityRequirement {
    pub fn new(name: impl Into<String>, bit: u8) -> Self {
        Self {
            name: name.into(),
            bit,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostExportBinding {
    /// The ESM export is an immutable binding to this registered host function.
    ImmutableHostFunction,
    /// The exported binding can change; it must be read through normal module semantics.
    Mutable,
    /// The export is resolved dynamically (for example, an accessor or exotic namespace).
    Dynamic,
}

#[derive(Clone)]
pub struct HostModuleExport {
    pub name: String,
    pub function: HostFunctionDescriptor,
    pub required_capabilities: Vec<HostCapabilityRequirement>,
    pub required_features: Vec<String>,
    pub binding: HostExportBinding,
}

#[derive(Clone)]
pub struct HostModuleDescriptor {
    pub descriptor_version: u16,
    pub abi_major: u16,
    pub abi_minor: u16,
    pub specifier: String,
    pub version: String,
    pub required_capabilities: Vec<HostCapabilityRequirement>,
    pub required_features: Vec<String>,
    pub dependencies: Vec<String>,
    pub native_dependencies: Vec<String>,
    pub exports: Vec<HostModuleExport>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ModuleDiagnosticCode {
    InvalidDescriptor,
    AbiMismatch,
    DuplicateModule,
    DuplicateExport,
    InvalidExportName,
    InvalidCapability,
    InvalidFeature,
    InvalidDependency,
    FunctionRegistrationFailed,
    UnknownModule,
    UnknownExport,
    ModuleNotEnabled,
    MissingDependency,
    DependencyCycle,
    MissingFeature,
    CapabilityDenied,
    RuntimeImportModuleStripped,
    NativeModuleDependencyStripped,
    MissingNativeDependency,
}

impl ModuleDiagnosticCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::InvalidDescriptor => "invalid_descriptor",
            Self::AbiMismatch => "abi_mismatch",
            Self::DuplicateModule => "duplicate_module",
            Self::DuplicateExport => "duplicate_export",
            Self::InvalidExportName => "invalid_export_name",
            Self::InvalidCapability => "invalid_capability",
            Self::InvalidFeature => "invalid_feature",
            Self::InvalidDependency => "invalid_dependency",
            Self::FunctionRegistrationFailed => "function_registration_failed",
            Self::UnknownModule => "unknown_module",
            Self::UnknownExport => "unknown_export",
            Self::ModuleNotEnabled => "module_not_enabled",
            Self::MissingDependency => "missing_dependency",
            Self::DependencyCycle => "dependency_cycle",
            Self::MissingFeature => "missing_feature",
            Self::CapabilityDenied => "capability_denied",
            Self::RuntimeImportModuleStripped => "runtime_import_module_stripped",
            Self::NativeModuleDependencyStripped => "native_module_dependency_stripped",
            Self::MissingNativeDependency => "missing_native_dependency",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ModuleDiagnostic {
    pub code: ModuleDiagnosticCode,
    pub module: String,
    pub subject: String,
    pub detail: String,
}

impl ModuleDiagnostic {
    fn new(
        code: ModuleDiagnosticCode,
        module: impl Into<String>,
        subject: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            code,
            module: module.into(),
            subject: subject.into(),
            detail: detail.into(),
        }
    }
}

impl fmt::Display for ModuleDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: module '{}'{}{}",
            self.code.as_str(),
            self.module,
            if self.subject.is_empty() {
                String::new()
            } else {
                format!(" [{}]", self.subject)
            },
            if self.detail.is_empty() {
                String::new()
            } else {
                format!(": {}", self.detail)
            },
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleImportKind {
    StaticNamedEsm,
    NamespaceEsm,
    DynamicImport,
    CommonJsRequire,
    DynamicPropertyAccess,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenericFallbackReason {
    NamespaceBinding,
    DynamicImport,
    CommonJsInterop,
    DynamicPropertyAccess,
    MutableExport,
    DynamicExport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleImportResolution {
    StaticFunctionSlot {
        slot: HostFunctionSlot,
        signature: HostFunctionSignature,
    },
    GenericFallback(GenericFallbackReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeModuleImport {
    pub specifier: String,
    pub kind: ModuleImportKind,
    pub imported_names: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct HostFeatureProfile {
    pub id: String,
    /// Explicit application roots. Declared module dependencies are added transitively.
    pub enabled_modules: BTreeSet<String>,
    pub enabled_features: BTreeSet<String>,
    pub granted_capabilities: Vec<HostCapabilityRequirement>,
    pub enabled_native_dependencies: BTreeSet<String>,
    /// All statically known or literal dynamic runtime module references.
    pub runtime_imports: Vec<RuntimeModuleImport>,
    /// Native adapters/modules that link against a host module.
    pub native_module_dependencies: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleBuildPlan {
    pub profile_id: String,
    pub included_modules: BTreeSet<String>,
    pub stripped_modules: BTreeSet<String>,
    pub native_dependencies: BTreeSet<String>,
    pub capability_mask: u64,
    pub enabled_features: BTreeSet<String>,
    pub granted_capabilities: BTreeMap<String, u8>,
}

#[derive(Clone)]
struct RegisteredExport {
    slot: HostFunctionSlot,
    signature: HostFunctionSignature,
    binding: HostExportBinding,
    required_capabilities: Vec<HostCapabilityRequirement>,
    required_features: BTreeSet<String>,
}

#[derive(Clone)]
struct RegisteredModule {
    version: String,
    required_capabilities: Vec<HostCapabilityRequirement>,
    required_features: BTreeSet<String>,
    dependencies: BTreeSet<String>,
    native_dependencies: BTreeSet<String>,
    exports: BTreeMap<String, RegisteredExport>,
}

pub struct HostModuleRegistry<'a> {
    functions: &'a HostFunctionRegistry,
    modules: RefCell<BTreeMap<String, RegisteredModule>>,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl<'a> HostModuleRegistry<'a> {
    pub fn new(functions: &'a HostFunctionRegistry) -> Self {
        Self {
            functions,
            modules: RefCell::new(BTreeMap::new()),
            _not_send_or_sync: PhantomData,
        }
    }

    /// Validates the whole descriptor before registering any export. If one function
    /// registration fails, previously registered slots from this module are rolled back.
    pub fn register(&self, descriptor: HostModuleDescriptor) -> Result<(), ModuleDiagnostic> {
        self.functions.ensure_owner_thread().map_err(|error| {
            ModuleDiagnostic::new(
                ModuleDiagnosticCode::FunctionRegistrationFailed,
                &descriptor.specifier,
                "",
                format!("{error:?}"),
            )
        })?;
        self.validate_descriptor(&descriptor)?;
        if self.modules.borrow().contains_key(&descriptor.specifier) {
            return Err(ModuleDiagnostic::new(
                ModuleDiagnosticCode::DuplicateModule,
                &descriptor.specifier,
                "",
                "module specifier is already registered",
            ));
        }

        let mut exports = BTreeMap::new();
        let mut registered_slots = Vec::new();
        for export in &descriptor.exports {
            let mut function = export.function;
            let mut all_capabilities = descriptor.required_capabilities.clone();
            all_capabilities.extend(export.required_capabilities.iter().cloned());
            let expected_mask = capabilities_mask(&all_capabilities).map_err(|reason| {
                ModuleDiagnostic::new(
                    ModuleDiagnosticCode::InvalidCapability,
                    &descriptor.specifier,
                    &export.name,
                    reason,
                )
            })?;
            if function.required_capabilities != 0
                && function.required_capabilities != expected_mask
            {
                for slot in registered_slots.iter().rev() {
                    let _ = self.functions.unregister(*slot);
                }
                return Err(ModuleDiagnostic::new(
                    ModuleDiagnosticCode::InvalidDescriptor,
                    &descriptor.specifier,
                    &export.name,
                    "function descriptor capability mask disagrees with named module/export metadata",
                ));
            }
            function.required_capabilities = expected_mask;

            let signature =
                HostFunctionSignature::from_abi_tag(function.signature).ok_or_else(|| {
                    ModuleDiagnostic::new(
                        ModuleDiagnosticCode::InvalidDescriptor,
                        &descriptor.specifier,
                        &export.name,
                        "unknown host function signature",
                    )
                })?;
            let slot = match self.functions.register(function) {
                Ok(slot) => slot,
                Err(error) => {
                    for registered in registered_slots.iter().rev() {
                        let _ = self.functions.unregister(*registered);
                    }
                    return Err(ModuleDiagnostic::new(
                        ModuleDiagnosticCode::FunctionRegistrationFailed,
                        &descriptor.specifier,
                        &export.name,
                        format!("{error:?}"),
                    ));
                }
            };
            registered_slots.push(slot);
            exports.insert(
                export.name.clone(),
                RegisteredExport {
                    slot,
                    signature,
                    binding: export.binding,
                    required_capabilities: export.required_capabilities.clone(),
                    required_features: export.required_features.iter().cloned().collect(),
                },
            );
        }

        self.modules.borrow_mut().insert(
            descriptor.specifier,
            RegisteredModule {
                version: descriptor.version,
                required_capabilities: descriptor.required_capabilities,
                required_features: descriptor.required_features.into_iter().collect(),
                dependencies: descriptor.dependencies.into_iter().collect(),
                native_dependencies: descriptor.native_dependencies.into_iter().collect(),
                exports,
            },
        );
        Ok(())
    }

    pub fn unregister(&self, specifier: &str) -> Result<(), ModuleDiagnostic> {
        self.functions.ensure_owner_thread().map_err(|error| {
            ModuleDiagnostic::new(
                ModuleDiagnosticCode::FunctionRegistrationFailed,
                specifier,
                "",
                format!("{error:?}"),
            )
        })?;
        if self.functions.active_dispatches.get() != 0 {
            return Err(ModuleDiagnostic::new(
                ModuleDiagnosticCode::FunctionRegistrationFailed,
                specifier,
                "",
                "module cannot be unregistered while a host dispatch is active",
            ));
        }
        let module = self
            .modules
            .borrow()
            .get(specifier)
            .cloned()
            .ok_or_else(|| {
                ModuleDiagnostic::new(
                    ModuleDiagnosticCode::UnknownModule,
                    specifier,
                    "",
                    "module is not registered",
                )
            })?;
        for export in module.exports.values() {
            if export.slot.slot_generation == u32::MAX {
                return Err(ModuleDiagnostic::new(
                    ModuleDiagnosticCode::FunctionRegistrationFailed,
                    specifier,
                    "",
                    "slot generation is exhausted; no exports were unregistered",
                ));
            }
            self.functions.snapshot(export.slot).map_err(|error| {
                ModuleDiagnostic::new(
                    ModuleDiagnosticCode::FunctionRegistrationFailed,
                    specifier,
                    "",
                    format!("{error:?}"),
                )
            })?;
        }

        let mut cleanup_errors = Vec::new();
        for export in module.exports.values() {
            if let Err(error) = self.functions.unregister(export.slot) {
                cleanup_errors.push(format!("{error:?}"));
            }
        }
        self.modules.borrow_mut().remove(specifier);

        if cleanup_errors.is_empty() {
            Ok(())
        } else {
            Err(ModuleDiagnostic::new(
                ModuleDiagnosticCode::FunctionRegistrationFailed,
                specifier,
                "",
                format!(
                    "module exports were unregistered, but ABI cleanup failed: {}",
                    cleanup_errors.join(", ")
                ),
            ))
        }
    }

    pub fn module_version(&self, specifier: &str) -> Option<String> {
        self.modules
            .borrow()
            .get(specifier)
            .map(|module| module.version.clone())
    }

    fn validate_descriptor(
        &self,
        descriptor: &HostModuleDescriptor,
    ) -> Result<(), ModuleDiagnostic> {
        let fail = |code, subject: &str, detail: &str| {
            ModuleDiagnostic::new(code, &descriptor.specifier, subject, detail)
        };
        if descriptor.descriptor_version != HOST_MODULE_DESCRIPTOR_VERSION {
            return Err(fail(
                ModuleDiagnosticCode::InvalidDescriptor,
                "descriptorVersion",
                "unsupported host module descriptor version",
            ));
        }
        if descriptor.abi_major != self.functions.abi_version.major
            || descriptor.abi_minor > self.functions.abi_version.minor
        {
            return Err(fail(
                ModuleDiagnosticCode::AbiMismatch,
                "abi",
                "module ABI version is not supported by the negotiated function registry",
            ));
        }
        if !valid_module_specifier(&descriptor.specifier) {
            return Err(fail(
                ModuleDiagnosticCode::InvalidDescriptor,
                "specifier",
                "expected a non-empty module specifier using ASCII letters, digits, ':', '/', '-', '_', '.', or '@'",
            ));
        }
        if !is_semver(&descriptor.version) {
            return Err(fail(
                ModuleDiagnosticCode::InvalidDescriptor,
                "version",
                "module version must use MAJOR.MINOR.PATCH numeric form",
            ));
        }
        capabilities_mask(&descriptor.required_capabilities)
            .map_err(|reason| fail(ModuleDiagnosticCode::InvalidCapability, "", &reason))?;
        validate_feature_names(&descriptor.required_features)
            .map_err(|reason| fail(ModuleDiagnosticCode::InvalidFeature, "", &reason))?;
        validate_unique_strings(&descriptor.dependencies, |value| {
            valid_module_specifier(value)
        })
        .map_err(|reason| {
            fail(
                ModuleDiagnosticCode::InvalidDependency,
                "dependencies",
                &reason,
            )
        })?;
        validate_unique_strings(&descriptor.native_dependencies, |value| {
            valid_native_dependency(value)
        })
        .map_err(|reason| {
            fail(
                ModuleDiagnosticCode::InvalidDependency,
                "nativeDependencies",
                &reason,
            )
        })?;
        if descriptor
            .dependencies
            .iter()
            .any(|dependency| dependency == &descriptor.specifier)
        {
            return Err(fail(
                ModuleDiagnosticCode::InvalidDependency,
                "dependencies",
                "module cannot depend on itself",
            ));
        }

        let mut names = BTreeSet::new();
        for export in &descriptor.exports {
            if !is_typescript_identifier(&export.name) {
                return Err(fail(
                    ModuleDiagnosticCode::InvalidExportName,
                    &export.name,
                    "export name must be a non-reserved TypeScript identifier in descriptor v1",
                ));
            }
            if !names.insert(export.name.clone()) {
                return Err(fail(
                    ModuleDiagnosticCode::DuplicateExport,
                    &export.name,
                    "export name occurs more than once",
                ));
            }
            validate_feature_names(&export.required_features).map_err(|reason| {
                fail(ModuleDiagnosticCode::InvalidFeature, &export.name, &reason)
            })?;
            let expected_name = format!("{}/{}", descriptor.specifier, export.name);
            let actual_name = export.function.name().map_err(|error| {
                fail(
                    ModuleDiagnosticCode::InvalidDescriptor,
                    &export.name,
                    &format!("invalid function descriptor name: {error:?}"),
                )
            })?;
            if actual_name != expected_name {
                return Err(fail(
                    ModuleDiagnosticCode::InvalidDescriptor,
                    &export.name,
                    &format!("function name must be '{expected_name}'"),
                ));
            }
            self.functions
                .validate_descriptor(&export.function)
                .map_err(|error| {
                    fail(
                        ModuleDiagnosticCode::InvalidDescriptor,
                        &export.name,
                        &format!("invalid function descriptor: {error:?}"),
                    )
                })?;
            let mut requirements = descriptor.required_capabilities.clone();
            requirements.extend(export.required_capabilities.iter().cloned());
            let expected_mask = capabilities_mask(&requirements).map_err(|reason| {
                fail(
                    ModuleDiagnosticCode::InvalidCapability,
                    &export.name,
                    &reason,
                )
            })?;
            if export.function.required_capabilities != 0
                && export.function.required_capabilities != expected_mask
            {
                return Err(fail(
                    ModuleDiagnosticCode::InvalidDescriptor,
                    &export.name,
                    "function descriptor capability mask disagrees with named module/export metadata",
                ));
            }
        }
        Ok(())
    }

    /// Computes a deterministic transitive module closure and verifies all references.
    /// Required runtime/native module references are never silently removed by stripping.
    pub fn build_profile(
        &self,
        profile: &HostFeatureProfile,
    ) -> Result<ModuleBuildPlan, Vec<ModuleDiagnostic>> {
        let modules = self.modules.borrow();
        let mut diagnostics = Vec::new();
        if profile.id.trim().is_empty() {
            diagnostics.push(ModuleDiagnostic::new(
                ModuleDiagnosticCode::InvalidDescriptor,
                "",
                "profile.id",
                "profile id must not be empty",
            ));
        }

        let granted_mask = match capabilities_mask(&profile.granted_capabilities) {
            Ok(mask) => mask,
            Err(reason) => {
                diagnostics.push(ModuleDiagnostic::new(
                    ModuleDiagnosticCode::InvalidCapability,
                    "",
                    "profile.grantedCapabilities",
                    reason,
                ));
                0
            }
        };
        let grant_map: BTreeMap<String, u8> = profile
            .granted_capabilities
            .iter()
            .map(|capability| (capability.name.clone(), capability.bit))
            .collect();
        let profile_features = profile.enabled_features.iter().cloned().collect::<Vec<_>>();
        if let Err(reason) = validate_feature_names(&profile_features) {
            diagnostics.push(ModuleDiagnostic::new(
                ModuleDiagnosticCode::InvalidFeature,
                "",
                "profile.enabledFeatures",
                reason,
            ));
        }
        let native_grants = profile
            .enabled_native_dependencies
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        if let Err(reason) = validate_unique_strings(&native_grants, valid_native_dependency) {
            diagnostics.push(ModuleDiagnostic::new(
                ModuleDiagnosticCode::InvalidDependency,
                "",
                "profile.enabledNativeDependencies",
                reason,
            ));
        }

        let mut included = BTreeSet::new();
        let mut visiting = BTreeSet::new();
        for root in &profile.enabled_modules {
            if !modules.contains_key(root) {
                diagnostics.push(ModuleDiagnostic::new(
                    ModuleDiagnosticCode::UnknownModule,
                    root,
                    "",
                    "profile selects a module that is not registered",
                ));
            } else {
                visit_module(
                    root,
                    &modules,
                    profile,
                    &grant_map,
                    &mut visiting,
                    &mut included,
                    &mut diagnostics,
                );
            }
        }

        for import in &profile.runtime_imports {
            if !modules.contains_key(&import.specifier) {
                diagnostics.push(ModuleDiagnostic::new(
                    ModuleDiagnosticCode::UnknownModule,
                    &import.specifier,
                    "runtimeImport",
                    "runtime import references an unregistered module",
                ));
                continue;
            }
            if !included.contains(&import.specifier) {
                diagnostics.push(ModuleDiagnostic::new(
                    ModuleDiagnosticCode::RuntimeImportModuleStripped,
                    &import.specifier,
                    "runtimeImport",
                    "module is referenced by runtime import but is not in the profile dependency closure",
                ));
                continue;
            }
            if import.kind == ModuleImportKind::StaticNamedEsm {
                if import.imported_names.is_empty() {
                    diagnostics.push(ModuleDiagnostic::new(
                        ModuleDiagnosticCode::InvalidDescriptor,
                        &import.specifier,
                        "runtimeImport",
                        "static named import must declare at least one imported name",
                    ));
                }
                let mut imported_names = BTreeSet::new();
                for name in &import.imported_names {
                    if !is_typescript_identifier(name) || !imported_names.insert(name) {
                        diagnostics.push(ModuleDiagnostic::new(
                            ModuleDiagnosticCode::InvalidDescriptor,
                            &import.specifier,
                            name,
                            "static import name is invalid or duplicated",
                        ));
                    }
                }
                if let Some(module) = modules.get(&import.specifier) {
                    for name in &import.imported_names {
                        match module.exports.get(name) {
                            None => diagnostics.push(ModuleDiagnostic::new(
                                ModuleDiagnosticCode::UnknownExport,
                                &import.specifier,
                                name,
                                "static import references an unknown export",
                            )),
                            Some(export) => {
                                for feature in &export.required_features {
                                    if !profile.enabled_features.contains(feature) {
                                        diagnostics.push(ModuleDiagnostic::new(
                                            ModuleDiagnosticCode::MissingFeature,
                                            &import.specifier,
                                            name,
                                            format!("export requires feature '{feature}'"),
                                        ));
                                    }
                                }
                                for capability in &export.required_capabilities {
                                    if grant_map.get(&capability.name) != Some(&capability.bit) {
                                        diagnostics.push(ModuleDiagnostic::new(
                                            ModuleDiagnosticCode::CapabilityDenied,
                                            &import.specifier,
                                            name,
                                            format!("missing capability '{}'", capability.name),
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        for native_module in &profile.native_module_dependencies {
            if !modules.contains_key(native_module) {
                diagnostics.push(ModuleDiagnostic::new(
                    ModuleDiagnosticCode::UnknownModule,
                    native_module,
                    "nativeModuleDependency",
                    "native dependency references an unregistered host module",
                ));
            } else if !included.contains(native_module) {
                diagnostics.push(ModuleDiagnostic::new(
                    ModuleDiagnosticCode::NativeModuleDependencyStripped,
                    native_module,
                    "nativeModuleDependency",
                    "module is referenced by a native dependency but is not in the profile dependency closure",
                ));
            }
        }

        let mut native_dependencies = BTreeSet::new();
        for specifier in &included {
            if let Some(module) = modules.get(specifier) {
                for dependency in &module.native_dependencies {
                    if !profile.enabled_native_dependencies.contains(dependency) {
                        diagnostics.push(ModuleDiagnostic::new(
                            ModuleDiagnosticCode::MissingNativeDependency,
                            specifier,
                            dependency,
                            "required native dependency is not enabled in this profile",
                        ));
                    } else {
                        native_dependencies.insert(dependency.clone());
                    }
                }
            }
        }

        let stripped_modules = modules
            .keys()
            .filter(|specifier| !included.contains(*specifier))
            .cloned()
            .collect::<BTreeSet<_>>();

        if !diagnostics.is_empty() {
            sort_diagnostics(&mut diagnostics);
            return Err(diagnostics);
        }

        Ok(ModuleBuildPlan {
            profile_id: profile.id.clone(),
            included_modules: included,
            stripped_modules,
            native_dependencies,
            capability_mask: granted_mask,
            enabled_features: profile.enabled_features.clone(),
            granted_capabilities: grant_map,
        })
    }

    /// Only immutable named ESM bindings may bypass the generic module/property path.
    pub fn resolve_import(
        &self,
        plan: &ModuleBuildPlan,
        specifier: &str,
        export_name: Option<&str>,
        kind: ModuleImportKind,
    ) -> Result<ModuleImportResolution, ModuleDiagnostic> {
        let fallback = match kind {
            ModuleImportKind::StaticNamedEsm => None,
            ModuleImportKind::NamespaceEsm => Some(GenericFallbackReason::NamespaceBinding),
            ModuleImportKind::DynamicImport => Some(GenericFallbackReason::DynamicImport),
            ModuleImportKind::CommonJsRequire => Some(GenericFallbackReason::CommonJsInterop),
            ModuleImportKind::DynamicPropertyAccess => {
                Some(GenericFallbackReason::DynamicPropertyAccess)
            }
        };
        if let Some(reason) = fallback {
            return Ok(ModuleImportResolution::GenericFallback(reason));
        }
        if !plan.included_modules.contains(specifier) {
            return Err(ModuleDiagnostic::new(
                ModuleDiagnosticCode::ModuleNotEnabled,
                specifier,
                export_name.unwrap_or_default(),
                "static import target is absent from the validated build plan",
            ));
        }
        let modules = self.modules.borrow();
        let module = modules.get(specifier).ok_or_else(|| {
            ModuleDiagnostic::new(
                ModuleDiagnosticCode::UnknownModule,
                specifier,
                "",
                "module is not registered",
            )
        })?;

        for feature in &module.required_features {
            if !plan.enabled_features.contains(feature) {
                return Err(ModuleDiagnostic::new(
                    ModuleDiagnosticCode::MissingFeature,
                    specifier,
                    feature,
                    format!("module requires feature '{feature}'"),
                ));
            }
        }

        let mut missing_module_capabilities = module
            .required_capabilities
            .iter()
            .filter(|required| {
                plan_capability_bit(plan, &required.name) != Some(required.bit)
            })
            .map(|required| required.name.clone())
            .collect::<Vec<_>>();
        missing_module_capabilities.sort();
        missing_module_capabilities.dedup();
        if !missing_module_capabilities.is_empty() {
            return Err(ModuleDiagnostic::new(
                ModuleDiagnosticCode::CapabilityDenied,
                specifier,
                "",
                format!(
                    "missing module capabilities: {}",
                    missing_module_capabilities.join(", ")
                ),
            ));
        }

        let name = export_name.ok_or_else(|| {
            ModuleDiagnostic::new(
                ModuleDiagnosticCode::InvalidDescriptor,
                specifier,
                "",
                "static named import requires an export name",
            )
        })?;
        let export = module.exports.get(name).ok_or_else(|| {
            ModuleDiagnostic::new(
                ModuleDiagnosticCode::UnknownExport,
                specifier,
                name,
                "static import references an unknown export",
            )
        })?;
        for feature in &export.required_features {
            if !plan.enabled_features.contains(feature) {
                return Err(ModuleDiagnostic::new(
                    ModuleDiagnosticCode::MissingFeature,
                    specifier,
                    name,
                    format!("export requires feature '{feature}'"),
                ));
            }
        }
        let mut missing = export
            .required_capabilities
            .iter()
            .filter(|required| plan_capability_bit(plan, &required.name) != Some(required.bit))
            .map(|required| required.name.clone())
            .collect::<Vec<_>>();
        missing.sort();
        missing.dedup();
        if !missing.is_empty() {
            return Err(ModuleDiagnostic::new(
                ModuleDiagnosticCode::CapabilityDenied,
                specifier,
                name,
                format!("missing capabilities: {}", missing.join(", ")),
            ));
        }
        match export.binding {
            HostExportBinding::ImmutableHostFunction => {
                Ok(ModuleImportResolution::StaticFunctionSlot {
                    slot: export.slot,
                    signature: export.signature,
                })
            }
            HostExportBinding::Mutable => Ok(ModuleImportResolution::GenericFallback(
                GenericFallbackReason::MutableExport,
            )),
            HostExportBinding::Dynamic => Ok(ModuleImportResolution::GenericFallback(
                GenericFallbackReason::DynamicExport,
            )),
        }
    }

    /// Generates profile-specific ambient declarations from the same registered exports
    /// that populate the function table. Capability requirements remain explicit in docs.
    pub fn generate_typescript_declarations(
        &self,
        plan: &ModuleBuildPlan,
        specifier: &str,
    ) -> Result<String, ModuleDiagnostic> {
        if !plan.included_modules.contains(specifier) {
            return Err(ModuleDiagnostic::new(
                ModuleDiagnosticCode::ModuleNotEnabled,
                specifier,
                "",
                "cannot generate declarations for a module excluded from the build profile",
            ));
        }
        let modules = self.modules.borrow();
        let module = modules.get(specifier).ok_or_else(|| {
            ModuleDiagnostic::new(
                ModuleDiagnosticCode::UnknownModule,
                specifier,
                "",
                "module is not registered",
            )
        })?;
        let mut declaration = format!(
            "// Generated by Ogma HostModuleRegistry. Module version: {}.\ndeclare module \"{}\" {{\n",
            module.version, specifier
        );
        for (name, export) in &module.exports {
            let required_features = module
                .required_features
                .union(&export.required_features)
                .cloned()
                .collect::<BTreeSet<_>>();
            if required_features
                .iter()
                .any(|feature| !plan.enabled_features.contains(feature))
            {
                continue;
            }

            let required_capabilities = module
                .required_capabilities
                .iter()
                .chain(export.required_capabilities.iter())
                .cloned()
                .collect::<BTreeSet<_>>();
            if required_capabilities
                .iter()
                .any(|required| plan_capability_bit(plan, &required.name) != Some(required.bit))
            {
                continue;
            }

            let capability_names = required_capabilities
                .iter()
                .map(|capability| capability.name.as_str())
                .collect::<Vec<_>>();
            if !capability_names.is_empty() {
                declaration.push_str(&format!(
                    "  /** Requires capabilities: {}. */\n",
                    capability_names.join(", ")
                ));
            }
            let feature_names = required_features.into_iter().collect::<Vec<_>>();
            if !feature_names.is_empty() {
                declaration.push_str(&format!(
                    "  /** Requires features: {}. */\n",
                    feature_names.join(", ")
                ));
            }
            let function_type = match export.signature {
                HostFunctionSignature::Generic => "(...args: unknown[]) => unknown",
                HostFunctionSignature::F64Binary | HostFunctionSignature::NumberHandleBinary => {
                    "(left: number, right: number) => number"
                }
            };
            declaration.push_str(&format!("  export const {name}: {function_type};\n"));
        }
        declaration.push_str("}\n");
        Ok(declaration)
    }
}

fn visit_module(
    specifier: &str,
    modules: &BTreeMap<String, RegisteredModule>,
    profile: &HostFeatureProfile,
    grants: &BTreeMap<String, u8>,
    visiting: &mut BTreeSet<String>,
    included: &mut BTreeSet<String>,
    diagnostics: &mut Vec<ModuleDiagnostic>,
) {
    if included.contains(specifier) {
        return;
    }
    if !visiting.insert(specifier.to_owned()) {
        diagnostics.push(ModuleDiagnostic::new(
            ModuleDiagnosticCode::DependencyCycle,
            specifier,
            "dependencies",
            "cycle detected in host module dependency graph",
        ));
        return;
    }
    let Some(module) = modules.get(specifier) else {
        visiting.remove(specifier);
        diagnostics.push(ModuleDiagnostic::new(
            ModuleDiagnosticCode::MissingDependency,
            specifier,
            "",
            "required module dependency is not registered",
        ));
        return;
    };

    for feature in &module.required_features {
        if !profile.enabled_features.contains(feature) {
            diagnostics.push(ModuleDiagnostic::new(
                ModuleDiagnosticCode::MissingFeature,
                specifier,
                feature,
                format!("module requires feature '{feature}'"),
            ));
        }
    }
    for required in &module.required_capabilities {
        if grants.get(&required.name) != Some(&required.bit) {
            diagnostics.push(ModuleDiagnostic::new(
                ModuleDiagnosticCode::CapabilityDenied,
                specifier,
                &required.name,
                format!(
                    "module requires capability '{}' at bit {}",
                    required.name, required.bit
                ),
            ));
        }
    }
    for dependency in &module.dependencies {
        if !modules.contains_key(dependency) {
            diagnostics.push(ModuleDiagnostic::new(
                ModuleDiagnosticCode::MissingDependency,
                specifier,
                dependency,
                "declared module dependency is not registered",
            ));
        } else {
            visit_module(
                dependency,
                modules,
                profile,
                grants,
                visiting,
                included,
                diagnostics,
            );
        }
    }
    visiting.remove(specifier);
    included.insert(specifier.to_owned());
}

fn plan_capability_bit(plan: &ModuleBuildPlan, name: &str) -> Option<u8> {
    plan.granted_capabilities.get(name).copied()
}

fn capabilities_mask(capabilities: &[HostCapabilityRequirement]) -> Result<u64, String> {
    let mut names = BTreeMap::<String, u8>::new();
    let mut bits = BTreeMap::<u8, String>::new();
    let mut mask = 0_u64;
    for capability in capabilities {
        if !valid_capability_name(&capability.name) || capability.bit >= 64 {
            return Err(format!(
                "invalid capability '{}' at bit {}",
                capability.name, capability.bit
            ));
        }
        if let Some(previous) = names.get(&capability.name) {
            if previous != &capability.bit {
                return Err(format!(
                    "capability '{}' is assigned to multiple bits",
                    capability.name
                ));
            }
        }
        if let Some(previous) = bits.get(&capability.bit) {
            if previous != &capability.name {
                return Err(format!(
                    "capability bit {} is assigned to both '{}' and '{}'",
                    capability.bit, previous, capability.name
                ));
            }
        }
        names.insert(capability.name.clone(), capability.bit);
        bits.insert(capability.bit, capability.name.clone());
        mask |= 1_u64 << capability.bit;
    }
    Ok(mask)
}

fn validate_feature_names(features: &[String]) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for feature in features {
        if !valid_feature_name(feature) {
            return Err(format!("invalid feature name '{feature}'"));
        }
        if !seen.insert(feature) {
            return Err(format!("duplicate feature name '{feature}'"));
        }
    }
    Ok(())
}

fn validate_unique_strings(values: &[String], valid: impl Fn(&str) -> bool) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for value in values {
        if !valid(value) {
            return Err(format!("invalid value '{value}'"));
        }
        if !seen.insert(value) {
            return Err(format!("duplicate value '{value}'"));
        }
    }
    Ok(())
}

fn valid_module_specifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && value
            .as_bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'/' | b'-' | b'_' | b'.' | b'@')
        })
        && !value.contains("//")
        && !value.contains("..")
}

fn valid_native_dependency(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_capability_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b':' | b'-' | b'_')
        })
}

fn valid_feature_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b':' | b'-' | b'_')
        })
}

fn is_semver(value: &str) -> bool {
    let parts = value.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part == &"0" || !part.starts_with('0'))
        })
}

fn is_typescript_identifier(value: &str) -> bool {
    const RESERVED: &[&str] = &[
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "function",
        "if",
        "import",
        "in",
        "instanceof",
        "new",
        "null",
        "return",
        "super",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "typeof",
        "var",
        "void",
        "while",
        "with",
        "yield",
        "let",
        "implements",
        "interface",
        "package",
        "private",
        "protected",
        "public",
        "static",
        "type",
        "namespace",
        "module",
    ];
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_' || first == '$')
        && chars.all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '$'
        })
        && !RESERVED.contains(&value)
}

fn sort_diagnostics(diagnostics: &mut Vec<ModuleDiagnostic>) {
    diagnostics.sort();
    diagnostics.dedup();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DispatchValue, HostFunctionId, JsCallable, OrdinaryHostObject, PropertyReceiver,
        EFFECT_MAY_ALLOCATE, EFFECT_MAY_GC, EFFECT_MAY_REENTER, EFFECT_MAY_THROW,
    };
    use ogma_host_execution_scope_proof::{HostExecutionContext, LibrarySyncMode};
    use ogma_rust_abi_proof::{
        OgmaCompletion, OgmaContextHandle, OgmaHandle, OgmaRuntime, OgmaStatus, COMPLETION_NORMAL,
    };
    use std::ffi::c_void;
    use std::ptr;

    unsafe extern "C-unwind" fn generic_ok(
        _runtime: *const OgmaRuntime,
        _context: OgmaContextHandle,
        _args: *const OgmaHandle,
        _args_len: usize,
        completion: *mut OgmaCompletion,
        _user_data: *mut c_void,
    ) -> OgmaStatus {
        if completion.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        unsafe {
            completion.write(OgmaCompletion {
                kind: COMPLETION_NORMAL,
                reserved: 0,
                value: OgmaHandle::default(),
            });
        }
        OgmaStatus::OK
    }

    unsafe extern "C-unwind" fn typed_add(
        _runtime: *const OgmaRuntime,
        _context: OgmaContextHandle,
        left: f64,
        right: f64,
        output: *mut f64,
        _user_data: *mut c_void,
    ) -> OgmaStatus {
        if output.is_null() {
            return OgmaStatus::INVALID_ARGUMENT;
        }
        unsafe {
            output.write(left + right);
        }
        OgmaStatus::OK
    }

    fn math_export(id: u64, name: &str, binding: HostExportBinding) -> HostModuleExport {
        HostModuleExport {
            name: name.to_owned(),
            function: HostFunctionDescriptor::f64_binary(
                HostFunctionId(id),
                &format!("ogma:math/{name}"),
                0,
                0,
                typed_add,
                generic_ok,
                ptr::null_mut(),
            )
            .unwrap(),
            required_capabilities: Vec::new(),
            required_features: Vec::new(),
            binding,
        }
    }

    fn math_module() -> HostModuleDescriptor {
        let abi = ogma_rust_abi_proof::ogma_abi_version();
        HostModuleDescriptor {
            descriptor_version: HOST_MODULE_DESCRIPTOR_VERSION,
            abi_major: abi.major,
            abi_minor: abi.minor,
            specifier: "ogma:math".to_owned(),
            version: "1.0.0".to_owned(),
            required_capabilities: Vec::new(),
            required_features: vec!["host-modules.v1".to_owned()],
            dependencies: Vec::new(),
            native_dependencies: Vec::new(),
            exports: vec![
                math_export(50_001, "add", HostExportBinding::ImmutableHostFunction),
                math_export(50_002, "replaceable", HostExportBinding::Mutable),
                math_export(50_003, "dynamic", HostExportBinding::Dynamic),
            ],
        }
    }

    fn profile(modules: &[&str]) -> HostFeatureProfile {
        HostFeatureProfile {
            id: "test".to_owned(),
            enabled_modules: modules.iter().map(|value| (*value).to_owned()).collect(),
            enabled_features: ["host-modules.v1".to_owned()].into_iter().collect(),
            granted_capabilities: Vec::new(),
            enabled_native_dependencies: BTreeSet::new(),
            runtime_imports: Vec::new(),
            native_module_dependencies: BTreeSet::new(),
        }
    }

    fn host_scope() -> HostExecutionContext {
        HostExecutionContext::new(LibrarySyncMode::None)
    }

    #[test]
    fn descriptor_abi_and_exports_are_validated_before_registration() {
        let functions = HostFunctionRegistry::new().unwrap();
        let modules = HostModuleRegistry::new(&functions);
        let mut descriptor = math_module();
        descriptor.abi_major = descriptor.abi_major.saturating_add(1);
        assert_eq!(
            modules.register(descriptor).unwrap_err().code,
            ModuleDiagnosticCode::AbiMismatch
        );
        assert_eq!(functions.metrics().active_slots, 0);

        modules.register(math_module()).unwrap();
        assert_eq!(
            modules.module_version("ogma:math").as_deref(),
            Some("1.0.0")
        );
        assert_eq!(functions.metrics().active_slots, 3);
    }

    #[test]
    fn declarations_are_generated_from_registered_exports_and_match_fixture() {
        let functions = HostFunctionRegistry::new().unwrap();
        let modules = HostModuleRegistry::new(&functions);
        let mut descriptor = math_module();
        descriptor
            .required_capabilities
            .push(HostCapabilityRequirement::new("math.read", 2));
        descriptor.exports = vec![
            math_export(51_001, "add", HostExportBinding::ImmutableHostFunction),
            HostModuleExport {
                name: "log".to_owned(),
                function: HostFunctionDescriptor::generic(
                    HostFunctionId(51_002),
                    "ogma:math/log",
                    0,
                    EFFECT_MAY_THROW | EFFECT_MAY_ALLOCATE | EFFECT_MAY_GC | EFFECT_MAY_REENTER,
                    generic_ok,
                    ptr::null_mut(),
                )
                .unwrap(),
                required_capabilities: Vec::new(),
                required_features: Vec::new(),
                binding: HostExportBinding::ImmutableHostFunction,
            },
        ];
        modules.register(descriptor).unwrap();
        let mut declaration_profile = profile(&["ogma:math"]);
        declaration_profile
            .granted_capabilities
            .push(HostCapabilityRequirement::new("math.read", 2));
        let plan = modules.build_profile(&declaration_profile).unwrap();
        let generated = modules
            .generate_typescript_declarations(&plan, "ogma:math")
            .unwrap();
        assert_eq!(generated, include_str!("../tests/fixtures/ogma-math.d.ts"));
    }

    #[test]
    fn partial_module_registration_rolls_back_all_previously_registered_exports() {
        let functions = HostFunctionRegistry::new().unwrap();
        let modules = HostModuleRegistry::new(&functions);
        let mut descriptor = math_module();
        descriptor.exports.push(math_export(
            50_001,
            "duplicate_id",
            HostExportBinding::ImmutableHostFunction,
        ));

        let error = modules.register(descriptor).unwrap_err();
        assert_eq!(error.code, ModuleDiagnosticCode::FunctionRegistrationFailed);
        assert_eq!(functions.metrics().active_slots, 0);
        assert_eq!(modules.module_version("ogma:math"), None);
    }

    #[test]
    fn unregister_removes_module_metadata_and_all_function_slots() {
        let functions = HostFunctionRegistry::new().unwrap();
        let modules = HostModuleRegistry::new(&functions);
        modules.register(math_module()).unwrap();
        assert_eq!(functions.metrics().active_slots, 3);
        assert_eq!(
            modules.module_version("ogma:math").as_deref(),
            Some("1.0.0")
        );

        modules.unregister("ogma:math").unwrap();

        assert_eq!(functions.metrics().active_slots, 0);
        assert_eq!(modules.module_version("ogma:math"), None);
    }

    #[test]
    fn only_immutable_static_named_imports_resolve_to_slots() {
        let functions = HostFunctionRegistry::new().unwrap();
        let modules = HostModuleRegistry::new(&functions);
        modules.register(math_module()).unwrap();
        let plan = modules.build_profile(&profile(&["ogma:math"])).unwrap();

        let static_add = modules
            .resolve_import(
                &plan,
                "ogma:math",
                Some("add"),
                ModuleImportKind::StaticNamedEsm,
            )
            .unwrap();
        let ModuleImportResolution::StaticFunctionSlot { slot, signature } = static_add else {
            panic!("immutable static import should resolve to a typed slot");
        };
        assert_eq!(signature, HostFunctionSignature::F64Binary);

        assert_eq!(
            modules
                .resolve_import(
                    &plan,
                    "ogma:math",
                    Some("replaceable"),
                    ModuleImportKind::StaticNamedEsm
                )
                .unwrap(),
            ModuleImportResolution::GenericFallback(GenericFallbackReason::MutableExport)
        );
        assert_eq!(
            modules
                .resolve_import(
                    &plan,
                    "ogma:math",
                    Some("dynamic"),
                    ModuleImportKind::StaticNamedEsm
                )
                .unwrap(),
            ModuleImportResolution::GenericFallback(GenericFallbackReason::DynamicExport)
        );
        assert_eq!(
            modules
                .resolve_import(&plan, "ogma:math", None, ModuleImportKind::DynamicImport)
                .unwrap(),
            ModuleImportResolution::GenericFallback(GenericFallbackReason::DynamicImport)
        );
        assert_eq!(
            modules
                .resolve_import(&plan, "ogma:math", None, ModuleImportKind::NamespaceEsm)
                .unwrap(),
            ModuleImportResolution::GenericFallback(GenericFallbackReason::NamespaceBinding)
        );
        assert_eq!(
            modules
                .resolve_import(&plan, "ogma:math", None, ModuleImportKind::CommonJsRequire)
                .unwrap(),
            ModuleImportResolution::GenericFallback(GenericFallbackReason::CommonJsInterop)
        );
        assert_eq!(
            modules
                .resolve_import(
                    &plan,
                    "ogma:math",
                    Some("add"),
                    ModuleImportKind::DynamicPropertyAccess
                )
                .unwrap(),
            ModuleImportResolution::GenericFallback(GenericFallbackReason::DynamicPropertyAccess)
        );

        let host = host_scope();
        let scope = host.enter_scope().unwrap();
        assert_eq!(
            functions
                .invoke_slot(
                    &scope,
                    slot,
                    plan.capability_mask,
                    &[DispatchValue::Number(4.0), DispatchValue::Number(7.0)]
                )
                .unwrap(),
            crate::JsCompletion::Normal(DispatchValue::Number(11.0))
        );

        // Existing property semantics remain dynamic: replacing a function value
        // invalidates the cache and invokes the replacement, not the old host slot.
        let mut object = OrdinaryHostObject::new();
        object.define("run", JsCallable::host(slot));
        let guard = functions.resolve_call_site(&object, "run").unwrap();
        object.replace("run", JsCallable::script(DispatchValue::Number(99.0)));
        assert_eq!(
            functions
                .call_property(
                    &scope,
                    PropertyReceiver::Ordinary(&object),
                    "run",
                    Some(&guard),
                    plan.capability_mask,
                    &[DispatchValue::Number(1.0), DispatchValue::Number(2.0)]
                )
                .unwrap(),
            crate::JsCompletion::Normal(DispatchValue::Number(99.0))
        );
    }

    #[test]
    fn capability_and_feature_denials_have_stable_diagnostics() {
        let functions = HostFunctionRegistry::new().unwrap();
        let modules = HostModuleRegistry::new(&functions);
        let mut descriptor = math_module();
        descriptor.required_features = vec!["host-modules.v1".to_owned(), "graphics.v2".to_owned()];
        descriptor.required_capabilities = vec![HostCapabilityRequirement::new("render.read", 3)];
        descriptor.exports[0]
            .required_capabilities
            .push(HostCapabilityRequirement::new("render.write", 4));
        modules.register(descriptor).unwrap();

        let no_feature = profile(&["ogma:math"]);
        let first = modules.build_profile(&no_feature).unwrap_err();
        let second = modules.build_profile(&no_feature).unwrap_err();
        assert_eq!(first, second);
        assert!(first
            .iter()
            .any(|item| item.code == ModuleDiagnosticCode::MissingFeature
                && item.subject == "graphics.v2"));
        assert!(first
            .iter()
            .any(|item| item.code == ModuleDiagnosticCode::CapabilityDenied
                && item.subject == "render.read"));
        assert!(first.windows(2).all(|pair| pair[0] <= pair[1]));

        let mut granted = profile(&["ogma:math"]);
        granted.enabled_features.insert("graphics.v2".to_owned());
        granted
            .granted_capabilities
            .push(HostCapabilityRequirement::new("render.read", 3));
        let plan = modules.build_profile(&granted).unwrap();

        let mut tampered_plan = plan.clone();
        tampered_plan.enabled_features.remove("graphics.v2");
        let feature_denial = modules
            .resolve_import(
                &tampered_plan,
                "ogma:math",
                Some("add"),
                ModuleImportKind::StaticNamedEsm,
            )
            .unwrap_err();
        assert_eq!(feature_denial.code, ModuleDiagnosticCode::MissingFeature);
        assert_eq!(feature_denial.module, "ogma:math");

        tampered_plan.enabled_features.insert("graphics.v2".to_owned());
        tampered_plan.granted_capabilities.remove("render.read");
        let module_capability_denial = modules
            .resolve_import(
                &tampered_plan,
                "ogma:math",
                Some("add"),
                ModuleImportKind::StaticNamedEsm,
            )
            .unwrap_err();
        assert_eq!(
            module_capability_denial.code,
            ModuleDiagnosticCode::CapabilityDenied
        );
        assert!(module_capability_denial
            .detail
            .contains("render.read"));

        let denied = modules
            .resolve_import(
                &plan,
                "ogma:math",
                Some("add"),
                ModuleImportKind::StaticNamedEsm,
            )
            .unwrap_err();
        assert_eq!(denied.code, ModuleDiagnosticCode::CapabilityDenied);
        assert_eq!(denied.subject, "add");
        assert!(denied.detail.contains("render.write"));

        granted
            .granted_capabilities
            .push(HostCapabilityRequirement::new("render.write", 4));
        let fully_granted_plan = modules.build_profile(&granted).unwrap();
        assert!(matches!(
            modules
                .resolve_import(
                    &fully_granted_plan,
                    "ogma:math",
                    Some("add"),
                    ModuleImportKind::StaticNamedEsm
                )
                .unwrap(),
            ModuleImportResolution::StaticFunctionSlot { .. }
        ));
    }

    #[test]
    fn stripped_runtime_and_native_module_references_fail_closed() {
        let functions = HostFunctionRegistry::new().unwrap();
        let modules = HostModuleRegistry::new(&functions);
        modules.register(math_module()).unwrap();
        let mut selected = profile(&[]);
        selected.runtime_imports.push(RuntimeModuleImport {
            specifier: "ogma:math".to_owned(),
            kind: ModuleImportKind::StaticNamedEsm,
            imported_names: vec!["add".to_owned()],
        });
        selected
            .native_module_dependencies
            .insert("ogma:math".to_owned());

        let first = modules.build_profile(&selected).unwrap_err();
        let second = modules.build_profile(&selected).unwrap_err();
        assert_eq!(first, second);
        assert!(first
            .iter()
            .any(|item| item.code == ModuleDiagnosticCode::RuntimeImportModuleStripped));
        assert!(first
            .iter()
            .any(|item| item.code == ModuleDiagnosticCode::NativeModuleDependencyStripped));
    }

    #[test]
    fn declared_dependencies_are_included_transitively_and_missing_native_deps_fail() {
        let functions = HostFunctionRegistry::new().unwrap();
        let modules = HostModuleRegistry::new(&functions);
        modules.register(math_module()).unwrap();
        let mut physics = math_module();
        physics.specifier = "ogma:physics".to_owned();
        physics.exports = vec![math_export(
            52_001,
            "step",
            HostExportBinding::ImmutableHostFunction,
        )];
        physics.dependencies = vec!["ogma:math".to_owned()];
        physics.native_dependencies = vec!["physics-kernel".to_owned()];
        physics.exports[0].function = HostFunctionDescriptor::f64_binary(
            HostFunctionId(52_001),
            "ogma:physics/step",
            0,
            0,
            typed_add,
            generic_ok,
            ptr::null_mut(),
        )
        .unwrap();
        modules.register(physics).unwrap();

        let plan = modules
            .build_profile(&profile(&["ogma:physics"]))
            .unwrap_err();
        assert!(plan
            .iter()
            .any(|item| item.code == ModuleDiagnosticCode::MissingNativeDependency));

        let mut enabled = profile(&["ogma:physics"]);
        enabled
            .enabled_native_dependencies
            .insert("physics-kernel".to_owned());
        let plan = modules.build_profile(&enabled).unwrap();
        assert_eq!(
            plan.included_modules,
            ["ogma:math".to_owned(), "ogma:physics".to_owned()]
                .into_iter()
                .collect()
        );
        assert!(plan.stripped_modules.is_empty());
    }

    #[test]
    fn dependency_cycles_produce_deterministic_diagnostics() {
        let functions = HostFunctionRegistry::new().unwrap();
        let modules = HostModuleRegistry::new(&functions);
        let mut alpha = math_module();
        alpha.specifier = "ogma:alpha".to_owned();
        alpha.dependencies = vec!["ogma:beta".to_owned()];
        alpha.exports = vec![HostModuleExport {
            name: "run".to_owned(),
            function: HostFunctionDescriptor::f64_binary(
                HostFunctionId(54_001),
                "ogma:alpha/run",
                0,
                0,
                typed_add,
                generic_ok,
                ptr::null_mut(),
            )
            .unwrap(),
            required_capabilities: Vec::new(),
            required_features: Vec::new(),
            binding: HostExportBinding::ImmutableHostFunction,
        }];
        let mut beta = math_module();
        beta.specifier = "ogma:beta".to_owned();
        beta.dependencies = vec!["ogma:alpha".to_owned()];
        beta.exports = vec![HostModuleExport {
            name: "run".to_owned(),
            function: HostFunctionDescriptor::f64_binary(
                HostFunctionId(54_002),
                "ogma:beta/run",
                0,
                0,
                typed_add,
                generic_ok,
                ptr::null_mut(),
            )
            .unwrap(),
            required_capabilities: Vec::new(),
            required_features: Vec::new(),
            binding: HostExportBinding::ImmutableHostFunction,
        }];
        modules.register(alpha).unwrap();
        modules.register(beta).unwrap();

        let first = modules
            .build_profile(&profile(&["ogma:alpha"]))
            .unwrap_err();
        let second = modules
            .build_profile(&profile(&["ogma:alpha"]))
            .unwrap_err();
        assert_eq!(first, second);
        assert!(first
            .iter()
            .any(|item| item.code == ModuleDiagnosticCode::DependencyCycle));
    }

    #[test]
    fn missing_module_dependencies_are_reported_without_silent_stripping() {
        let functions = HostFunctionRegistry::new().unwrap();
        let modules = HostModuleRegistry::new(&functions);
        let mut physics = math_module();
        physics.specifier = "ogma:physics".to_owned();
        physics.exports = vec![math_export(
            53_001,
            "step",
            HostExportBinding::ImmutableHostFunction,
        )];
        physics.dependencies = vec!["ogma:missing".to_owned()];
        physics.exports[0].function = HostFunctionDescriptor::f64_binary(
            HostFunctionId(53_001),
            "ogma:physics/step",
            0,
            0,
            typed_add,
            generic_ok,
            ptr::null_mut(),
        )
        .unwrap();
        modules.register(physics).unwrap();
        let errors = modules
            .build_profile(&profile(&["ogma:physics"]))
            .unwrap_err();
        assert!(errors
            .iter()
            .any(|item| item.code == ModuleDiagnosticCode::MissingDependency
                && item.subject == "ogma:missing"));
    }
}
