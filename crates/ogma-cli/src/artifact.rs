//! Canonical OGMB validation and CLI compilation-unit metadata.

use ogma_bytecode::{VerifiedArtifact, sha256_hex, verify_register_artifact_from_manifest};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_UNIT_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;
const MAX_UNIT_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;
const UNIT_SCHEMA: &str = "ogma.cli-unit";
const UNIT_VERSION: &str = "1.0";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceInfo {
    pub(crate) path: String,
    pub(crate) id: String,
    pub(crate) sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArtifactEntry {
    pub(crate) id: String,
    pub(crate) name: String,
    /// None denotes the artifact at the command output path.
    pub(crate) file: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UnitManifest {
    pub(crate) schema: String,
    pub(crate) version: String,
    pub(crate) source: SourceInfo,
    pub(crate) entry_function: String,
    pub(crate) entry_sha256: String,
    pub(crate) module: Option<ModuleDescriptorData>,
    pub(crate) functions: Vec<ArtifactEntry>,
    pub(crate) dependencies: Vec<DependencyModuleData>,
    pub(crate) resolved_requests: Vec<ResolvedModuleRequest>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResolvedModuleRequest {
    pub(crate) referrer: String,
    pub(crate) specifier: String,
    pub(crate) target: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DependencyModuleData {
    pub(crate) source: SourceInfo,
    pub(crate) descriptor: ModuleDescriptorData,
    pub(crate) functions: Vec<ArtifactEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModuleDescriptorData {
    pub(crate) id: String,
    pub(crate) entry_function: String,
    pub(crate) environment: String,
    pub(crate) imports: Vec<ModuleImportData>,
    pub(crate) exports: Vec<ModuleExportData>,
    pub(crate) requests: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModuleImportData {
    pub(crate) specifier: String,
    pub(crate) imported_name: Option<String>,
    pub(crate) local_binding: Option<String>,
    pub(crate) kind: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModuleExportData {
    pub(crate) export_name: String,
    pub(crate) local_binding: Option<String>,
    pub(crate) source: Option<String>,
    pub(crate) imported_name: Option<String>,
    pub(crate) kind: String,
}

impl From<&ogma_bytecode::ModuleDescriptor> for ModuleDescriptorData {
    fn from(value: &ogma_bytecode::ModuleDescriptor) -> Self {
        Self {
            id: value.id.clone(),
            entry_function: value.entry_function.clone(),
            environment: value.environment.clone(),
            imports: value
                .imports
                .iter()
                .map(|item| ModuleImportData {
                    specifier: item.specifier.clone(),
                    imported_name: item.imported_name.clone(),
                    local_binding: item.local_binding.clone(),
                    kind: match item.kind {
                        ogma_bytecode::ModuleImportKind::SideEffect => "side_effect",
                        ogma_bytecode::ModuleImportKind::Default => "default",
                        ogma_bytecode::ModuleImportKind::Named => "named",
                        ogma_bytecode::ModuleImportKind::Namespace => "namespace",
                    }
                    .to_owned(),
                })
                .collect(),
            exports: value
                .exports
                .iter()
                .map(|item| ModuleExportData {
                    export_name: item.export_name.clone(),
                    local_binding: item.local_binding.clone(),
                    source: item.source.clone(),
                    imported_name: item.imported_name.clone(),
                    kind: match item.kind {
                        ogma_bytecode::ModuleExportKind::Local => "local",
                        ogma_bytecode::ModuleExportKind::ReExport => "re_export",
                        ogma_bytecode::ModuleExportKind::Star => "star",
                    }
                    .to_owned(),
                })
                .collect(),
            requests: value.requests.clone(),
        }
    }
}

impl TryFrom<ModuleDescriptorData> for ogma_bytecode::ModuleDescriptor {
    type Error = String;

    fn try_from(value: ModuleDescriptorData) -> Result<Self, Self::Error> {
        let imports = value
            .imports
            .into_iter()
            .map(|item| {
                let kind = match item.kind.as_str() {
                    "side_effect" => ogma_bytecode::ModuleImportKind::SideEffect,
                    "default" => ogma_bytecode::ModuleImportKind::Default,
                    "named" => ogma_bytecode::ModuleImportKind::Named,
                    "namespace" => ogma_bytecode::ModuleImportKind::Namespace,
                    other => return Err(format!("unsupported module import kind {other:?}")),
                };
                Ok(ogma_bytecode::ModuleImport {
                    specifier: item.specifier,
                    imported_name: item.imported_name,
                    local_binding: item.local_binding,
                    kind,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let exports = value
            .exports
            .into_iter()
            .map(|item| {
                let kind = match item.kind.as_str() {
                    "local" => ogma_bytecode::ModuleExportKind::Local,
                    "re_export" => ogma_bytecode::ModuleExportKind::ReExport,
                    "star" => ogma_bytecode::ModuleExportKind::Star,
                    other => return Err(format!("unsupported module export kind {other:?}")),
                };
                Ok(ogma_bytecode::ModuleExport {
                    export_name: item.export_name,
                    local_binding: item.local_binding,
                    source: item.source,
                    imported_name: item.imported_name,
                    kind,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Self {
            id: value.id,
            entry_function: value.entry_function,
            environment: value.environment,
            imports,
            exports,
            requests: value.requests,
        })
    }
}

#[derive(Debug)]
pub(crate) struct LoadedUnit {
    pub(crate) manifest: UnitManifest,
    pub(crate) artifacts: Vec<Vec<u8>>,
    pub(crate) dependencies: Vec<LoadedDependency>,
}

#[derive(Debug)]
pub(crate) struct LoadedDependency {
    pub(crate) descriptor: ModuleDescriptorData,
    pub(crate) artifacts: Vec<Vec<u8>>,
}

pub(crate) fn sidecar_path(artifact: &Path) -> PathBuf {
    let name = artifact.file_name().unwrap_or_default().to_string_lossy();
    artifact.with_file_name(format!("{name}.d"))
}

pub(crate) fn read_artifact(path: &Path) -> Result<Vec<u8>, String> {
    let metadata =
        fs::metadata(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if metadata.len() == 0 || metadata.len() > MAX_ARTIFACT_BYTES {
        return Err(format!(
            "invalid artifact size for {}: {} bytes; accepted size is 1..={} bytes",
            path.display(),
            metadata.len(),
            MAX_ARTIFACT_BYTES
        ));
    }
    fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))
}

pub(crate) fn verify_artifact(bytes: &[u8]) -> Result<VerifiedArtifact, String> {
    if bytes.is_empty() || bytes.len() as u64 > MAX_ARTIFACT_BYTES {
        return Err(format!(
            "artifact size must be within 1..={} bytes",
            MAX_ARTIFACT_BYTES
        ));
    }
    verify_register_artifact_from_manifest(bytes).map_err(|diagnostics| {
        diagnostics
            .into_iter()
            .map(|diagnostic| {
                format!(
                    "{} at {}: {}",
                    diagnostic.code, diagnostic.path, diagnostic.message
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    })
}

pub(crate) fn read_unit(
    artifact_path: &Path,
    root_bytes: &[u8],
) -> Result<Option<LoadedUnit>, String> {
    let directory = sidecar_path(artifact_path);
    let metadata = match fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot inspect {}: {error}", directory.display())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "compilation-unit sidecar {} must be a real directory",
            directory.display()
        ));
    }
    let manifest_path = directory.join("unit.json");
    let manifest_file_metadata = fs::symlink_metadata(&manifest_path)
        .map_err(|error| format!("cannot read {}: {error}", manifest_path.display()))?;
    if manifest_file_metadata.file_type().is_symlink() || !manifest_file_metadata.is_file() {
        return Err("compilation-unit manifest must be a regular file".to_owned());
    }
    let manifest_metadata = manifest_file_metadata;
    if manifest_metadata.len() == 0 || manifest_metadata.len() > MAX_UNIT_MANIFEST_BYTES {
        return Err(format!(
            "compilation-unit manifest has an invalid size: {} bytes",
            manifest_metadata.len()
        ));
    }
    let bytes = fs::read(&manifest_path)
        .map_err(|error| format!("cannot read {}: {error}", manifest_path.display()))?;
    let manifest: UnitManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid {}: {error}", manifest_path.display()))?;
    validate_unit_header(&manifest)?;

    if sha256_hex(root_bytes) != manifest.entry_sha256 {
        return Err(
            "the entry artifact does not match the compilation-unit manifest (SHA-256 mismatch)"
                .to_owned(),
        );
    }
    let root_entries = manifest
        .functions
        .iter()
        .filter(|item| item.file.is_none())
        .collect::<Vec<_>>();
    if root_entries.len() != 1 || root_entries[0].id != manifest.entry_function {
        return Err(
            "compilation-unit must list exactly one root artifact matching its entry function"
                .to_owned(),
        );
    }
    let entry = root_entries[0];
    if entry.id.is_empty()
        || entry.id.len() > 128
        || entry.name.is_empty()
        || entry.name.len() > 512
    {
        return Err("compilation-unit contains an invalid entry function identity".to_owned());
    }
    let root_verified = verify_artifact(root_bytes)?;
    check_function_identity(&root_verified, entry)?;

    let canonical_directory = fs::canonicalize(&directory).map_err(|error| {
        format!(
            "cannot resolve sidecar directory {}: {error}",
            directory.display()
        )
    })?;
    let mut ids = HashSet::new();
    ids.insert(entry.id.clone());
    let mut artifacts = Vec::with_capacity(manifest.functions.len());
    artifacts.push(root_bytes.to_vec());
    let mut total_bytes = root_bytes.len() as u64;

    for item in manifest.functions.iter().filter(|item| item.file.is_some()) {
        let helper_bytes = read_helper_artifact(
            &directory,
            &canonical_directory,
            item,
            &mut ids,
            &mut total_bytes,
        )?;
        artifacts.push(helper_bytes);
    }

    if let Some(module) = &manifest.module {
        if module.id != manifest.source.id || module.entry_function != manifest.entry_function {
            return Err(
                "module descriptor does not match its compilation-unit source or entry function"
                    .to_owned(),
            );
        }
        validate_module_descriptor(module)?;
    } else if !manifest.dependencies.is_empty() || !manifest.resolved_requests.is_empty() {
        return Err(
            "script compilation unit must not contain ESM dependency modules or resolver mappings"
                .to_owned(),
        );
    }

    validate_resolved_requests(&manifest)?;
    let mut dependency_ids = HashSet::new();
    let mut dependencies = Vec::with_capacity(manifest.dependencies.len());
    for dependency in &manifest.dependencies {
        if dependency.descriptor.id != dependency.source.id
            || dependency.descriptor.id == manifest.source.id
            || !dependency_ids.insert(dependency.descriptor.id.clone())
        {
            return Err(
                "compilation-unit contains an invalid or duplicate dependency module identity"
                    .to_owned(),
            );
        }
        if dependency.functions.is_empty()
            || dependency
                .functions
                .iter()
                .filter(|item| item.id == dependency.descriptor.entry_function)
                .count()
                != 1
        {
            return Err(format!(
                "dependency module {:?} does not list its entry function",
                dependency.descriptor.id
            ));
        }
        validate_module_descriptor(&dependency.descriptor)?;
        let mut dependency_artifacts = Vec::with_capacity(dependency.functions.len());
        for item in &dependency.functions {
            if item.file.is_none() {
                return Err("dependency module artifacts must all use sidecar paths".to_owned());
            }
            let helper_bytes = read_helper_artifact(
                &directory,
                &canonical_directory,
                item,
                &mut ids,
                &mut total_bytes,
            )?;
            dependency_artifacts.push(helper_bytes);
        }
        dependencies.push(LoadedDependency {
            descriptor: dependency.descriptor.clone(),
            artifacts: dependency_artifacts,
        });
    }
    Ok(Some(LoadedUnit {
        manifest,
        artifacts,
        dependencies,
    }))
}

fn read_helper_artifact(
    directory: &Path,
    canonical_directory: &Path,
    item: &ArtifactEntry,
    ids: &mut HashSet<String>,
    total_bytes: &mut u64,
) -> Result<Vec<u8>, String> {
    if item.id.is_empty() || item.id.len() > 128 || item.name.is_empty() || item.name.len() > 512 {
        return Err("compilation-unit contains an invalid function identity".to_owned());
    }
    if !ids.insert(item.id.clone()) {
        return Err(format!(
            "duplicate function identity {:?} in compilation-unit manifest",
            item.id
        ));
    }
    let Some(relative_name) = item.file.as_deref() else {
        return Err("helper artifact is missing its sidecar path".to_owned());
    };
    let relative = Path::new(relative_name);
    if !is_safe_relative_path(relative)
        || relative.extension().and_then(|value| value.to_str()) != Some("ogmb")
    {
        return Err(format!("unsafe helper artifact path {:?}", item.file));
    }
    let helper_path = directory.join(relative);
    let helper_metadata = fs::symlink_metadata(&helper_path).map_err(|error| {
        format!(
            "cannot read helper artifact {}: {error}",
            helper_path.display()
        )
    })?;
    if helper_metadata.file_type().is_symlink() || !helper_metadata.is_file() {
        return Err(format!(
            "helper artifact {} must be a regular file",
            helper_path.display()
        ));
    }
    let canonical_helper = fs::canonicalize(&helper_path).map_err(|error| {
        format!(
            "cannot resolve helper artifact {}: {error}",
            helper_path.display()
        )
    })?;
    if !canonical_helper.starts_with(canonical_directory) {
        return Err(format!(
            "helper artifact {} escapes the compilation-unit sidecar",
            helper_path.display()
        ));
    }
    if helper_metadata.len() > MAX_ARTIFACT_BYTES {
        return Err(
            "compilation-unit helper artifact exceeds the per-artifact size limit".to_owned(),
        );
    }
    let bytes = fs::read(&canonical_helper).map_err(|error| {
        format!(
            "cannot read helper artifact {}: {error}",
            helper_path.display()
        )
    })?;
    *total_bytes = total_bytes.saturating_add(bytes.len() as u64);
    if *total_bytes > MAX_UNIT_ARTIFACT_BYTES || bytes.len() as u64 > MAX_ARTIFACT_BYTES {
        return Err(
            "compilation-unit helper artifacts exceed the configured size limit".to_owned(),
        );
    }
    let verified = verify_artifact(&bytes)?;
    check_function_identity(&verified, item)?;
    Ok(bytes)
}

fn validate_module_descriptor(module: &ModuleDescriptorData) -> Result<(), String> {
    if module.id.is_empty()
        || module.id.len() > 4096
        || module.entry_function.is_empty()
        || module.entry_function.len() > 128
        || module.environment.is_empty()
        || module.environment.len() > 4096
        || module.imports.len() > 65_536
        || module.exports.len() > 65_536
        || module.requests.len() > 255
        || module
            .requests
            .iter()
            .any(|request| request.is_empty() || request.len() > 4096)
        || module.imports.iter().any(|item| {
            item.specifier.is_empty()
                || item.specifier.len() > 4096
                || !matches!(
                    item.kind.as_str(),
                    "side_effect" | "default" | "named" | "namespace"
                )
                || item
                    .imported_name
                    .as_ref()
                    .is_some_and(|name| name.len() > 4096)
                || item
                    .local_binding
                    .as_ref()
                    .is_some_and(|name| name.len() > 4096)
        })
        || module.exports.iter().any(|item| {
            item.export_name.is_empty()
                || item.export_name.len() > 4096
                || !matches!(item.kind.as_str(), "local" | "re_export" | "star")
                || item
                    .local_binding
                    .as_ref()
                    .is_some_and(|name| name.len() > 4096)
                || item
                    .source
                    .as_ref()
                    .is_some_and(|source| source.len() > 4096)
                || item
                    .imported_name
                    .as_ref()
                    .is_some_and(|name| name.len() > 4096)
        })
    {
        return Err(
            "module descriptor contains invalid identifiers or exceeds bounded collection sizes"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_resolved_requests(manifest: &UnitManifest) -> Result<(), String> {
    let Some(root) = &manifest.module else {
        if manifest.resolved_requests.is_empty() {
            return Ok(());
        }
        return Err("script compilation unit must not contain module resolver mappings".to_owned());
    };
    let mut descriptors = HashMap::from([(root.id.as_str(), root)]);
    for dependency in &manifest.dependencies {
        descriptors.insert(dependency.descriptor.id.as_str(), &dependency.descriptor);
    }
    let mut expected = HashSet::new();
    for (module_id, descriptor) in &descriptors {
        for specifier in &descriptor.requests {
            expected.insert(((*module_id).to_owned(), specifier.clone()));
        }
    }
    let mut actual = HashSet::new();
    for mapping in &manifest.resolved_requests {
        if !descriptors.contains_key(mapping.referrer.as_str())
            || !descriptors.contains_key(mapping.target.as_str())
            || mapping.specifier.is_empty()
            || mapping.specifier.len() > 4096
            || !actual.insert((mapping.referrer.clone(), mapping.specifier.clone()))
        {
            return Err(
                "compilation-unit contains an invalid or duplicate module resolver mapping"
                    .to_owned(),
            );
        }
    }
    if actual != expected {
        return Err("compilation-unit resolver mappings do not cover the complete static module request graph".to_owned());
    }
    if manifest.resolved_requests.iter().any(|mapping| {
        !(mapping.specifier.starts_with("./") || mapping.specifier.starts_with("../"))
            || mapping.specifier.contains('\\')
            || mapping.specifier.contains('\0')
    }) {
        return Err(
            "compilation-unit contains an unsupported non-relative module request".to_owned(),
        );
    }
    Ok(())
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn validate_source_info(source: &SourceInfo) -> Result<(), String> {
    let components = source.path.split('/').collect::<Vec<_>>();
    let valid_path = !source.path.is_empty()
        && source.path.len() <= 4096
        && !source.path.contains('\\')
        && !source.path.starts_with('/')
        && components
            .iter()
            .all(|item| !item.is_empty() && *item != "." && *item != "..");
    let expected_id = format!("source_{}", sha256_hex(source.path.as_bytes()));
    if !valid_path || source.id != expected_id || !valid_digest(&source.sha256) {
        return Err("compilation-unit source metadata contains an invalid relative path, identity or SHA-256 digest".to_owned());
    }
    Ok(())
}

fn validate_unit_header(manifest: &UnitManifest) -> Result<(), String> {
    if manifest.schema != UNIT_SCHEMA || manifest.version != UNIT_VERSION {
        return Err(format!(
            "unsupported compilation-unit format {:?} version {:?}",
            manifest.schema, manifest.version
        ));
    }
    validate_source_info(&manifest.source)?;
    if !valid_digest(&manifest.entry_sha256)
        || manifest.entry_function.is_empty()
        || manifest.entry_function.len() > 128
        || manifest.functions.is_empty()
        || manifest.functions.len() > 65_536
        || manifest.dependencies.len() > 255
        || manifest.resolved_requests.len() > 65_280
    {
        return Err("compilation-unit manifest has invalid entry digest, function identity or bounded collection sizes".to_owned());
    }
    let mut dependency_ids = HashSet::new();
    let mut source_paths = HashSet::from([manifest.source.path.as_str()]);
    if let Some(module) = &manifest.module {
        if module.id != manifest.source.id || module.entry_function != manifest.entry_function {
            return Err(
                "root module descriptor does not match the source or entry function".to_owned(),
            );
        }
        validate_module_descriptor(module)?;
    } else if !manifest.dependencies.is_empty() || !manifest.resolved_requests.is_empty() {
        return Err(
            "script compilation unit must not contain ESM dependencies or resolver mappings"
                .to_owned(),
        );
    }
    for dependency in &manifest.dependencies {
        validate_source_info(&dependency.source)?;
        if !source_paths.insert(dependency.source.path.as_str()) {
            return Err("compilation-unit contains duplicate relative source paths".to_owned());
        }
        if !dependency_ids.insert(dependency.descriptor.id.as_str()) {
            return Err("compilation-unit contains duplicate dependency module IDs".to_owned());
        }
        if dependency.descriptor.id != dependency.source.id
            || dependency.descriptor.entry_function.is_empty()
            || dependency.descriptor.entry_function.len() > 128
            || dependency.functions.is_empty()
            || dependency.functions.len() > 65_536
            || dependency
                .functions
                .iter()
                .filter(|item| item.id == dependency.descriptor.entry_function)
                .count()
                != 1
        {
            return Err(
                "compilation-unit dependency metadata is inconsistent or exceeds its limits"
                    .to_owned(),
            );
        }
        validate_module_descriptor(&dependency.descriptor)?;
    }
    validate_resolved_requests(manifest)?;
    Ok(())
}

fn is_safe_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn check_function_identity(
    verified: &VerifiedArtifact,
    expected: &ArtifactEntry,
) -> Result<(), String> {
    let metadata = verified
        .manifest
        .get("metadata")
        .ok_or_else(|| "verified artifact is missing function metadata".to_owned())?;
    let id = metadata.pointer("/function/id").and_then(Value::as_str);
    let name = metadata.pointer("/function/name").and_then(Value::as_str);
    if id != Some(expected.id.as_str()) || name != Some(expected.name.as_str()) {
        return Err(format!(
            "artifact function identity mismatch: expected {:?} ({:?}), found {:?} ({:?})",
            expected.id, expected.name, id, name
        ));
    }
    Ok(())
}

pub(crate) fn write_compilation_unit(
    output: &Path,
    entry_bytes: &[u8],
    helper_artifacts: &[(ArtifactEntry, Vec<u8>)],
    manifest: &UnitManifest,
) -> Result<(), String> {
    validate_unit_header(manifest)?;
    if manifest.dependencies.len() > 255
        || (manifest.module.is_none()
            && (!manifest.dependencies.is_empty() || !manifest.resolved_requests.is_empty()))
    {
        return Err("compilation-unit dependency graph metadata is inconsistent".to_owned());
    }
    if let Some(module) = &manifest.module {
        if module.id != manifest.source.id || module.entry_function != manifest.entry_function {
            return Err(
                "root module descriptor does not match the source or entry function".to_owned(),
            );
        }
        validate_module_descriptor(module)?;
    }
    if sha256_hex(entry_bytes) != manifest.entry_sha256 {
        return Err("entry artifact digest does not match compilation-unit manifest".to_owned());
    }
    let root_entry = manifest
        .functions
        .iter()
        .filter(|item| item.file.is_none())
        .collect::<Vec<_>>();
    if root_entry.len() != 1 || root_entry[0].id != manifest.entry_function {
        return Err("compilation-unit must contain exactly one root entry function".to_owned());
    }
    let verified_entry = verify_artifact(entry_bytes)?;
    check_function_identity(&verified_entry, root_entry[0])?;
    let sidecar = sidecar_path(output);
    if let Ok(metadata) = fs::symlink_metadata(&sidecar)
        && metadata.file_type().is_symlink()
    {
        return Err(format!(
            "refusing to write through sidecar symlink {}",
            sidecar.display()
        ));
    }
    fs::create_dir_all(&sidecar)
        .map_err(|error| format!("cannot create {}: {error}", sidecar.display()))?;
    let canonical_sidecar = fs::canonicalize(&sidecar)
        .map_err(|error| format!("cannot resolve sidecar {}: {error}", sidecar.display()))?;

    let expected_helpers = manifest
        .functions
        .iter()
        .chain(
            manifest
                .dependencies
                .iter()
                .flat_map(|dependency| dependency.functions.iter()),
        )
        .filter(|entry| entry.file.is_some())
        .collect::<Vec<_>>();
    if expected_helpers.len() != helper_artifacts.len() {
        return Err(
            "compilation-unit helper artifacts do not match the manifest function index".to_owned(),
        );
    }
    let mut written_ids = HashSet::from([manifest.entry_function.as_str()]);
    let mut written_paths = HashSet::new();
    for (entry, bytes) in helper_artifacts {
        if !written_ids.insert(entry.id.as_str()) {
            return Err(format!("duplicate helper function identity {:?}", entry.id));
        }
        if !expected_helpers.iter().any(|expected| {
            expected.id == entry.id && expected.name == entry.name && expected.file == entry.file
        }) {
            return Err(format!(
                "helper artifact {:?} is not referenced by the compilation-unit manifest",
                entry.id
            ));
        }
        let relative_name = entry
            .file
            .as_deref()
            .ok_or_else(|| "helper artifact has no sidecar path".to_owned())?;
        if !written_paths.insert(relative_name) {
            return Err(format!("duplicate helper output path {relative_name:?}"));
        }
        let relative = Path::new(relative_name);
        if !is_safe_relative_path(relative)
            || relative.extension().and_then(|value| value.to_str()) != Some("ogmb")
        {
            return Err(format!(
                "unsafe or unsupported helper output path {relative:?}"
            ));
        }
        let verified = verify_artifact(bytes)?;
        check_function_identity(&verified, entry)?;
        let target = sidecar.join(relative);
        let target_parent = target.parent().unwrap_or(&sidecar);
        fs::create_dir_all(target_parent).map_err(|error| {
            format!(
                "cannot create helper artifact directory {}: {error}",
                target_parent.display()
            )
        })?;
        let canonical_parent = fs::canonicalize(target_parent).map_err(|error| {
            format!(
                "cannot resolve helper artifact directory {}: {error}",
                target_parent.display()
            )
        })?;
        if !canonical_parent.starts_with(&canonical_sidecar) {
            return Err(format!(
                "helper artifact output {} escapes the sidecar directory",
                target.display()
            ));
        }
        write_atomic(&target, bytes)?;
    }
    let manifest_bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| format!("cannot encode compilation-unit manifest: {error}"))?;
    if manifest_bytes.len() as u64 > MAX_UNIT_MANIFEST_BYTES {
        return Err("compilation-unit manifest exceeds the size limit".to_owned());
    }
    write_atomic(&sidecar.join("unit.json"), &manifest_bytes)?;
    write_atomic(output, entry_bytes)?;
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "cannot create output directory {}: {error}",
            parent.display()
        )
    })?;
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.tmp-{}-{sequence}",
        std::process::id()
    ));
    let result = (|| -> io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        match fs::rename(&temporary, path) {
            Ok(()) => Ok(()),
            Err(error) if path.exists() => {
                fs::remove_file(path)?;
                fs::rename(&temporary, path).map_err(|replacement_error| {
                    io::Error::new(
                        replacement_error.kind(),
                        format!("{error}; replacement failed: {replacement_error}"),
                    )
                })
            }
            Err(error) => Err(error),
        }
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|error| format!("cannot write {}: {error}", path.display()))
}

pub(crate) fn artifact_report(verified: &VerifiedArtifact, byte_len: usize) -> Value {
    let manifest = &verified.manifest;
    let metadata = manifest.get("metadata").cloned().unwrap_or(Value::Null);
    let source_map = verified.source_map.as_ref();
    let files = source_map
        .and_then(|map| map.get("files"))
        .and_then(Value::as_array);
    let file_summaries: Vec<Value> = files
        .into_iter()
        .flatten()
        .map(|file| {
            json!({
                "id": file.get("id"),
                "path": file.get("path"),
                "language": file.get("language"),
                "sha256": file.get("sha256"),
            })
        })
        .collect();
    json!({
        "container": {
            "format": manifest.get("format"),
            "version": manifest.get("version"),
            "encoding_id": manifest.get("encoding_id"),
            "feature_flags": if verified.bytes.len() >= 20 {
                u64::from_le_bytes(verified.bytes[12..20].try_into().expect("fixed header slice"))
            } else { 0 },
            "bytes": byte_len,
            "code_bytes": verified.code.len(),
            "instructions": verified.instructions.len(),
            "constants": verified.constants.len(),
            "exception_regions": verified.exceptions.len(),
            "code_sha256": manifest.get("code_sha256"),
        },
        "function": metadata.get("function"),
        "execution_model": metadata.get("execution_model"),
        "frame_layout": metadata.get("frame_layout"),
        "environment": metadata.get("environment"),
        "verifier_summary": metadata.get("verifier_summary"),
        "source": {
            "files": file_summaries,
            "location_count": source_map.and_then(|map| map.get("locations")).and_then(Value::as_array).map_or(0, Vec::len),
            "lexical_scope_count": source_map.and_then(|map| map.get("lexical_scopes")).and_then(Value::as_array).map_or(0, Vec::len),
            "variable_location_count": source_map.and_then(|map| map.get("variable_locations")).and_then(Value::as_array).map_or(0, Vec::len),
        }
    })
}

/// Returns a Core IR-safe deterministic source identity for a canonical filesystem path.
pub(crate) fn source_relative_path(root: &Path, path: &Path) -> Result<String, String> {
    let relative = path.strip_prefix(root).map_err(|_| {
        format!(
            "source {} is outside its compilation-unit root {}",
            path.display(),
            root.display()
        )
    })?;
    let components = relative
        .components()
        .map(|component| match component {
            Component::Normal(value) => value.to_str().map(str::to_owned).ok_or_else(|| {
                format!(
                    "source {} contains a non-UTF-8 path component",
                    path.display()
                )
            }),
            _ => Err(format!(
                "source {} has a non-normal relative path",
                path.display()
            )),
        })
        .collect::<Result<Vec<_>, String>>()?;
    if components.is_empty() {
        return Err(format!(
            "source {} does not identify a file below its compilation-unit root",
            path.display()
        ));
    }
    Ok(components.join("/"))
}

pub(crate) fn source_id(root: &Path, path: &Path) -> Result<String, String> {
    let relative = source_relative_path(root, path)?;
    Ok(format!("source_{}", sha256_hex(relative.as_bytes())))
}

/// Returns a compiler-accepted memory URI while keeping the filesystem path out of Core IR.
pub(crate) fn source_uri(id: &str, _path: &Path) -> String {
    format!("memory:///cli/{id}/source.js")
}

pub(crate) fn sha256(bytes: &[u8]) -> String {
    sha256_hex(bytes)
}
