//! Bounded filesystem module-source resolver used during M1 compilation.

use crate::artifact::{
    ArtifactEntry, DependencyModuleData, ModuleDescriptorData, ResolvedModuleRequest, SourceInfo,
    sha256, source_id, source_relative_path, source_uri,
};
use crate::compiler::render_compile_failure;
use ogma_compiler::compile_to_bytecode;
use ogma_frontend::{ParseGoal, ParseMode, ParseRequestOptions, SourceFile, parse};
use oxc_allocator::Allocator;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_MODULES: usize = 256;
const MAX_MODULE_GRAPH_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

pub(crate) struct CompiledDependencyModule {
    pub(crate) data: DependencyModuleData,
    pub(crate) artifacts: Vec<(ArtifactEntry, Vec<u8>)>,
}

pub(crate) struct CompiledDependencyGraph {
    pub(crate) modules: Vec<CompiledDependencyModule>,
    pub(crate) resolved_requests: Vec<ResolvedModuleRequest>,
}

/// Compiles the static dependency graph of one Module-goal entry source.
///
/// Resolution is intentionally explicit: only relative .js/.mjs files below
/// the entry directory are accepted. No Node or package-manager probing occurs.
pub(crate) fn compile_dependency_graph(
    entry_path: &Path,
    entry_id: &str,
    requests: &[String],
) -> Result<CompiledDependencyGraph, String> {
    let root = entry_path
        .parent()
        .ok_or_else(|| "module entry source has no parent directory".to_owned())?;
    let root = fs::canonicalize(root).map_err(|error| {
        format!(
            "cannot resolve module source root {}: {error}",
            root.display()
        )
    })?;
    let entry_size = fs::metadata(entry_path)
        .map_err(|error| {
            format!(
                "cannot inspect module entry source {}: {error}",
                entry_path.display()
            )
        })?
        .len();
    let mut total_bytes = entry_size;
    if total_bytes > MAX_MODULE_GRAPH_SOURCE_BYTES {
        return Err("module source graph exceeds the 64 MiB CLI source limit".to_owned());
    }

    let mut queue = VecDeque::new();
    for specifier in requests {
        queue.push_back((entry_id.to_owned(), specifier.clone()));
    }
    let mut visited = HashSet::from([entry_id.to_owned()]);
    let mut source_paths = HashMap::from([(entry_id.to_owned(), entry_path.to_path_buf())]);
    let mut modules = Vec::new();
    let mut resolved_requests = Vec::new();

    while let Some((referrer, specifier)) = queue.pop_front() {
        let referrer_path = source_paths.get(&referrer).ok_or_else(|| {
            format!("module graph references unknown referrer identity {referrer:?}")
        })?;
        let path = resolve_source(&root, referrer_path, &specifier)?;
        let module_id = source_id(&root, &path)?;
        resolved_requests.push(ResolvedModuleRequest {
            referrer: referrer.clone(),
            specifier: specifier.clone(),
            target: module_id.clone(),
        });
        if !visited.insert(module_id.clone()) {
            continue;
        }
        if visited.len() > MAX_MODULES {
            return Err(format!(
                "module graph exceeds the {MAX_MODULES}-module CLI limit"
            ));
        }

        let metadata = fs::metadata(&path).map_err(|error| {
            format!("cannot inspect imported module {}: {error}", path.display())
        })?;
        total_bytes = total_bytes.saturating_add(metadata.len());
        if total_bytes > MAX_MODULE_GRAPH_SOURCE_BYTES {
            return Err("module source graph exceeds the 64 MiB CLI source limit".to_owned());
        }

        let module = compile_module(&root, &path)?;
        source_paths.insert(module_id.clone(), path.clone());
        for request in &module.data.descriptor.requests {
            queue.push_back((module_id.clone(), request.clone()));
        }
        modules.push(module);
    }
    Ok(CompiledDependencyGraph {
        modules,
        resolved_requests,
    })
}

fn resolve_source(root: &Path, referrer: &Path, specifier: &str) -> Result<PathBuf, String> {
    if !(specifier.starts_with("./") || specifier.starts_with("../"))
        || specifier.contains('\\')
        || specifier.contains('\0')
    {
        return Err(format!(
            "M1 module resolver accepts only explicit relative specifiers with forward slashes; received {specifier:?}"
        ));
    }
    let parent = referrer.parent().ok_or_else(|| {
        format!(
            "referrer module {} has no parent directory",
            referrer.display()
        )
    })?;
    let unresolved = parent.join(specifier);
    let resolved = fs::canonicalize(&unresolved).map_err(|error| {
        format!(
            "cannot resolve module {specifier:?} from {}: {error}",
            referrer.display()
        )
    })?;
    if !resolved.starts_with(root) {
        return Err(format!(
            "module request {specifier:?} resolves outside the entry source directory"
        ));
    }
    if !resolved.is_file() {
        return Err(format!(
            "module request {specifier:?} does not resolve to a regular file"
        ));
    }
    if !matches!(
        resolved.extension().and_then(|value| value.to_str()),
        Some("js" | "mjs")
    ) {
        return Err(format!(
            "M1 module imports must resolve to .js or .mjs files; got {}",
            resolved.display()
        ));
    }
    Ok(resolved)
}

fn compile_module(root: &Path, path: &Path) -> Result<CompiledDependencyModule, String> {
    let identity = source_id(root, path)?;
    let text = fs::read_to_string(path)
        .map_err(|error| format!("cannot read UTF-8 module {}: {error}", path.display()))?;
    let uri = source_uri(&identity, path);
    let source = SourceFile::new(identity.clone(), uri, None, text)
        .map_err(|error| format!("invalid module source {}: {error}", path.display()))?;
    let allocator = Allocator::default();
    let parsed = parse(
        &allocator,
        &source,
        ParseRequestOptions {
            goal: ParseGoal::Module,
            mode: ParseMode::Sloppy,
        },
    )
    .map_err(|failure| {
        failure
            .diagnostics
            .into_iter()
            .map(|diagnostic| {
                let code = diagnostic
                    .code
                    .map(|code| format!("[{code}]"))
                    .unwrap_or_default();
                if let Some(label) = diagnostic
                    .labels
                    .iter()
                    .find(|item| item.primary)
                    .or_else(|| diagnostic.labels.first())
                {
                    format!(
                        "{}:{}:{}: error{}: {}",
                        path.display(),
                        label.span.start.line,
                        label.span.start.column_utf16 + 1,
                        code,
                        diagnostic.message
                    )
                } else {
                    format!("{}: error{}: {}", path.display(), code, diagnostic.message)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    })?;
    let compiled = compile_to_bytecode(&parsed)
        .map_err(|failure| render_compile_failure(&path.display().to_string(), failure))?;
    let descriptor = compiled.module.ok_or_else(|| {
        format!(
            "Module-goal compiler did not emit a descriptor for {}",
            path.display()
        )
    })?;

    let mut functions = Vec::with_capacity(compiled.functions.len());
    let mut artifacts = Vec::with_capacity(compiled.functions.len());
    for function in compiled.functions {
        let file = format!(
            "modules/{}/functions/{}.ogmb",
            sha256(identity.as_bytes()),
            sha256(function.id.as_bytes()),
        );
        let entry = ArtifactEntry {
            id: function.id,
            name: function.name,
            file: Some(file),
        };
        artifacts.push((entry.clone(), function.artifact.bytes));
        functions.push(entry);
    }
    if !functions
        .iter()
        .any(|function| function.id == descriptor.entry_function)
    {
        return Err(format!(
            "compiled module {} does not contain its Program entry function",
            path.display()
        ));
    }

    let data = DependencyModuleData {
        source: SourceInfo {
            path: source_relative_path(root, path)?,
            id: identity,
            sha256: source.content_sha256().to_owned(),
        },
        descriptor: ModuleDescriptorData::from(&descriptor),
        functions,
    };
    Ok(CompiledDependencyModule { data, artifacts })
}
