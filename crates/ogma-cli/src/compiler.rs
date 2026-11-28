//! Source-to-OGMB compilation through the production frontend/compiler.

use crate::artifact::{
    ArtifactEntry, ModuleDescriptorData, SourceInfo, UnitManifest, sha256, source_id,
    source_relative_path, source_uri, write_compilation_unit,
};
use crate::module_source::compile_dependency_graph;
use ogma_compiler::{CompileFailure, compile_to_bytecode};
use ogma_frontend::{
    DiagnosticSeverity, ParseGoal, ParseMode, ParseRequestOptions, SourceFile, parse,
    parse_typescript,
};
use oxc_allocator::Allocator;
use std::fs;
use std::path::{Path, PathBuf};

const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SourceGoal {
    Script,
    Module,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SourceLanguage {
    JavaScript,
    TypeScript,
    TypeScriptJsx,
}

pub(crate) struct CompileOptions {
    pub(crate) source: PathBuf,
    pub(crate) output: PathBuf,
    pub(crate) goal: SourceGoal,
    pub(crate) language: SourceLanguage,
}

pub(crate) struct CompileSummary {
    pub(crate) output: PathBuf,
    pub(crate) function_count: usize,
    pub(crate) module: bool,
}

pub(crate) fn compile_file(options: CompileOptions) -> Result<CompileSummary, String> {
    if options.output.extension().and_then(|value| value.to_str()) != Some("ogmb") {
        return Err("compile output must use the .ogmb extension".to_owned());
    }
    let source_path = fs::canonicalize(&options.source).map_err(|error| {
        format!(
            "cannot resolve source {}: {error}",
            options.source.display()
        )
    })?;
    let metadata = fs::metadata(&source_path)
        .map_err(|error| format!("cannot inspect source {}: {error}", source_path.display()))?;
    if !metadata.is_file() {
        return Err(format!(
            "source {} is not a regular file",
            source_path.display()
        ));
    }
    if metadata.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "source exceeds the {}-byte CLI source limit",
            MAX_SOURCE_BYTES
        ));
    }
    let source_text = fs::read_to_string(&source_path).map_err(|error| {
        format!(
            "cannot read UTF-8 source {}: {error}",
            source_path.display()
        )
    })?;
    let source_parent = source_path
        .parent()
        .ok_or_else(|| format!("source {} has no parent directory", source_path.display()))?;
    let source_root = fs::canonicalize(source_parent).map_err(|error| {
        format!(
            "cannot resolve source root {}: {error}",
            source_parent.display()
        )
    })?;
    let source_id = source_id(&source_root, &source_path)?;
    let uri = source_uri(&source_id, &source_path);
    let source = SourceFile::new(source_id.clone(), uri, None, source_text)
        .map_err(|error| format!("invalid source {}: {error}", source_path.display()))?;
    let allocator = Allocator::default();
    let parse_options = ParseRequestOptions {
        goal: match options.goal {
            SourceGoal::Script => ParseGoal::Script,
            SourceGoal::Module => ParseGoal::Module,
        },
        mode: ParseMode::Sloppy,
    };
    let parsed_result = match options.language {
        SourceLanguage::JavaScript => parse(&allocator, &source, parse_options),
        SourceLanguage::TypeScript => parse_typescript(&allocator, &source, parse_options, false),
        SourceLanguage::TypeScriptJsx => parse_typescript(&allocator, &source, parse_options, true),
    };
    let parsed = parsed_result
        .map_err(|failure| render_parse_failure(&source_path.display().to_string(), &failure))?;

    for diagnostic in parsed
        .diagnostics
        .iter()
        .filter(|item| item.severity == DiagnosticSeverity::Warning)
    {
        eprintln!(
            "{}: warning{}: {}",
            source_path.display(),
            diagnostic
                .code
                .as_deref()
                .map(|code| format!("[{code}]"))
                .unwrap_or_default(),
            diagnostic.message
        );
        if let Some(help) = &diagnostic.help {
            eprintln!("  help: {help}");
        }
    }

    let compiled = compile_to_bytecode(&parsed)
        .map_err(|failure| render_compile_failure(&source_path.display().to_string(), failure))?;
    let module_descriptor = compiled.module.as_ref();
    let module = module_descriptor.map(ModuleDescriptorData::from);
    if (options.goal == SourceGoal::Module) != module.is_some() {
        return Err(
            "compiler returned module metadata inconsistent with the requested source goal"
                .to_owned(),
        );
    }
    let (dependency_modules, resolved_requests) = if let Some(descriptor) = module_descriptor {
        let graph = compile_dependency_graph(&source_path, &descriptor.id, &descriptor.requests)?;
        (graph.modules, graph.resolved_requests)
    } else {
        (Vec::new(), Vec::new())
    };
    let entry_id = module
        .as_ref()
        .map(|descriptor| descriptor.entry_function.as_str());
    let entry = compiled
        .functions
        .iter()
        .find(|function| entry_id.map_or(function.name == "<program>", |id| function.id == id))
        .ok_or_else(|| "compiler output does not contain the Program entry function".to_owned())?;
    let entry_bytes = entry.artifact.bytes.clone();

    let mut entries = Vec::with_capacity(compiled.functions.len());
    let mut helpers = Vec::new();
    for function in &compiled.functions {
        if function.id == entry.id {
            entries.push(ArtifactEntry {
                id: function.id.clone(),
                name: function.name.clone(),
                file: None,
            });
        } else {
            let file = format!("functions/{}.ogmb", sha256(function.id.as_bytes()));
            let descriptor = ArtifactEntry {
                id: function.id.clone(),
                name: function.name.clone(),
                file: Some(file),
            };
            helpers.push((descriptor.clone(), function.artifact.bytes.clone()));
            entries.push(descriptor);
        }
    }

    let mut dependency_data = Vec::with_capacity(dependency_modules.len());
    let mut all_helpers = helpers;
    let dependency_function_count = dependency_modules
        .iter()
        .map(|dependency| dependency.data.functions.len())
        .sum::<usize>();
    for dependency in dependency_modules {
        all_helpers.extend(dependency.artifacts);
        dependency_data.push(dependency.data);
    }

    let manifest = UnitManifest {
        schema: "ogma.cli-unit".to_owned(),
        version: "1.0".to_owned(),
        source: SourceInfo {
            path: source_relative_path(&source_root, &source_path)?,
            id: source_id,
            sha256: source.content_sha256().to_owned(),
        },
        entry_function: entry.id.clone(),
        entry_sha256: sha256(&entry_bytes),
        module,
        functions: entries,
        dependencies: dependency_data,
        resolved_requests,
    };
    let output = absolute_output_path(&options.output)?;
    write_compilation_unit(&output, &entry_bytes, &all_helpers, &manifest)?;
    Ok(CompileSummary {
        output,
        function_count: compiled.functions.len() + dependency_function_count,
        module: manifest.module.is_some(),
    })
}

fn absolute_output_path(path: &Path) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "cannot create output directory {}: {error}",
            parent.display()
        )
    })?;
    let parent = fs::canonicalize(parent).map_err(|error| {
        format!(
            "cannot resolve output directory {}: {error}",
            parent.display()
        )
    })?;
    let name = path
        .file_name()
        .ok_or_else(|| "output path must name a file".to_owned())?;
    Ok(parent.join(name))
}

fn render_parse_failure(source_label: &str, failure: &ogma_frontend::ParseFailure) -> String {
    let mut output = Vec::new();
    for diagnostic in &failure.diagnostics {
        let code = diagnostic
            .code
            .as_deref()
            .map(|value| format!("[{value}]"))
            .unwrap_or_default();
        let primary = diagnostic
            .labels
            .iter()
            .find(|label| label.primary)
            .or_else(|| diagnostic.labels.first());
        if let Some(label) = primary {
            let start = label.span.start;
            output.push(format!(
                "{}:{}:{}: error{}: {}",
                source_label,
                start.line,
                start.column_utf16 + 1,
                code,
                diagnostic.message
            ));
            if let Some(message) = &label.message {
                output.push(format!("  {message}"));
            }
        } else {
            output.push(format!(
                "{}: error{}: {}",
                source_label, code, diagnostic.message
            ));
        }
        if let Some(help) = &diagnostic.help {
            output.push(format!("  help: {help}"));
        }
        if let Some(note) = &diagnostic.note {
            output.push(format!("  note: {note}"));
        }
    }
    if output.is_empty() {
        format!(
            "{}: source parsing failed ({:?})",
            source_label, failure.phase
        )
    } else {
        output.join("\n")
    }
}

pub(crate) fn render_compile_failure(source_label: &str, failure: CompileFailure) -> String {
    match failure {
        CompileFailure::Semantic(diagnostics) => diagnostics
            .into_iter()
            .map(|diagnostic| {
                if let Some(span) = diagnostic.span {
                    format!(
                        "{}:{}:{}: error[{}]: {}",
                        source_label,
                        span.start.line,
                        span.start.column_utf16 + 1,
                        diagnostic.code,
                        diagnostic.message
                    )
                } else {
                    format!(
                        "{}: error[{}]: {}",
                        source_label, diagnostic.code, diagnostic.message
                    )
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        CompileFailure::Lowering(diagnostics) => diagnostics
            .into_iter()
            .map(|diagnostic| {
                if let Some(span) = diagnostic.span {
                    format!(
                        "{}:{}:{}: error[{}] in {}: {}",
                        source_label,
                        span.start.line,
                        span.start.column_utf16 + 1,
                        diagnostic.code,
                        diagnostic.function,
                        diagnostic.message
                    )
                } else {
                    format!(
                        "{}: error[{}] in {}: {}",
                        source_label, diagnostic.code, diagnostic.function, diagnostic.message
                    )
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        CompileFailure::Bytecode(diagnostics) => diagnostics
            .into_iter()
            .map(|diagnostic| {
                format!(
                    "{}: error[{}] at {}: {}",
                    source_label, diagnostic.code, diagnostic.path, diagnostic.message
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}
