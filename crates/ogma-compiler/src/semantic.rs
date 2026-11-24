//! Language-neutral binding and closure analysis over the frontend's resolved snapshot.

use std::collections::{HashMap, HashSet};

use ogma_core_ir::{
    Binding, BindingKind, Capture, CaptureMode, Environment, FunctionParameter, Representation,
    SemanticType,
};
use ogma_frontend::{
    ParseDiagnostic, ResolvedBinding, ResolvedBindingKind, ResolvedFunction, SemanticSnapshot,
    SourceFile, SourceSpan,
};

/// A structured compiler semantic-analysis diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticDiagnostic {
    /// Stable diagnostic code.
    pub code: &'static str,
    /// Optional source range associated with the issue.
    pub span: Option<SourceSpan>,
    /// Human-readable message.
    pub message: String,
}

/// A source binding paired with its canonical Core IR descriptor.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticBinding {
    /// Canonical Core IR binding descriptor.
    pub ir: Binding,
    /// Exact lexical scope that owns the binding.
    pub lexical_scope: String,
    /// Source declaration range.
    pub declaration: SourceSpan,
}

/// Per-function semantic facts required by canonical Core IR lowering.
#[derive(Clone, Debug, PartialEq)]
pub struct FunctionSemantics {
    /// Stable function identity from the frontend.
    pub id: String,
    /// Source name or generated frontend label.
    pub name: String,
    /// Function's lexical environment and local bindings.
    pub environment: Environment,
    /// Ordered function parameters.
    pub parameters: Vec<FunctionParameter>,
    /// Captured bindings imported from enclosing function environments.
    pub captures: Vec<Capture>,
    /// Source range of this function.
    pub span: SourceSpan,
    /// Stable enclosing function scope, if nested.
    pub parent_function_scope: Option<String>,
    /// Whether this is an arrow function.
    pub is_arrow: bool,
    /// Whether this function is async.
    pub is_async: bool,
    /// Whether this function is a generator.
    pub is_generator: bool,
}

/// Resolved semantic input for a single source compilation unit.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticModule {
    /// Stable frontend source identity.
    pub source_id: String,
    /// SHA-256 of exact source bytes.
    pub source_sha256: String,
    /// Function and Program semantic units in deterministic source order.
    pub functions: Vec<FunctionSemantics>,
    /// All bindings, including bindings in nested lexical blocks.
    pub bindings: Vec<SemanticBinding>,
}

/// Analyze frontend-resolved scopes, declarations and references into owned
/// Core IR environment/capture facts.
///
/// Oxc is deliberately not referenced from this crate. The frontend owns
/// parsing and ECMAScript name resolution; this function validates that the
/// resulting facts can be represented by the M1 Core IR binding model.
pub fn analyze(
    source: &SourceFile,
    snapshot: &SemanticSnapshot,
) -> Result<SemanticModule, Vec<SemanticDiagnostic>> {
    let scopes: HashMap<&str, _> = snapshot
        .scopes
        .iter()
        .map(|scope| (scope.id.as_str(), scope))
        .collect();
    let bindings_by_id: HashMap<&str, &ResolvedBinding> = snapshot
        .bindings
        .iter()
        .map(|binding| (binding.id.as_str(), binding))
        .collect();

    let mut diagnostics = Vec::new();
    let mut semantic_bindings = Vec::with_capacity(snapshot.bindings.len());
    let mut ir_bindings_by_id = HashMap::with_capacity(snapshot.bindings.len());

    for binding in &snapshot.bindings {
        let Some(scope) = scopes.get(binding.scope.as_str()) else {
            diagnostics.push(SemanticDiagnostic {
                code: "semantic.unknown_binding_scope",
                span: Some(binding.declaration),
                message: format!(
                    "binding {:?} references missing lexical scope {:?}",
                    binding.name, binding.scope
                ),
            });
            continue;
        };
        let kind = match binding.kind {
            ResolvedBindingKind::Var => BindingKind::Var,
            ResolvedBindingKind::Let => BindingKind::Let,
            ResolvedBindingKind::Const => BindingKind::Const,
            ResolvedBindingKind::Parameter => BindingKind::Parameter,
            ResolvedBindingKind::Catch => BindingKind::Catch,
            ResolvedBindingKind::Import => BindingKind::Import,
            ResolvedBindingKind::Function if scope.is_var => BindingKind::Var,
            ResolvedBindingKind::Function => BindingKind::Let,
            ResolvedBindingKind::Other => {
                diagnostics.push(SemanticDiagnostic {
                    code: "semantic.unsupported_binding",
                    span: Some(binding.declaration),
                    message: format!(
                        "binding {:?} is not representable in the M1 Core IR environment contract",
                        binding.name
                    ),
                });
                continue;
            }
        };
        let expected_mutability = matches!(
            kind,
            BindingKind::Var | BindingKind::Let | BindingKind::Parameter | BindingKind::Catch
        );
        if binding.mutable != expected_mutability {
            diagnostics.push(SemanticDiagnostic {
                code: "semantic.binding_mutability",
                span: Some(binding.declaration),
                message: format!(
                    "frontend mutability for {:?} conflicts with the Core IR binding kind",
                    binding.name
                ),
            });
            continue;
        }
        let descriptor = Binding {
            id: binding.id.clone(),
            name: binding.name.clone(),
            kind,
            mutable: binding.mutable,
            semantic_type: SemanticType::Dynamic,
            representation: Representation::Boxed,
        };
        ir_bindings_by_id.insert(binding.id.as_str(), descriptor.clone());
        semantic_bindings.push(SemanticBinding {
            ir: descriptor,
            lexical_scope: binding.scope.clone(),
            declaration: binding.declaration,
        });
    }

    let mut function_by_scope: HashMap<&str, &ResolvedFunction> = HashMap::new();
    let mut function_ids = HashSet::new();
    for function in &snapshot.functions {
        if !function_ids.insert(function.id.as_str()) {
            diagnostics.push(SemanticDiagnostic {
                code: "semantic.duplicate_function_id",
                span: Some(function.span),
                message: format!("duplicate function identity {:?}", function.id),
            });
        }
        if function_by_scope
            .insert(function.scope.as_str(), function)
            .is_some()
        {
            diagnostics.push(SemanticDiagnostic {
                code: "semantic.duplicate_function_scope",
                span: Some(function.span),
                message: format!(
                    "multiple function descriptors own scope {:?}",
                    function.scope
                ),
            });
        }
    }

    let function_scope_for = |scope_id: &str| -> Option<&str> {
        let mut current = Some(scope_id);
        while let Some(id) = current {
            let scope = scopes.get(id)?;
            if scope.is_function || id == snapshot.root_scope.as_str() {
                return Some(scope.id.as_str());
            }
            current = scope.parent.as_deref();
        }
        None
    };

    for function in &snapshot.functions {
        if function.scope != snapshot.root_scope {
            if let Some(parent_scope) = &function.parent_function_scope {
                if !function_by_scope.contains_key(parent_scope.as_str()) {
                    diagnostics.push(SemanticDiagnostic {
                        code: "semantic.unknown_parent_function",
                        span: Some(function.span),
                        message: format!(
                            "function {:?} references unknown parent function scope {:?}",
                            function.id, parent_scope
                        ),
                    });
                }
            } else {
                diagnostics.push(SemanticDiagnostic {
                    code: "semantic.missing_parent_function",
                    span: Some(function.span),
                    message: format!(
                        "nested function {:?} must identify its enclosing Program/function scope",
                        function.id
                    ),
                });
            }
        } else if function.parent_function_scope.is_some() {
            diagnostics.push(SemanticDiagnostic {
                code: "semantic.program_has_parent",
                span: Some(function.span),
                message: "the Program environment cannot have a parent function scope".to_owned(),
            });
        }
    }

    let mut functions = Vec::with_capacity(snapshot.functions.len());
    for function in &snapshot.functions {
        let is_program = function.scope == snapshot.root_scope;
        if scopes
            .get(function.scope.as_str())
            .is_none_or(|scope| !scope.is_function && !is_program)
        {
            diagnostics.push(SemanticDiagnostic {
                code: "semantic.invalid_function_scope",
                span: Some(function.span),
                message: format!(
                    "function {:?} does not own a function lexical scope",
                    function.id
                ),
            });
            continue;
        }
        if function.is_async || function.is_generator {
            diagnostics.push(SemanticDiagnostic {
                code: "semantic.unsupported_function_kind",
                span: Some(function.span),
                message: "async and generator suspension semantics are outside the synchronous M1 Core IR lowering scope".to_owned(),
            });
            continue;
        }
        if !function.simple_parameters {
            diagnostics.push(SemanticDiagnostic {
                code: "semantic.unsupported_parameter_pattern",
                span: Some(function.span),
                message: "destructured and rest parameters are not representable by the current M1 function parameter contract".to_owned(),
            });
            continue;
        }

        let mut local_bindings: Vec<Binding> = semantic_bindings
            .iter()
            .filter(|binding| {
                function_scope_for(&binding.lexical_scope) == Some(function.scope.as_str())
            })
            .map(|binding| binding.ir.clone())
            .collect();
        local_bindings.sort_by(|left, right| left.id.cmp(&right.id));

        let mut parameters = Vec::with_capacity(function.parameters.len());
        for (index, binding_id) in function.parameters.iter().enumerate() {
            let Some(binding) = ir_bindings_by_id.get(binding_id.as_str()) else {
                diagnostics.push(SemanticDiagnostic {
                    code: "semantic.parameter_binding_missing",
                    span: Some(function.span),
                    message: format!(
                        "function parameter references missing binding {binding_id:?}"
                    ),
                });
                continue;
            };
            if binding.kind != BindingKind::Parameter {
                diagnostics.push(SemanticDiagnostic {
                    code: "semantic.parameter_binding_kind",
                    span: Some(function.span),
                    message: format!(
                        "parameter {:?} is not represented as a parameter binding",
                        binding.name
                    ),
                });
                continue;
            }
            parameters.push(FunctionParameter {
                id: format!("param_{}_{}", function.id, index),
                kind: "value".to_owned(),
                semantic_type: binding.semantic_type,
                representation: binding.representation,
                name: binding.name.clone(),
                binding: binding.id.clone(),
            });
        }

        let mut captures = Vec::with_capacity(function.captures.len());
        let mut seen_captures = HashSet::new();
        for binding_id in &function.captures {
            if !seen_captures.insert(binding_id) {
                diagnostics.push(SemanticDiagnostic {
                    code: "semantic.duplicate_capture",
                    span: Some(function.span),
                    message: format!("capture {binding_id:?} occurs more than once"),
                });
                continue;
            }
            let Some(binding) = bindings_by_id.get(binding_id.as_str()) else {
                diagnostics.push(SemanticDiagnostic {
                    code: "semantic.capture_binding_missing",
                    span: Some(function.span),
                    message: format!("closure references unknown outer binding {binding_id:?}"),
                });
                continue;
            };
            let Some(ir_binding) = ir_bindings_by_id.get(binding_id.as_str()) else {
                diagnostics.push(SemanticDiagnostic {
                    code: "semantic.capture_unrepresentable",
                    span: Some(binding.declaration),
                    message: format!(
                        "captured binding {:?} cannot be represented in Core IR",
                        binding.name
                    ),
                });
                continue;
            };
            let owner = function_scope_for(&binding.scope).unwrap_or(snapshot.root_scope.as_str());
            if owner == function.scope {
                diagnostics.push(SemanticDiagnostic {
                    code: "semantic.capture_not_outer",
                    span: Some(binding.declaration),
                    message: format!(
                        "binding {:?} is declared within the capturing function",
                        binding.name
                    ),
                });
                continue;
            }
            captures.push(Capture {
                id: format!("capture_{}", binding.id),
                source_environment: owner.to_owned(),
                binding: binding.id.clone(),
                name: binding.name.clone(),
                mutable: ir_binding.mutable,
                semantic_type: ir_binding.semantic_type,
                representation: ir_binding.representation,
                mode: if ir_binding.mutable {
                    CaptureMode::Cell
                } else {
                    CaptureMode::Value
                },
            });
        }
        captures.sort_by(|left, right| left.binding.cmp(&right.binding));

        functions.push(FunctionSemantics {
            id: function.id.clone(),
            name: function.name.clone(),
            environment: Environment {
                id: function.scope.clone(),
                parent: function.parent_function_scope.clone(),
                bindings: local_bindings,
            },
            parameters,
            captures,
            span: function.span,
            parent_function_scope: function.parent_function_scope.clone(),
            is_arrow: function.is_arrow,
            is_async: function.is_async,
            is_generator: function.is_generator,
        });
    }

    if diagnostics.is_empty() {
        Ok(SemanticModule {
            source_id: source.id().as_str().to_owned(),
            source_sha256: source.content_sha256().to_owned(),
            functions,
            bindings: semantic_bindings,
        })
    } else {
        Err(diagnostics)
    }
}

/// Adapt a failed frontend parse/early-error diagnostic to the compiler
/// diagnostic surface without losing its source location or message.
pub fn from_frontend_diagnostic(diagnostic: &ParseDiagnostic) -> SemanticDiagnostic {
    SemanticDiagnostic {
        code: match diagnostic.phase {
            ogma_frontend::DiagnosticPhase::Parse => "frontend.parse",
            ogma_frontend::DiagnosticPhase::Early => "frontend.early_error",
        },
        span: diagnostic
            .labels
            .iter()
            .find(|label| label.primary)
            .map(|label| label.span),
        message: diagnostic.message.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ogma_frontend::{
        ResolvedBinding, ResolvedBindingKind, ResolvedFunction, ResolvedReference, ResolvedScope,
        SemanticSnapshot, SourcePosition,
    };
    use std::sync::Arc;

    fn span(start_byte: u32, end_byte: u32) -> SourceSpan {
        SourceSpan {
            start_byte,
            end_byte,
            start: SourcePosition {
                line: 1,
                column_utf16: start_byte,
            },
            end: SourcePosition {
                line: 1,
                column_utf16: end_byte,
            },
        }
    }

    fn source() -> SourceFile {
        SourceFile::new(
            "unit",
            "unit.js",
            None,
            Arc::<str>::from("function outer(arg) { let local = arg; return () => local; }"),
        )
        .expect("valid source")
    }

    fn scope(id: &str, parent: Option<&str>, is_function: bool, is_var: bool) -> ResolvedScope {
        ResolvedScope {
            id: id.to_owned(),
            parent: parent.map(str::to_owned),
            is_function,
            is_var,
        }
    }

    fn binding(
        id: &str,
        name: &str,
        kind: ResolvedBindingKind,
        mutable: bool,
        scope: &str,
    ) -> ResolvedBinding {
        ResolvedBinding {
            id: id.to_owned(),
            name: name.to_owned(),
            kind,
            mutable,
            scope: scope.to_owned(),
            declaration: span(0, 1),
            symbol_flags: 0,
        }
    }

    fn function(
        id: &str,
        scope: &str,
        parent: Option<&str>,
        parameters: Vec<&str>,
        captures: Vec<&str>,
        is_arrow: bool,
        simple_parameters: bool,
        is_async: bool,
    ) -> ResolvedFunction {
        ResolvedFunction {
            id: id.to_owned(),
            name: id.to_owned(),
            scope: scope.to_owned(),
            parent_function_scope: parent.map(str::to_owned),
            span: span(0, 20),
            parameters: parameters.into_iter().map(str::to_owned).collect(),
            captures: captures.into_iter().map(str::to_owned).collect(),
            is_arrow,
            is_async,
            is_generator: false,
            simple_parameters,
        }
    }

    fn snapshot() -> SemanticSnapshot {
        SemanticSnapshot {
            root_scope: "scope_0".to_owned(),
            scopes: vec![
                scope("scope_0", None, false, true),
                scope("scope_1", Some("scope_0"), true, true),
                scope("scope_2", Some("scope_1"), true, true),
                scope("scope_3", Some("scope_1"), false, false),
                scope("scope_4", Some("scope_1"), true, true),
                scope("scope_5", Some("scope_1"), true, true),
            ],
            bindings: vec![
                binding(
                    "binding_0",
                    "arg",
                    ResolvedBindingKind::Parameter,
                    true,
                    "scope_1",
                ),
                binding(
                    "binding_1",
                    "local",
                    ResolvedBindingKind::Let,
                    true,
                    "scope_3",
                ),
                binding(
                    "binding_2",
                    "value",
                    ResolvedBindingKind::Let,
                    true,
                    "scope_0",
                ),
                binding(
                    "binding_3",
                    "value",
                    ResolvedBindingKind::Let,
                    true,
                    "scope_1",
                ),
            ],
            references: vec![ResolvedReference {
                name: "local".to_owned(),
                span: span(10, 15),
                scope: "scope_2".to_owned(),
                binding: Some("binding_1".to_owned()),
                read: true,
                write: false,
            }],
            functions: vec![
                function(
                    "fn_program",
                    "scope_0",
                    None,
                    vec![],
                    vec![],
                    false,
                    true,
                    false,
                ),
                function(
                    "fn_1",
                    "scope_1",
                    Some("scope_0"),
                    vec!["binding_0"],
                    vec![],
                    false,
                    true,
                    false,
                ),
                function(
                    "fn_2",
                    "scope_2",
                    Some("scope_1"),
                    vec![],
                    vec!["binding_1"],
                    true,
                    true,
                    false,
                ),
            ],
        }
    }

    #[test]
    fn maps_shadowed_declarations_to_distinct_core_ir_bindings() {
        let module = analyze(&source(), &snapshot()).expect("semantic facts are representable");
        let values: Vec<_> = module
            .bindings
            .iter()
            .filter(|binding| binding.ir.name == "value")
            .collect();
        assert_eq!(values.len(), 2);
        assert_ne!(values[0].ir.id, values[1].ir.id);
        assert_ne!(values[0].lexical_scope, values[1].lexical_scope);
        let program = module
            .functions
            .iter()
            .find(|function| function.id == "fn_program")
            .unwrap();
        assert!(
            program
                .environment
                .bindings
                .iter()
                .any(|binding| binding.name == "value")
        );
    }

    #[test]
    fn constructs_mutable_cell_capture_for_an_arrow_function() {
        let module = analyze(&source(), &snapshot()).expect("semantic facts are representable");
        let arrow = module
            .functions
            .iter()
            .find(|function| function.is_arrow)
            .unwrap();
        assert_eq!(arrow.captures.len(), 1);
        assert_eq!(arrow.captures[0].name, "local");
        assert_eq!(arrow.captures[0].mode, CaptureMode::Cell);
        assert!(
            arrow
                .environment
                .bindings
                .iter()
                .all(|binding| binding.name != "local")
        );
    }

    #[test]
    fn rejects_async_and_non_simple_parameter_forms_without_fabricating_ir() {
        let source = source();
        let mut input = snapshot();
        input.functions.push(function(
            "fn_async",
            "scope_4",
            Some("scope_1"),
            vec![],
            vec![],
            false,
            true,
            true,
        ));
        input.functions.push(function(
            "fn_pattern",
            "scope_5",
            Some("scope_1"),
            vec![],
            vec![],
            false,
            false,
            false,
        ));
        let diagnostics = analyze(&source, &input).expect_err("unsupported forms fail closed");
        assert!(
            diagnostics
                .iter()
                .any(|item| item.code == "semantic.unsupported_function_kind")
        );
        assert!(
            diagnostics
                .iter()
                .any(|item| item.code == "semantic.unsupported_parameter_pattern")
        );
    }

    #[test]
    fn rejects_binding_kinds_outside_the_core_ir_environment_model() {
        let mut input = snapshot();
        input.bindings.push(binding(
            "binding_4",
            "type_only",
            ResolvedBindingKind::Other,
            false,
            "scope_0",
        ));
        let diagnostics = analyze(&source(), &input)
            .expect_err("unsupported declaration cannot be silently lowered");
        assert!(
            diagnostics
                .iter()
                .any(|item| item.code == "semantic.unsupported_binding")
        );
    }
}
