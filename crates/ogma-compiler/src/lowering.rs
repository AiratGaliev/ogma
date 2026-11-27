//! Canonical AST/semantic to Core IR v1 lowering.
//!
//! The compiler consumes the frontend-owned syntax projection. Oxc types and
//! lifetimes do not cross this crate boundary; emitted documents are verified
//! against the canonical Core IR v1 model before they are returned.

use std::collections::{BTreeMap, HashMap, HashSet};

use ogma_core_ir::{
    BasicBlock, Binding, BindingKind, BlockParameter, Capture, CaptureMode, Edge, EffectDefinition,
    Effects, ExceptionFlow, Function, FunctionDocument, Instruction, Representation,
    ReturnSignature, SemanticType, Source, SourceFile as CoreSourceFile, SourceLanguage,
    SourcePosition as CoreSourcePosition, Terminator, UnreachableReason, ValueDefinition,
    verify_document,
};
use ogma_frontend::{
    ResolvedSyntaxFunction, ResolvedSyntaxProgram, SourceFile, SourceSpan, SyntaxAssignmentTarget,
    SyntaxBinaryOperator, SyntaxExpression, SyntaxForInitializer, SyntaxLiteral, SyntaxMemberKey,
    SyntaxStatement, SyntaxUnaryOperator, SyntaxVariableKind,
};
use serde_json::{Value as JsonValue, json};

use crate::semantic::{FunctionSemantics, SemanticModule};

/// A source-positioned diagnostic produced by canonical Core IR lowering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoweringDiagnostic {
    /// Stable diagnostic identifier.
    pub code: &'static str,
    /// Function being lowered.
    pub function: String,
    /// Source range, when the error maps to user-authored syntax.
    pub span: Option<SourceSpan>,
    /// Human-readable explanation.
    pub message: String,
}

/// Lower a parsed and semantically resolved source unit into one verified
/// Core IR v1 document per Program/function.
///
/// Unsupported syntax, unrepresentable binding lifetimes and verifier
/// failures return structured diagnostics instead of guessed IR.
pub fn lower(
    source: &SourceFile,
    semantic: &SemanticModule,
    syntax: &ResolvedSyntaxProgram,
) -> Result<Vec<FunctionDocument>, Vec<LoweringDiagnostic>> {
    if semantic.source_id != source.id().as_str()
        || semantic.source_sha256 != source.content_sha256()
    {
        return Err(vec![LoweringDiagnostic {
            code: "lowering.source_identity_mismatch",
            function: "<module>".to_owned(),
            span: None,
            message:
                "semantic analysis was produced for a different source identity or content revision"
                    .to_owned(),
        }]);
    }

    let mut semantic_by_id = HashMap::with_capacity(semantic.functions.len());
    for function in &semantic.functions {
        if semantic_by_id
            .insert(function.id.as_str(), function)
            .is_some()
        {
            return Err(vec![LoweringDiagnostic {
                code: "lowering.duplicate_semantic_function",
                function: function.id.clone(),
                span: Some(function.span),
                message: "semantic module contains duplicate function identities".to_owned(),
            }]);
        }
    }

    let source_file = match core_source_file(source) {
        Ok(file) => file,
        Err(diagnostic) => return Err(vec![diagnostic]),
    };
    let syntax_by_id: HashMap<_, _> = syntax
        .functions
        .iter()
        .map(|function| (function.id.as_str(), function))
        .collect();
    let mut documents = Vec::with_capacity(syntax.functions.len());
    let mut diagnostics = Vec::new();
    let mut seen = HashSet::new();

    for syntax_function in &syntax.functions {
        if !seen.insert(syntax_function.id.as_str()) {
            diagnostics.push(diag(
                syntax_function,
                "lowering.duplicate_function",
                Some(syntax_function.span),
                "duplicate syntax function identity",
            ));
            continue;
        }
        let Some(function_semantics) = semantic_by_id.get(syntax_function.id.as_str()).copied()
        else {
            diagnostics.push(diag(
                syntax_function,
                "lowering.function_semantics_missing",
                Some(syntax_function.span),
                "syntax function has no matching semantic environment",
            ));
            continue;
        };
        let mut compiler = match FunctionCompiler::new(
            &source_file,
            semantic,
            &syntax_by_id,
            function_semantics,
            syntax_function,
        ) {
            Ok(compiler) => compiler,
            Err(error) => {
                diagnostics.push(error);
                continue;
            }
        };
        match compiler.compile() {
            Ok(document) => {
                for item in verify_document(&document) {
                    diagnostics.push(LoweringDiagnostic {
                        code: item.code,
                        function: syntax_function.id.clone(),
                        span: None,
                        message: format!("{}: {}", item.path, item.message),
                    });
                }
                if diagnostics.is_empty() {
                    documents.push(document);
                }
            }
            Err(error) => diagnostics.push(error),
        }
    }

    if diagnostics.is_empty() {
        let missing: Vec<_> = semantic_by_id
            .keys()
            .filter(|id| !seen.contains(**id))
            .copied()
            .collect();
        if !missing.is_empty() {
            diagnostics.push(LoweringDiagnostic {
                code: "lowering.syntax_function_missing",
                function: "<module>".to_owned(),
                span: None,
                message: format!(
                    "semantic model contains functions without syntax units: {}",
                    missing.join(", ")
                ),
            });
        }
    }

    if diagnostics.is_empty() {
        Ok(documents)
    } else {
        Err(diagnostics)
    }
}

fn diag(
    syntax: &ResolvedSyntaxFunction,
    code: &'static str,
    span: Option<SourceSpan>,
    message: impl Into<String>,
) -> LoweringDiagnostic {
    LoweringDiagnostic {
        code,
        function: syntax.id.clone(),
        span,
        message: message.into(),
    }
}

fn valid_core_id(id: &str) -> bool {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    id.len() <= 128
        && (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | ':' | '-'))
}

fn core_source_file(source: &SourceFile) -> Result<CoreSourceFile, LoweringDiagnostic> {
    let uri = source.uri();
    let mut path = if let Some(rest) = uri.strip_prefix("memory://") {
        rest.trim_start_matches('/').to_owned()
    } else if uri.contains("://") {
        return Err(LoweringDiagnostic {
            code: "lowering.source_path",
            function: "<module>".to_owned(),
            span: None,
            message: format!(
                "source URI {uri:?} is not a repository-relative path; only memory URIs and plain relative paths can be lowered"
            ),
        });
    } else {
        uri.to_owned()
    };
    while path.starts_with("./") {
        path.drain(..2);
    }
    let invalid = path.is_empty()
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains('\\')
        || path.contains(':')
        || path
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..");
    let digest = source.content_sha256();
    let source_id = source.id().as_str();
    if !valid_core_id(source_id) {
        return Err(LoweringDiagnostic {
            code: "lowering.source_identity",
            function: "<module>".to_owned(),
            span: None,
            message: format!("source identity {source_id:?} is not a valid Core IR identity"),
        });
    }
    if invalid {
        return Err(LoweringDiagnostic {
            code: "lowering.source_path",
            function: "<module>".to_owned(),
            span: None,
            message: format!(
                "source URI {uri:?} is not a normalized repository-relative POSIX path"
            ),
        });
    }
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(LoweringDiagnostic {
            code: "lowering.source_digest",
            function: "<module>".to_owned(),
            span: None,
            message: "frontend source digest is not a SHA-256 hexadecimal string".to_owned(),
        });
    }
    Ok(CoreSourceFile {
        id: source_id.to_owned(),
        path,
        language: SourceLanguage::Javascript,
        sha256: Some(digest.to_owned()),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ValueType {
    semantic: SemanticType,
    representation: Representation,
}

impl ValueType {
    const DYNAMIC: Self = Self {
        semantic: SemanticType::Dynamic,
        representation: Representation::Boxed,
    };
    const BOOLEAN: Self = Self {
        semantic: SemanticType::Boolean,
        representation: Representation::I1,
    };
    const NUMBER: Self = Self {
        semantic: SemanticType::Number,
        representation: Representation::F64,
    };
    const STRING: Self = Self {
        semantic: SemanticType::String,
        representation: Representation::StringRef,
    };
    const FUNCTION: Self = Self {
        semantic: SemanticType::Function,
        representation: Representation::FunctionRef,
    };
}

#[derive(Clone, Debug)]
struct ValueRef {
    id: String,
    ty: ValueType,
}

#[derive(Clone, Debug)]
struct Cursor {
    block: usize,
    effect: Option<String>,
}

#[derive(Clone, Debug)]
enum EvaluatedMemberKey {
    Static(String),
    Computed(ValueRef),
}

#[derive(Clone, Debug)]
enum EvaluatedAssignmentTarget {
    Binding(String),
    Member {
        object: ValueRef,
        key: EvaluatedMemberKey,
    },
}
/// A member expression remains a Reference when wrapped in parentheses, so
/// calls through `(object.method)()` must retain the original receiver.
fn member_call_reference(
    expression: &SyntaxExpression,
) -> Option<(&SyntaxExpression, &SyntaxMemberKey, SourceSpan)> {
    match expression {
        SyntaxExpression::Member { object, key, span } => Some((object, key, *span)),
        SyntaxExpression::Parenthesized { expression, .. } => member_call_reference(expression),
        _ => None,
    }
}

fn collect_reentered_lexical_scopes(
    statements: &[SyntaxStatement],
    inside_loop_body: bool,
    output: &mut HashSet<String>,
) {
    for statement in statements {
        match statement {
            SyntaxStatement::Block { scope, body, .. } => {
                if inside_loop_body && let Some(scope) = scope {
                    output.insert(scope.clone());
                }
                collect_reentered_lexical_scopes(body, inside_loop_body, output);
            }
            SyntaxStatement::If {
                consequent,
                alternate,
                ..
            } => {
                collect_reentered_lexical_scopes(
                    std::slice::from_ref(consequent),
                    inside_loop_body,
                    output,
                );
                if let Some(alternate) = alternate {
                    collect_reentered_lexical_scopes(
                        std::slice::from_ref(alternate),
                        inside_loop_body,
                        output,
                    );
                }
            }
            SyntaxStatement::While { body, .. } => {
                collect_reentered_lexical_scopes(std::slice::from_ref(body), true, output);
            }
            SyntaxStatement::For { scope, body, .. } => {
                if inside_loop_body && let Some(scope) = scope {
                    output.insert(scope.clone());
                }
                collect_reentered_lexical_scopes(std::slice::from_ref(body), true, output);
            }
            _ => {}
        }
    }
}

struct FunctionCompiler<'a> {
    source_file: &'a CoreSourceFile,
    semantic_module: &'a SemanticModule,
    syntax_registry: &'a HashMap<&'a str, &'a ResolvedSyntaxFunction>,
    semantic: &'a FunctionSemantics,
    syntax: &'a ResolvedSyntaxFunction,
    local_bindings: HashMap<String, Binding>,
    captures_by_binding: HashMap<String, Capture>,
    const_initializers: HashMap<String, SyntaxExpression>,
    const_values: HashMap<String, ValueRef>,
    promise_bindings: HashSet<String>,
    hoisted_functions: HashSet<String>,
    blocks: Vec<BasicBlock>,
    current: Option<usize>,
    current_effect: Option<String>,
    effect_enabled: bool,
    next_value: u64,
    next_instruction: u64,
    next_effect: u64,
    next_block: u64,
}

impl<'a> FunctionCompiler<'a> {
    fn new(
        source_file: &'a CoreSourceFile,
        semantic_module: &'a SemanticModule,
        syntax_registry: &'a HashMap<&'a str, &'a ResolvedSyntaxFunction>,
        semantic: &'a FunctionSemantics,
        syntax: &'a ResolvedSyntaxFunction,
    ) -> Result<Self, LoweringDiagnostic> {
        if syntax.is_async || syntax.is_generator || semantic.is_async || semantic.is_generator {
            return Err(diag(
                syntax,
                "lowering.unsupported_function_kind",
                Some(syntax.span),
                "async and generator suspension semantics are outside the synchronous M1 lowering subset",
            ));
        }

        let local_bindings = semantic
            .environment
            .bindings
            .iter()
            .cloned()
            .map(|binding| (binding.id.clone(), binding))
            .collect();
        let captures_by_binding = semantic
            .captures
            .iter()
            .cloned()
            .map(|capture| (capture.binding.clone(), capture))
            .collect();

        // Core IR v1 exposes a function environment but has no operation for
        // entering/resetting a nested lexical environment. A mutable binding
        // in a loop-reentered block would retain stale initialized state and
        // could bypass TDZ on the next entry. Single-entry block bindings can
        // be flattened when they are not captured; captured block bindings
        // are rejected separately because their environment must outlive the
        // block. A for-header scope nested inside a re-entered loop body must
        // also be rejected; only a non-re-entered for-header lifetime is safe
        // to flatten without allocating a fresh environment.
        let mut reentered_block_scopes = HashSet::new();
        collect_reentered_lexical_scopes(&syntax.body, false, &mut reentered_block_scopes);
        for binding in &semantic.environment.bindings {
            if binding.kind != BindingKind::Let {
                continue;
            }
            let Some(source_binding) = semantic_module
                .bindings
                .iter()
                .find(|item| item.ir.id == binding.id)
            else {
                return Err(diag(
                    syntax,
                    "lowering.binding_source_missing",
                    Some(syntax.span),
                    format!("binding {:?} has no source lexical scope", binding.id),
                ));
            };
            if reentered_block_scopes.contains(&source_binding.lexical_scope) {
                return Err(diag(
                    syntax,
                    "lowering.block_environment_lifetime",
                    Some(source_binding.declaration),
                    format!(
                        "mutable block-scoped binding {:?} is declared in a loop-reentered block; Core IR v1 lowering cannot reset a fresh lexical environment on each entry",
                        binding.name
                    ),
                ));
            }
        }

        for capture in &semantic.captures {
            let Some(binding) = semantic_module
                .bindings
                .iter()
                .find(|binding| binding.ir.id == capture.binding)
            else {
                return Err(diag(
                    syntax,
                    "lowering.capture_binding_missing",
                    Some(syntax.span),
                    format!("capture {:?} has no source binding", capture.binding),
                ));
            };
            if binding.ir.kind == BindingKind::Import {
                if capture.mode != CaptureMode::Cell || binding.ir.mutable {
                    return Err(diag(
                        syntax,
                        "lowering.import_capture_mode",
                        Some(binding.declaration),
                        format!(
                            "import binding {:?} must be captured as a read-only live reference",
                            binding.ir.name
                        ),
                    ));
                }
                continue;
            }
            if binding.ir.kind == BindingKind::Const {
                if capture.mode != CaptureMode::Value || binding.ir.mutable {
                    return Err(diag(
                        syntax,
                        "lowering.const_capture_mode",
                        Some(binding.declaration),
                        format!(
                            "immutable binding {:?} must use a by-value closure capture",
                            binding.ir.name
                        ),
                    ));
                }
                continue;
            }
            let function_scope = semantic_module
                .functions
                .iter()
                .any(|function| function.environment.id == binding.lexical_scope);
            if !function_scope {
                return Err(diag(
                    syntax,
                    "lowering.block_capture_lifetime",
                    Some(binding.declaration),
                    format!(
                        "capture of mutable block-scoped binding {:?} requires per-block/per-iteration environment materialization",
                        binding.ir.name
                    ),
                ));
            }
        }

        let const_initializers = collect_const_initializers(&syntax.body);
        let effectful_bindings: HashSet<String> = semantic
            .environment
            .bindings
            .iter()
            .filter(|binding| binding.kind != BindingKind::Const)
            .map(|binding| binding.id.clone())
            .chain(
                semantic
                    .captures
                    .iter()
                    .map(|capture| capture.binding.clone()),
            )
            .collect();
        let effect_enabled = !effectful_bindings.is_empty()
            || statements_need_effects(&syntax.body, &effectful_bindings);

        Ok(Self {
            source_file,
            semantic_module,
            syntax_registry,
            semantic,
            syntax,
            local_bindings,
            captures_by_binding,
            const_initializers,
            const_values: HashMap::new(),
            promise_bindings: HashSet::new(),
            hoisted_functions: HashSet::new(),
            blocks: Vec::new(),
            current: None,
            current_effect: None,
            effect_enabled,
            next_value: 0,
            next_instruction: 0,
            next_effect: 0,
            next_block: 0,
        })
    }

    fn compile(&mut self) -> Result<FunctionDocument, LoweringDiagnostic> {
        self.create_entry_block();
        self.emit_parameter_initializers()?;
        self.emit_var_initializers()?;
        let body = self.syntax.body.clone();
        self.emit_hoisted_functions(&body)?;
        self.compile_sequence(&body)?;

        if let Some(cursor) = self.cursor() {
            self.restore_cursor(cursor);
            let undefined = self.emit_undefined("materialize implicit JavaScript return value")?;
            self.terminate_return(
                Some(undefined.id),
                self.generated_source("implicit JavaScript function completion", Vec::new()),
            )?;
        }

        let function = Function {
            id: self.syntax.id.clone(),
            name: self.syntax.name.clone(),
            parameters: self.semantic.parameters.clone(),
            return_signature: self.infer_return_signature()?,
            source_files: vec![self.source_file.clone()],
            environment: self.semantic.environment.clone(),
            captures: self.semantic.captures.clone(),
            entry_block: "entry".to_owned(),
            effect_entry: self.effect_enabled.then(|| "effect_entry".to_owned()),
            blocks: self.blocks.clone(),
        };
        Ok(FunctionDocument {
            contract: "ogma.core-ir".to_owned(),
            version: "1.0.0".to_owned(),
            function,
        })
    }

    fn fail<T>(
        &self,
        code: &'static str,
        span: Option<SourceSpan>,
        message: impl Into<String>,
    ) -> Result<T, LoweringDiagnostic> {
        Err(diag(self.syntax, code, span, message))
    }

    fn create_entry_block(&mut self) {
        let parameters = if self.effect_enabled {
            vec![BlockParameter::Effect(EffectDefinition {
                id: "effect_entry".to_owned(),
                kind: "effect".to_owned(),
            })]
        } else {
            Vec::new()
        };
        self.blocks.push(BasicBlock {
            id: "entry".to_owned(),
            parameters,
            exception_parameter: None,
            instructions: Vec::new(),
            terminator: Terminator::Unreachable {
                reason: UnreachableReason::ProvenUnreachable,
                source: self
                    .generated_source("entry block is finalized during lowering", Vec::new()),
            },
        });
        self.current = Some(0);
        self.current_effect = self.effect_enabled.then(|| "effect_entry".to_owned());
    }

    fn create_block(&mut self) -> usize {
        let id = format!("bb{}", self.next_block);
        self.next_block += 1;
        let effect_id = format!("effect_{id}");
        let parameters = if self.effect_enabled {
            vec![BlockParameter::Effect(EffectDefinition {
                id: effect_id,
                kind: "effect".to_owned(),
            })]
        } else {
            Vec::new()
        };
        self.blocks.push(BasicBlock {
            id,
            parameters,
            exception_parameter: None,
            instructions: Vec::new(),
            terminator: Terminator::Unreachable {
                reason: UnreachableReason::ProvenUnreachable,
                source: self.generated_source(
                    "basic block terminator is finalized during lowering",
                    Vec::new(),
                ),
            },
        });
        self.blocks.len() - 1
    }

    fn block_id(&self, block: usize) -> String {
        self.blocks[block].id.clone()
    }

    fn cursor(&self) -> Option<Cursor> {
        self.current.map(|block| Cursor {
            block,
            effect: self.current_effect.clone(),
        })
    }

    fn restore_cursor(&mut self, cursor: Cursor) {
        self.current = Some(cursor.block);
        self.current_effect = cursor.effect;
    }

    fn select_block(&mut self, block: usize) {
        self.current = Some(block);
        self.current_effect = if self.effect_enabled {
            self.blocks[block]
                .parameters
                .last()
                .and_then(|parameter| match parameter {
                    BlockParameter::Effect(effect) => Some(effect.id.clone()),
                    BlockParameter::Value(_) => None,
                })
        } else {
            None
        };
    }

    fn user_source(&self, span: SourceSpan, bindings: impl IntoIterator<Item = String>) -> Source {
        let mut bindings: Vec<_> = bindings.into_iter().collect();
        bindings.sort();
        bindings.dedup();
        Source::User {
            file: self.source_file.id.clone(),
            start: CoreSourcePosition {
                line: span.start.line,
                column: span.start.column_utf16,
            },
            end: CoreSourcePosition {
                line: span.end.line,
                column: span.end.column_utf16,
            },
            scope: Some(self.semantic.environment.id.clone()),
            bindings,
            suspension: None,
            inline_chain: Vec::new(),
        }
    }

    fn generated_source(&self, reason: &str, bindings: Vec<String>) -> Source {
        Source::Generated {
            reason: reason.to_owned(),
            scope: Some(self.semantic.environment.id.clone()),
            bindings,
            suspension: None,
            inline_chain: Vec::new(),
        }
    }

    fn next_value_id(&mut self) -> String {
        let id = format!("v{}", self.next_value);
        self.next_value += 1;
        id
    }

    fn next_instruction_id(&mut self) -> String {
        let id = format!("i{}", self.next_instruction);
        self.next_instruction += 1;
        id
    }

    fn next_effect_id(&mut self) -> String {
        let id = format!("e{}", self.next_effect);
        self.next_effect += 1;
        id
    }

    fn emit_instruction(
        &mut self,
        op: &str,
        operands: Vec<String>,
        result_type: Option<ValueType>,
        attributes: BTreeMap<String, JsonValue>,
        effects: Effects,
        source: Source,
    ) -> Result<Option<ValueRef>, LoweringDiagnostic> {
        let Some(block) = self.current else {
            return self.fail(
                "lowering.instruction_without_block",
                None,
                format!("cannot emit {op:?} after a terminating instruction"),
            );
        };
        let (effect_in, effect_out) = if effects.is_effectful() {
            let Some(current) = self.current_effect.clone() else {
                return self.fail(
                    "lowering.effect_context_missing",
                    None,
                    format!("effectful operation {op:?} has no current effect token"),
                );
            };
            (Some(current), Some(self.next_effect_id()))
        } else {
            (None, None)
        };
        let result = result_type.map(|ty| ValueRef {
            id: self.next_value_id(),
            ty,
        });
        let instruction_id = self.next_instruction_id();
        let instruction = Instruction {
            id: instruction_id,
            op: op.to_owned(),
            operands,
            result: result.as_ref().map(|value| ValueDefinition {
                id: value.id.clone(),
                kind: "value".to_owned(),
                semantic_type: value.ty.semantic,
                representation: value.ty.representation,
            }),
            attributes,
            effects,
            source,
            effect_in,
            effect_out: effect_out.clone(),
            exception: effects.may_throw.then_some(ExceptionFlow::Propagate),
        };
        self.blocks[block].instructions.push(instruction);
        if let Some(next) = effect_out {
            self.current_effect = Some(next);
        }
        Ok(result)
    }

    fn emit_constant(
        &mut self,
        literal: &str,
        value: JsonValue,
        ty: ValueType,
        span: Option<SourceSpan>,
    ) -> Result<ValueRef, LoweringDiagnostic> {
        let mut attributes = BTreeMap::new();
        attributes.insert("literal".to_owned(), JsonValue::String(literal.to_owned()));
        attributes.insert("value".to_owned(), value);
        let source = span
            .map(|span| self.user_source(span, Vec::new()))
            .unwrap_or_else(|| {
                self.generated_source("materialize JavaScript constant", Vec::new())
            });
        self.emit_instruction(
            "js.constant",
            Vec::new(),
            Some(ty),
            attributes,
            Effects::default(),
            source,
        )?
        .ok_or_else(|| {
            diag(
                self.syntax,
                "lowering.constant_result",
                span,
                "js.constant produced no value",
            )
        })
    }

    fn emit_undefined(&mut self, reason: &str) -> Result<ValueRef, LoweringDiagnostic> {
        self.emit_constant(
            "undefined",
            JsonValue::Null,
            ValueType {
                semantic: SemanticType::Undefined,
                representation: Representation::Boxed,
            },
            None,
        )
        .map_err(|mut error| {
            error.message = format!("{reason}: {}", error.message);
            error
        })
    }

    fn emit_env_load(
        &mut self,
        binding_id: &str,
        span: SourceSpan,
    ) -> Result<ValueRef, LoweringDiagnostic> {
        let (alias, ty) = if let Some(binding) = self.local_bindings.get(binding_id) {
            (
                binding.id.clone(),
                ValueType {
                    semantic: binding.semantic_type,
                    representation: binding.representation,
                },
            )
        } else if let Some(capture) = self.captures_by_binding.get(binding_id) {
            (
                capture.id.clone(),
                ValueType {
                    semantic: capture.semantic_type,
                    representation: capture.representation,
                },
            )
        } else {
            return self.fail(
                "lowering.binding_not_in_environment",
                Some(span),
                format!("binding {binding_id:?} is neither local nor captured"),
            );
        };
        let mut attributes = BTreeMap::new();
        attributes.insert("binding".to_owned(), JsonValue::String(alias));
        self.emit_instruction(
            "env.load",
            Vec::new(),
            Some(ty),
            attributes,
            Effects {
                may_throw: true,
                ..Effects::default()
            },
            self.user_source(span, [binding_id.to_owned()]),
        )?
        .ok_or_else(|| {
            diag(
                self.syntax,
                "lowering.env_load_result",
                Some(span),
                "env.load produced no value",
            )
        })
    }

    fn emit_env_store(
        &mut self,
        binding_id: &str,
        value: ValueRef,
        span: Option<SourceSpan>,
        generated_reason: Option<&str>,
    ) -> Result<ValueRef, LoweringDiagnostic> {
        let (alias, mutable, capture_value) =
            if let Some(binding) = self.local_bindings.get(binding_id) {
                (binding.id.clone(), binding.mutable, false)
            } else if let Some(capture) = self.captures_by_binding.get(binding_id) {
                (
                    capture.id.clone(),
                    capture.mutable,
                    capture.mode == CaptureMode::Value,
                )
            } else {
                return self.fail(
                    "lowering.binding_not_in_environment",
                    span,
                    format!("binding {binding_id:?} is neither local nor captured"),
                );
            };
        if !mutable || capture_value {
            return self.fail(
                "lowering.immutable_store",
                span,
                format!("cannot assign to immutable binding {binding_id:?}"),
            );
        }
        let mut attributes = BTreeMap::new();
        attributes.insert("binding".to_owned(), JsonValue::String(alias));
        let source = match (span, generated_reason) {
            (Some(span), _) => self.user_source(span, [binding_id.to_owned()]),
            (None, Some(reason)) => self.generated_source(reason, vec![binding_id.to_owned()]),
            (None, None) => {
                self.generated_source("initialize lexical binding", vec![binding_id.to_owned()])
            }
        };
        self.emit_instruction(
            "env.store",
            vec![value.id],
            Some(value.ty),
            attributes,
            Effects {
                may_throw: true,
                ..Effects::default()
            },
            source,
        )?
        .ok_or_else(|| {
            diag(
                self.syntax,
                "lowering.env_store_result",
                span,
                "env.store produced no assigned value",
            )
        })
    }
    fn attach_binding_to_value_source(
        &mut self,
        value_id: &str,
        binding_id: &str,
        span: SourceSpan,
    ) -> Result<(), LoweringDiagnostic> {
        for block in &mut self.blocks {
            for instruction in &mut block.instructions {
                if instruction
                    .result
                    .as_ref()
                    .is_some_and(|result| result.id == value_id)
                {
                    let bindings = match &mut instruction.source {
                        Source::User { bindings, .. } | Source::Generated { bindings, .. } => {
                            bindings
                        }
                    };
                    bindings.push(binding_id.to_owned());
                    bindings.sort();
                    bindings.dedup();
                    return Ok(());
                }
            }
        }
        self.fail(
            "lowering.const_definition_missing",
            Some(span),
            format!(
                "const binding {binding_id:?} has no SSA definition {value_id:?} to carry its source provenance"
            ),
        )
    }

    fn emit_to_boolean(
        &mut self,
        value: ValueRef,
        span: SourceSpan,
    ) -> Result<ValueRef, LoweringDiagnostic> {
        if value.ty.semantic == SemanticType::Boolean {
            return Ok(value);
        }
        self.emit_instruction(
            "js.to_boolean",
            vec![value.id],
            Some(ValueType::BOOLEAN),
            BTreeMap::new(),
            Effects::default(),
            self.user_source(span, Vec::new()),
        )?
        .ok_or_else(|| {
            diag(
                self.syntax,
                "lowering.to_boolean_result",
                Some(span),
                "js.to_boolean produced no value",
            )
        })
    }

    fn emit_binary(
        &mut self,
        op: &str,
        left: ValueRef,
        right: ValueRef,
        ty: ValueType,
        effects: Effects,
        span: SourceSpan,
    ) -> Result<ValueRef, LoweringDiagnostic> {
        self.emit_instruction(
            op,
            vec![left.id, right.id],
            Some(ty),
            BTreeMap::new(),
            effects,
            self.user_source(span, Vec::new()),
        )?
        .ok_or_else(|| {
            diag(
                self.syntax,
                "lowering.binary_result",
                Some(span),
                format!("{op} produced no value"),
            )
        })
    }

    fn emit_member_get(
        &mut self,
        object: ValueRef,
        key: &SyntaxMemberKey,
        span: SourceSpan,
    ) -> Result<ValueRef, LoweringDiagnostic> {
        match key {
            SyntaxMemberKey::Static(key) => {
                let mut attributes = BTreeMap::new();
                attributes.insert("key".to_owned(), JsonValue::String(key.clone()));
                self.emit_instruction(
                    "js.get_property",
                    vec![object.id],
                    Some(ValueType::DYNAMIC),
                    attributes,
                    dynamic_effects(),
                    self.user_source(span, Vec::new()),
                )?
                .ok_or_else(|| {
                    diag(
                        self.syntax,
                        "lowering.property_result",
                        Some(span),
                        "js.get_property produced no value",
                    )
                })
            }
            SyntaxMemberKey::Computed(key) => {
                let key = self.emit_expression(key)?;
                self.emit_instruction(
                    "js.get_element",
                    vec![object.id, key.id],
                    Some(ValueType::DYNAMIC),
                    BTreeMap::new(),
                    dynamic_effects(),
                    self.user_source(span, Vec::new()),
                )?
                .ok_or_else(|| {
                    diag(
                        self.syntax,
                        "lowering.element_result",
                        Some(span),
                        "js.get_element produced no value",
                    )
                })
            }
        }
    }

    fn emit_member_set(
        &mut self,
        object: ValueRef,
        key: EvaluatedMemberKey,
        value: ValueRef,
        span: SourceSpan,
    ) -> Result<ValueRef, LoweringDiagnostic> {
        match key {
            EvaluatedMemberKey::Static(key) => {
                let mut attributes = BTreeMap::new();
                attributes.insert("key".to_owned(), JsonValue::String(key.clone()));
                self.emit_instruction(
                    "js.set_property",
                    vec![object.id, value.id.clone()],
                    Some(value.ty),
                    attributes,
                    dynamic_effects(),
                    self.user_source(span, Vec::new()),
                )?
                .ok_or_else(|| {
                    diag(
                        self.syntax,
                        "lowering.set_property_result",
                        Some(span),
                        "js.set_property produced no value",
                    )
                })
            }
            EvaluatedMemberKey::Computed(key) => self
                .emit_instruction(
                    "js.set_element",
                    vec![object.id, key.id, value.id.clone()],
                    Some(value.ty),
                    BTreeMap::new(),
                    dynamic_effects(),
                    self.user_source(span, Vec::new()),
                )?
                .ok_or_else(|| {
                    diag(
                        self.syntax,
                        "lowering.set_element_result",
                        Some(span),
                        "js.set_element produced no value",
                    )
                }),
        }
    }

    fn emit_closure(
        &mut self,
        child_id: &str,
        span: SourceSpan,
    ) -> Result<ValueRef, LoweringDiagnostic> {
        let Some(child) = self
            .semantic_module
            .functions
            .iter()
            .find(|function| function.id == child_id)
        else {
            return self.fail(
                "lowering.closure_function_missing",
                Some(span),
                format!("closure target {child_id:?} has no semantic function"),
            );
        };
        let Some(child_syntax) = self.syntax_registry.get(child_id).copied() else {
            return self.fail(
                "lowering.closure_syntax_missing",
                Some(span),
                format!("closure target {child_id:?} has no syntax unit"),
            );
        };
        if child_syntax.has_private_self_name || child.is_async || child.is_generator {
            return self.fail(
                "lowering.unsupported_closure_kind",
                Some(span),
                format!("closure target {child_id:?} uses an unsupported function form"),
            );
        }

        let mut capture_descriptors = Vec::with_capacity(child.captures.len());
        let mut source_bindings = Vec::with_capacity(child.captures.len());
        for capture in &child.captures {
            let alias = if let Some(binding) = self.local_bindings.get(&capture.binding) {
                let Some(source_binding) = self
                    .semantic_module
                    .bindings
                    .iter()
                    .find(|item| item.ir.id == binding.id)
                else {
                    return self.fail(
                        "lowering.capture_source_missing",
                        Some(span),
                        format!("no source metadata for captured binding {:?}", binding.id),
                    );
                };
                if binding.kind == BindingKind::Import {
                    if capture.mode != CaptureMode::Cell {
                        return self.fail(
                            "lowering.import_capture_mode",
                            Some(span),
                            format!(
                                "import binding {:?} must remain a live reference",
                                binding.name
                            ),
                        );
                    }
                    binding.id.clone()
                } else if binding.kind == BindingKind::Const {
                    if capture.mode != CaptureMode::Value {
                        return self.fail(
                            "lowering.const_capture_mode",
                            Some(span),
                            format!(
                                "immutable const binding {:?} requires by-value capture",
                                binding.name
                            ),
                        );
                    }
                    if !self.const_values.contains_key(&capture.binding) {
                        return self.fail(
                            "lowering.const_capture_tdz",
                            Some(source_binding.declaration),
                            format!(
                                "const binding {:?} is not initialized when the closure is created; by-value capture would violate TDZ semantics",
                                binding.name
                            ),
                        );
                    }
                    binding.id.clone()
                } else {
                    if source_binding.lexical_scope != self.semantic.environment.id {
                        return self.fail(
                            "lowering.block_capture_lifetime",
                            Some(source_binding.declaration),
                            format!("captured mutable binding {:?} is not owned by the enclosing function environment", binding.name),
                        );
                    }
                    if capture.mode != CaptureMode::Cell {
                        return self.fail(
                            "lowering.mutable_capture_mode",
                            Some(source_binding.declaration),
                            format!(
                                "mutable binding {:?} must be captured by shared cell",
                                binding.name
                            ),
                        );
                    }
                    binding.id.clone()
                }
            } else if let Some(parent_capture) = self.captures_by_binding.get(&capture.binding) {
                parent_capture.id.clone()
            } else {
                return self.fail(
                    "lowering.capture_unavailable",
                    Some(span),
                    format!(
                        "enclosing function cannot provide captured binding {:?}",
                        capture.binding
                    ),
                );
            };
            let mode = match capture.mode {
                CaptureMode::Cell => "cell",
                CaptureMode::Value => "value",
            };
            capture_descriptors.push(json!({ "binding": alias, "mode": mode }));
            source_bindings.push(capture.binding.clone());
        }
        let mut attributes = BTreeMap::new();
        attributes.insert(
            "function".to_owned(),
            JsonValue::String(child_id.to_owned()),
        );
        attributes.insert("captures".to_owned(), JsonValue::Array(capture_descriptors));
        self.emit_instruction(
            "closure.create",
            Vec::new(),
            Some(ValueType::FUNCTION),
            attributes,
            Effects {
                may_allocate: true,
                may_gc: true,
                ..Effects::default()
            },
            self.user_source(span, source_bindings),
        )?
        .ok_or_else(|| {
            diag(
                self.syntax,
                "lowering.closure_result",
                Some(span),
                "closure.create produced no function value",
            )
        })
    }

    fn is_promise_expression(&self, expression: &SyntaxExpression) -> bool {
        match expression {
            SyntaxExpression::Parenthesized { expression, .. } => {
                self.is_promise_expression(expression)
            }
            SyntaxExpression::Identifier {
                binding: Some(binding),
                ..
            } => self.promise_bindings.contains(binding),
            SyntaxExpression::Call { callee, .. } => match callee.as_ref() {
                SyntaxExpression::Member {
                    object,
                    key: SyntaxMemberKey::Static(method),
                    ..
                } if matches!(
                    object.as_ref(),
                    SyntaxExpression::Identifier {
                        name,
                        binding: None,
                        ..
                    } if name == "Promise"
                ) && matches!(method.as_str(), "resolve" | "reject") =>
                {
                    true
                }
                SyntaxExpression::Member {
                    object,
                    key: SyntaxMemberKey::Static(method),
                    ..
                } if matches!(method.as_str(), "then" | "catch") => {
                    self.is_promise_expression(object)
                }
                _ => false,
            },
            _ => false,
        }
    }

    fn emit_expression(
        &mut self,
        expression: &SyntaxExpression,
    ) -> Result<ValueRef, LoweringDiagnostic> {
        match expression {
            SyntaxExpression::Literal { value, span } => self.emit_literal(value, *span),
            SyntaxExpression::Identifier {
                name,
                binding,
                span,
            } => {
                if let Some(binding) = binding {
                    if let Some(value) = self.const_values.get(binding) {
                        return Ok(value.clone());
                    }
                    if self
                        .local_bindings
                        .get(binding)
                        .is_some_and(|item| item.kind == BindingKind::Const)
                    {
                        return self.fail(
                            "lowering.const_tdz",
                            Some(*span),
                            format!("const binding {name:?} is read before its declaration initializer has been lowered"),
                        );
                    }
                    return self.emit_env_load(binding, *span);
                }
                if name == "undefined" {
                    return self.emit_undefined("materialize unresolved global undefined");
                }
                self.fail(
                    "lowering.unresolved_global",
                    Some(*span),
                    format!(
                        "unresolved global {name:?} has no Core IR host/global binding contract"
                    ),
                )
            }
            SyntaxExpression::Parenthesized { expression, .. } => self.emit_expression(expression),
            SyntaxExpression::Binary {
                operator,
                left,
                right,
                span,
            } => {
                let left = self.emit_expression(left)?;
                let right = self.emit_expression(right)?;
                match operator {
                    SyntaxBinaryOperator::Add => self.emit_binary(
                        "js.add",
                        left,
                        right,
                        ValueType::DYNAMIC,
                        dynamic_effects(),
                        *span,
                    ),
                    SyntaxBinaryOperator::StrictEqual => self.emit_binary(
                        "js.strict_equal",
                        left,
                        right,
                        ValueType::BOOLEAN,
                        Effects::default(),
                        *span,
                    ),
                    SyntaxBinaryOperator::StrictNotEqual => {
                        let equal = self.emit_binary(
                            "js.strict_equal",
                            left,
                            right,
                            ValueType::BOOLEAN,
                            Effects::default(),
                            *span,
                        )?;
                        let false_value =
                            self.emit_literal(&SyntaxLiteral::Boolean(false), *span)?;
                        self.emit_binary(
                            "js.strict_equal",
                            equal,
                            false_value,
                            ValueType::BOOLEAN,
                            Effects::default(),
                            *span,
                        )
                    }
                    SyntaxBinaryOperator::LessThan => self.emit_binary(
                        "js.less_than",
                        left,
                        right,
                        ValueType::BOOLEAN,
                        dynamic_effects(),
                        *span,
                    ),
                    SyntaxBinaryOperator::GreaterThan => self.emit_binary(
                        "js.greater_than",
                        left,
                        right,
                        ValueType::BOOLEAN,
                        dynamic_effects(),
                        *span,
                    ),
                }
            }
            SyntaxExpression::Unary {
                operator,
                argument,
                span,
            } => match operator {
                SyntaxUnaryOperator::LogicalNot => {
                    let value = self.emit_expression(argument)?;
                    let boolean = self.emit_to_boolean(value, *span)?;
                    let false_value = self.emit_literal(&SyntaxLiteral::Boolean(false), *span)?;
                    self.emit_binary(
                        "js.strict_equal",
                        boolean,
                        false_value,
                        ValueType::BOOLEAN,
                        Effects::default(),
                        *span,
                    )
                }
                SyntaxUnaryOperator::Typeof => {
                    // ECMAScript's typeof operator is the sole operation that
                    // may inspect an unresolved identifier without throwing.
                    // Do not route this case through ordinary identifier
                    // lowering, which correctly rejects ambient globals.
                    if let SyntaxExpression::Identifier {
                        name,
                        binding: None,
                        ..
                    } = argument.as_ref()
                        && name != "undefined"
                    {
                        return self.emit_constant(
                            "string",
                            JsonValue::String("undefined".to_owned()),
                            ValueType::STRING,
                            Some(*span),
                        );
                    }
                    let value = self.emit_expression(argument)?;
                    self.emit_instruction(
                        "js.typeof",
                        vec![value.id],
                        Some(ValueType::STRING),
                        BTreeMap::new(),
                        Effects::default(),
                        self.user_source(*span, Vec::new()),
                    )?
                    .ok_or_else(|| {
                        diag(
                            self.syntax,
                            "lowering.typeof_result",
                            Some(*span),
                            "js.typeof produced no value",
                        )
                    })
                }
                SyntaxUnaryOperator::Positive => {
                    if let SyntaxExpression::Literal {
                        value: SyntaxLiteral::Number(number),
                        ..
                    } = argument.as_ref()
                    {
                        return self.emit_literal(&SyntaxLiteral::Number(*number), *span);
                    }
                    let value = self.emit_expression(argument)?;
                    self.emit_instruction(
                        "js.to_number",
                        vec![value.id],
                        Some(ValueType::NUMBER),
                        BTreeMap::new(),
                        dynamic_effects(),
                        self.user_source(*span, Vec::new()),
                    )?
                    .ok_or_else(|| {
                        diag(
                            self.syntax,
                            "lowering.to_number_result",
                            Some(*span),
                            "js.to_number produced no value",
                        )
                    })
                }
                SyntaxUnaryOperator::Negate => {
                    if let SyntaxExpression::Literal {
                        value: SyntaxLiteral::Number(number),
                        ..
                    } = argument.as_ref()
                    {
                        return self.emit_literal(&SyntaxLiteral::Number(-*number), *span);
                    }
                    self.fail(
                        "lowering.unary_negation_unsupported",
                        Some(*span),
                        "dynamic unary negation requires a dedicated Core IR operation",
                    )
                }
            },
            SyntaxExpression::Call {
                callee,
                arguments,
                span,
            } => {
                let static_promise_method = match callee.as_ref() {
                    SyntaxExpression::Member {
                        object,
                        key: SyntaxMemberKey::Static(name),
                        ..
                    } if matches!(
                        object.as_ref(),
                        SyntaxExpression::Identifier {
                            name: object_name,
                            binding: None,
                            ..
                        } if object_name == "Promise"
                    ) =>
                    {
                        Some(name.as_str())
                    }
                    _ => None,
                };
                if let Some(method @ ("resolve" | "reject")) = static_promise_method {
                    let value = if let Some(argument) = arguments.first() {
                        self.emit_expression(argument)?
                    } else {
                        self.emit_undefined("materialize missing Promise static-method argument")?
                    };
                    let operation = if method == "resolve" {
                        "js.promise_resolve"
                    } else {
                        "js.promise_reject"
                    };
                    return self
                        .emit_instruction(
                            operation,
                            vec![value.id],
                            Some(ValueType::DYNAMIC),
                            BTreeMap::new(),
                            dynamic_effects(),
                            self.user_source(*span, Vec::new()),
                        )?
                        .ok_or_else(|| {
                            diag(
                                self.syntax,
                                "lowering.promise_result",
                                Some(*span),
                                "Promise intrinsic produced no value",
                            )
                        });
                }

                let member_promise_method = match callee.as_ref() {
                    SyntaxExpression::Member {
                        object,
                        key: SyntaxMemberKey::Static(name),
                        span: member_span,
                    } if (name == "then" || name == "catch")
                        && self.is_promise_expression(object) =>
                    {
                        Some((object.as_ref(), name.as_str(), *member_span))
                    }
                    _ => None,
                };
                if let Some((object, method, _member_span)) = member_promise_method {
                    let promise = self.emit_expression(object)?;
                    let (fulfilled, rejected) = if method == "catch" {
                        let undefined_fulfilled = self
                            .emit_undefined("materialize missing Promise fulfillment reaction")?;
                        let rejected = if let Some(argument) = arguments.first() {
                            self.emit_expression(argument)?
                        } else {
                            self.emit_undefined("materialize missing Promise rejection reaction")?
                        };
                        (undefined_fulfilled, rejected)
                    } else {
                        let fulfilled = if let Some(argument) = arguments.first() {
                            self.emit_expression(argument)?
                        } else {
                            self.emit_undefined("materialize missing Promise fulfillment reaction")?
                        };
                        let rejected = if let Some(argument) = arguments.get(1) {
                            self.emit_expression(argument)?
                        } else {
                            self.emit_undefined("materialize missing Promise rejection reaction")?
                        };
                        (fulfilled, rejected)
                    };
                    return self
                        .emit_instruction(
                            "js.promise_then",
                            vec![promise.id, fulfilled.id, rejected.id],
                            Some(ValueType::DYNAMIC),
                            BTreeMap::new(),
                            dynamic_effects(),
                            self.user_source(*span, Vec::new()),
                        )?
                        .ok_or_else(|| {
                            diag(
                                self.syntax,
                                "lowering.promise_result",
                                Some(*span),
                                "Promise.then produced no child Promise",
                            )
                        });
                }

                let (callee_value, this_value) = if let Some((object, key, member_span)) =
                    member_call_reference(callee)
                {
                    let object_value = self.emit_expression(object)?;
                    let callee_value =
                        self.emit_member_get(object_value.clone(), key, member_span)?;
                    (callee_value, object_value)
                } else {
                    let callee_value = self.emit_expression(callee)?;
                    let this_value = self.emit_undefined("materialize undefined call receiver")?;
                    (callee_value, this_value)
                };
                let mut operands = vec![callee_value.id, this_value.id];
                for argument in arguments {
                    operands.push(self.emit_expression(argument)?.id);
                }
                self.emit_instruction(
                    "js.call",
                    operands,
                    Some(ValueType::DYNAMIC),
                    BTreeMap::new(),
                    dynamic_effects(),
                    self.user_source(*span, Vec::new()),
                )?
                .ok_or_else(|| {
                    diag(
                        self.syntax,
                        "lowering.call_result",
                        Some(*span),
                        "js.call produced no value",
                    )
                })
            }
            SyntaxExpression::Member { object, key, span } => {
                let object = self.emit_expression(object)?;
                self.emit_member_get(object, key, *span)
            }
            SyntaxExpression::Assign {
                target,
                value,
                span,
            } => {
                let target = self.evaluate_assignment_target(target, *span)?;
                let is_promise = self.is_promise_expression(value);
                let value = self.emit_expression(value)?;
                if let EvaluatedAssignmentTarget::Binding(binding) = &target {
                    if is_promise {
                        self.promise_bindings.insert(binding.clone());
                    } else {
                        self.promise_bindings.remove(binding);
                    }
                }
                self.emit_assignment_target(target, value, *span)
            }
            SyntaxExpression::Update { span, .. } => self.fail(
                "lowering.update_expression_unsupported",
                Some(*span),
                "increment/decrement requires ECMAScript ToNumeric semantics, including BigInt",
            ),
            SyntaxExpression::Function { function, span, .. } => self.emit_closure(function, *span),
            SyntaxExpression::Unsupported { syntax, span } => self.fail(
                "lowering.unsupported_expression",
                Some(*span),
                format!("syntax {syntax:?} is outside the implemented Core IR lowering subset"),
            ),
        }
    }

    fn emit_literal(
        &mut self,
        literal: &SyntaxLiteral,
        span: SourceSpan,
    ) -> Result<ValueRef, LoweringDiagnostic> {
        match literal {
            SyntaxLiteral::Null => self.emit_constant(
                "null",
                JsonValue::Null,
                ValueType {
                    semantic: SemanticType::Null,
                    representation: Representation::Boxed,
                },
                Some(span),
            ),
            SyntaxLiteral::Boolean(value) => {
                self.emit_constant("boolean", json!(value), ValueType::BOOLEAN, Some(span))
            }
            SyntaxLiteral::Number(value) => self.emit_constant(
                "number",
                number_literal_json(*value),
                ValueType::NUMBER,
                Some(span),
            ),
            SyntaxLiteral::String(value) => self.emit_constant(
                "string",
                JsonValue::String(value.clone()),
                ValueType::STRING,
                Some(span),
            ),
            SyntaxLiteral::Bigint(value) => self.emit_constant(
                "bigint",
                JsonValue::String(value.clone()),
                ValueType {
                    semantic: SemanticType::Bigint,
                    representation: Representation::BigintRef,
                },
                Some(span),
            ),
        }
    }

    fn evaluate_assignment_target(
        &mut self,
        target: &SyntaxAssignmentTarget,
        span: SourceSpan,
    ) -> Result<EvaluatedAssignmentTarget, LoweringDiagnostic> {
        match target {
            SyntaxAssignmentTarget::Identifier {
                binding: Some(binding),
                ..
            } => Ok(EvaluatedAssignmentTarget::Binding(binding.clone())),
            SyntaxAssignmentTarget::Identifier {
                name,
                binding: None,
                ..
            } => self.fail(
                "lowering.unresolved_assignment_target",
                Some(span),
                format!("assignment to unresolved global {name:?} is unsupported"),
            ),
            SyntaxAssignmentTarget::Member { object, key, .. } => {
                let object = self.emit_expression(object)?;
                let key = match key {
                    SyntaxMemberKey::Static(key) => EvaluatedMemberKey::Static(key.clone()),
                    SyntaxMemberKey::Computed(expression) => {
                        EvaluatedMemberKey::Computed(self.emit_expression(expression)?)
                    }
                };
                Ok(EvaluatedAssignmentTarget::Member { object, key })
            }
            SyntaxAssignmentTarget::Unsupported { syntax, span } => self.fail(
                "lowering.unsupported_assignment_target",
                Some(*span),
                format!("assignment target {syntax:?} is unsupported"),
            ),
        }
    }

    fn emit_assignment_target(
        &mut self,
        target: EvaluatedAssignmentTarget,
        value: ValueRef,
        span: SourceSpan,
    ) -> Result<ValueRef, LoweringDiagnostic> {
        match target {
            EvaluatedAssignmentTarget::Binding(binding) => {
                self.emit_env_store(&binding, value, Some(span), None)
            }
            EvaluatedAssignmentTarget::Member { object, key } => {
                self.emit_member_set(object, key, value, span)
            }
        }
    }

    fn compile_sequence(
        &mut self,
        statements: &[SyntaxStatement],
    ) -> Result<(), LoweringDiagnostic> {
        self.emit_hoisted_functions(statements)?;
        for statement in statements {
            if self.current.is_none() {
                break;
            }
            self.compile_statement(statement)?;
        }
        Ok(())
    }

    fn emit_hoisted_functions(
        &mut self,
        statements: &[SyntaxStatement],
    ) -> Result<(), LoweringDiagnostic> {
        // Sloppy scripts may contain duplicate function declarations in
        // the same function-scoped environment. FunctionDeclarationInstantiation
        // keeps the last declaration for a duplicate name, so visit declarations
        // in reverse source order and materialize each binding once.
        for statement in statements.iter().rev() {
            if let SyntaxStatement::FunctionDeclaration {
                function,
                binding: Some(binding),
                span,
                ..
            } = statement
                && self.hoisted_functions.insert(binding.clone())
            {
                let closure = self.emit_closure(function, *span)?;
                self.emit_env_store(binding, closure, Some(*span), None)?;
            }
        }
        Ok(())
    }

    fn compile_statement(&mut self, statement: &SyntaxStatement) -> Result<(), LoweringDiagnostic> {
        match statement {
            SyntaxStatement::Block { body, .. } => self.compile_sequence(body),
            SyntaxStatement::Variable {
                kind,
                declarations,
                span,
            } => {
                if *kind == SyntaxVariableKind::Unsupported {
                    return self.fail(
                        "lowering.unsupported_declaration_kind",
                        Some(*span),
                        "using/await using declarations are not supported by the M1 compiler profile",
                    );
                }
                for declaration in declarations {
                    let Some(binding_id) = declaration.binding.as_deref() else {
                        return self.fail(
                            "lowering.destructuring_declaration",
                            Some(declaration.span),
                            "destructuring declarations are not supported by the M1 lowering subset",
                        );
                    };
                    match kind {
                        SyntaxVariableKind::Var => {
                            if let Some(initializer) = &declaration.initializer {
                                let is_promise = self.is_promise_expression(initializer);
                                let value = self.emit_expression(initializer)?;
                                self.emit_env_store(
                                    binding_id,
                                    value,
                                    Some(declaration.span),
                                    None,
                                )?;
                                if is_promise {
                                    self.promise_bindings.insert(binding_id.to_owned());
                                } else {
                                    self.promise_bindings.remove(binding_id);
                                }
                            } else {
                                self.promise_bindings.remove(binding_id);
                            }
                        }
                        SyntaxVariableKind::Let => {
                            let is_promise = declaration
                                .initializer
                                .as_ref()
                                .is_some_and(|initializer| self.is_promise_expression(initializer));
                            let value = if let Some(initializer) = &declaration.initializer {
                                self.emit_expression(initializer)?
                            } else {
                                self.emit_undefined("initialize uninitialized let declaration")?
                            };
                            self.emit_env_store(binding_id, value, Some(declaration.span), None)?;
                            if is_promise {
                                self.promise_bindings.insert(binding_id.to_owned());
                            } else {
                                self.promise_bindings.remove(binding_id);
                            }
                        }
                        SyntaxVariableKind::Const => {
                            let Some(initializer) = &declaration.initializer else {
                                return self.fail(
                                    "lowering.const_without_initializer",
                                    Some(declaration.span),
                                    "const declaration has no initializer",
                                );
                            };
                            let is_promise = self.is_promise_expression(initializer);
                            let value = self.emit_expression(initializer)?;
                            self.attach_binding_to_value_source(
                                &value.id,
                                binding_id,
                                declaration.span,
                            )?;
                            if is_promise {
                                self.promise_bindings.insert(binding_id.to_owned());
                            }
                            self.const_values.insert(binding_id.to_owned(), value);
                        }
                        SyntaxVariableKind::Unsupported => unreachable!(),
                    }
                }
                Ok(())
            }
            SyntaxStatement::FunctionDeclaration {
                binding: Some(binding),
                ..
            } => {
                if self.hoisted_functions.contains(binding) {
                    Ok(())
                } else {
                    self.fail(
                        "lowering.function_declaration_scope",
                        Some(self.syntax.span),
                        "function declaration was not materialized at the beginning of its lexical block",
                    )
                }
            }
            SyntaxStatement::FunctionDeclaration {
                binding: None,
                span,
                ..
            } => self.fail(
                "lowering.function_declaration_binding_missing",
                Some(*span),
                "function declaration has no resolved lexical binding",
            ),
            SyntaxStatement::Expression { expression, .. } => {
                self.emit_expression(expression)?;
                Ok(())
            }
            SyntaxStatement::If {
                test,
                consequent,
                alternate,
                span,
            } => self.compile_if(test, consequent, alternate.as_deref(), *span),
            SyntaxStatement::While { test, body, span } => self.compile_while(test, body, *span),
            SyntaxStatement::For {
                initializer,
                test,
                update,
                body,
                span,
                ..
            } => self.compile_for(
                initializer.as_ref(),
                test.as_ref(),
                update.as_ref(),
                body,
                *span,
            ),
            SyntaxStatement::Return { value, span } => {
                let value = if let Some(value) = value {
                    self.emit_expression(value)?
                } else {
                    self.emit_undefined("materialize undefined return value")?
                };
                self.terminate_return(Some(value.id), self.user_source(*span, Vec::new()))
            }
            SyntaxStatement::Throw { value, span } => {
                let value = self.emit_expression(value)?;
                self.emit_instruction(
                    "js.throw",
                    vec![value.id],
                    None,
                    BTreeMap::new(),
                    Effects {
                        may_throw: true,
                        ..Effects::default()
                    },
                    self.user_source(*span, Vec::new()),
                )?;
                let Some(block) = self.current else {
                    return self.fail(
                        "lowering.throw_block",
                        Some(*span),
                        "throw instruction has no current block",
                    );
                };
                self.blocks[block].terminator = Terminator::Unreachable {
                    reason: UnreachableReason::AfterThrow,
                    source: self.user_source(*span, Vec::new()),
                };
                self.current = None;
                self.current_effect = None;
                Ok(())
            }
            SyntaxStatement::Empty { .. } => Ok(()),
            SyntaxStatement::Unsupported { syntax, span } => self.fail(
                "lowering.unsupported_statement",
                Some(*span),
                format!("statement {syntax:?} is outside the implemented Core IR lowering subset"),
            ),
        }
    }

    fn compile_if(
        &mut self,
        test: &SyntaxExpression,
        consequent: &SyntaxStatement,
        alternate: Option<&SyntaxStatement>,
        span: SourceSpan,
    ) -> Result<(), LoweringDiagnostic> {
        let condition = self.emit_expression(test)?;
        let condition = self.emit_to_boolean(condition, span)?;
        let Some(predecessor) = self.cursor() else {
            return self.fail(
                "lowering.if_without_block",
                Some(span),
                "if statement has no active predecessor",
            );
        };
        let then_block = self.create_block();
        let else_block = self.create_block();
        let arguments: Vec<String> = predecessor.effect.clone().into_iter().collect();
        let then_id = self.block_id(then_block);
        let else_id = self.block_id(else_block);
        let branch_source = self.user_source(span, Vec::new());
        self.blocks[predecessor.block].terminator = Terminator::Branch {
            condition: condition.id,
            then_edge: Edge {
                target: then_id,
                arguments: arguments.clone(),
            },
            else_edge: Edge {
                target: else_id,
                arguments,
            },
            source: branch_source,
        };

        self.select_block(then_block);
        self.compile_statement(consequent)?;
        let then_cursor = self.cursor();

        self.select_block(else_block);
        if let Some(alternate) = alternate {
            self.compile_statement(alternate)?;
        }
        let else_cursor = self.cursor();

        if then_cursor.is_none() && else_cursor.is_none() {
            self.current = None;
            self.current_effect = None;
            return Ok(());
        }
        let merge_block = self.create_block();
        let merge_id = self.block_id(merge_block);
        if let Some(cursor) = then_cursor {
            self.restore_cursor(cursor);
            self.terminate_jump(&merge_id, span)?;
        }
        if let Some(cursor) = else_cursor {
            self.restore_cursor(cursor);
            self.terminate_jump(&merge_id, span)?;
        }
        self.select_block(merge_block);
        Ok(())
    }

    fn compile_while(
        &mut self,
        test: &SyntaxExpression,
        body: &SyntaxStatement,
        span: SourceSpan,
    ) -> Result<(), LoweringDiagnostic> {
        let Some(preheader) = self.cursor() else {
            return self.fail(
                "lowering.while_without_block",
                Some(span),
                "while loop has no active predecessor",
            );
        };
        let header = self.create_block();
        let body_block = self.create_block();
        let exit_block = self.create_block();
        let header_id = self.block_id(header);
        self.restore_cursor(preheader);
        self.terminate_jump(&header_id, span)?;
        self.select_block(header);

        let condition = self.emit_expression(test)?;
        let condition = self.emit_to_boolean(condition, span)?;
        let Some(header_cursor) = self.cursor() else {
            return self.fail(
                "lowering.while_header",
                Some(span),
                "while header terminated during test evaluation",
            );
        };
        let effect_args: Vec<String> = header_cursor.effect.into_iter().collect();
        let body_id = self.block_id(body_block);
        let exit_id = self.block_id(exit_block);
        let branch_source = self.user_source(span, Vec::new());
        self.blocks[header_cursor.block].terminator = Terminator::Branch {
            condition: condition.id,
            then_edge: Edge {
                target: body_id,
                arguments: effect_args.clone(),
            },
            else_edge: Edge {
                target: exit_id,
                arguments: effect_args,
            },
            source: branch_source,
        };

        self.select_block(body_block);
        self.compile_statement(body)?;
        if let Some(cursor) = self.cursor() {
            self.restore_cursor(cursor);
            self.terminate_jump(&header_id, span)?;
        }
        self.select_block(exit_block);
        Ok(())
    }

    fn compile_for(
        &mut self,
        initializer: Option<&SyntaxForInitializer>,
        test: Option<&SyntaxExpression>,
        update: Option<&SyntaxExpression>,
        body: &SyntaxStatement,
        span: SourceSpan,
    ) -> Result<(), LoweringDiagnostic> {
        if let Some(initializer) = initializer {
            match initializer {
                SyntaxForInitializer::Variable {
                    kind,
                    declarations,
                    span,
                } => {
                    self.compile_statement(&SyntaxStatement::Variable {
                        kind: *kind,
                        declarations: declarations.clone(),
                        span: *span,
                    })?;
                }
                SyntaxForInitializer::Expression(expression) => {
                    self.emit_expression(expression)?;
                }
            }
        }
        let Some(preheader) = self.cursor() else {
            return self.fail(
                "lowering.for_without_block",
                Some(span),
                "for loop has no active predecessor",
            );
        };
        let header = self.create_block();
        let body_block = self.create_block();
        let exit_block = self.create_block();
        let header_id = self.block_id(header);
        self.restore_cursor(preheader);
        self.terminate_jump(&header_id, span)?;
        self.select_block(header);

        let condition = if let Some(test) = test {
            let value = self.emit_expression(test)?;
            self.emit_to_boolean(value, span)?
        } else {
            self.emit_literal(&SyntaxLiteral::Boolean(true), span)?
        };
        let Some(header_cursor) = self.cursor() else {
            return self.fail(
                "lowering.for_header",
                Some(span),
                "for header terminated during condition evaluation",
            );
        };
        let effect_args: Vec<String> = header_cursor.effect.into_iter().collect();
        let body_id = self.block_id(body_block);
        let exit_id = self.block_id(exit_block);
        let branch_source = self.user_source(span, Vec::new());
        self.blocks[header_cursor.block].terminator = Terminator::Branch {
            condition: condition.id,
            then_edge: Edge {
                target: body_id,
                arguments: effect_args.clone(),
            },
            else_edge: Edge {
                target: exit_id,
                arguments: effect_args,
            },
            source: branch_source,
        };

        self.select_block(body_block);
        self.compile_statement(body)?;
        if let Some(body_cursor) = self.cursor() {
            let update_block = self.create_block();
            let update_id = self.block_id(update_block);
            self.restore_cursor(body_cursor);
            self.terminate_jump(&update_id, span)?;
            self.select_block(update_block);
            if let Some(update) = update {
                self.emit_expression(update)?;
            }
            self.terminate_jump(&header_id, span)?;
        }
        self.select_block(exit_block);
        Ok(())
    }

    fn terminate_jump(&mut self, target: &str, span: SourceSpan) -> Result<(), LoweringDiagnostic> {
        let Some(block) = self.current else {
            return self.fail(
                "lowering.jump_without_block",
                Some(span),
                "jump has no active source block",
            );
        };
        let arguments: Vec<String> = self.current_effect.clone().into_iter().collect();
        let source = self.user_source(span, Vec::new());
        self.blocks[block].terminator = Terminator::Jump {
            target: target.to_owned(),
            arguments,
            source,
        };
        self.current = None;
        self.current_effect = None;
        Ok(())
    }

    fn terminate_return(
        &mut self,
        value: Option<String>,
        source: Source,
    ) -> Result<(), LoweringDiagnostic> {
        let Some(block) = self.current else {
            return self.fail(
                "lowering.return_without_block",
                None,
                "return has no active source block",
            );
        };
        self.blocks[block].terminator = Terminator::Return { value, source };
        self.current = None;
        self.current_effect = None;
        Ok(())
    }

    fn emit_parameter_initializers(&mut self) -> Result<(), LoweringDiagnostic> {
        for parameter in self.semantic.parameters.clone() {
            self.emit_env_store(
                &parameter.binding,
                ValueRef {
                    id: parameter.id,
                    ty: ValueType {
                        semantic: parameter.semantic_type,
                        representation: parameter.representation,
                    },
                },
                None,
                Some("initialize function parameter environment binding"),
            )?;
        }
        Ok(())
    }

    fn emit_var_initializers(&mut self) -> Result<(), LoweringDiagnostic> {
        let var_ids: Vec<_> = self
            .local_bindings
            .values()
            .filter(|binding| binding.kind == BindingKind::Var)
            .map(|binding| binding.id.clone())
            .collect();
        let mut var_ids = var_ids;
        var_ids.sort();
        for binding_id in var_ids {
            let value = self.emit_undefined("initialize hoisted var binding")?;
            self.emit_env_store(
                &binding_id,
                value,
                None,
                Some("initialize hoisted var binding to undefined"),
            )?;
        }
        Ok(())
    }

    fn infer_return_signature(&self) -> Result<Option<ReturnSignature>, LoweringDiagnostic> {
        let mut returns = Vec::new();
        self.collect_return_types(&self.syntax.body, &mut returns)?;
        let undefined = ValueType {
            semantic: SemanticType::Undefined,
            representation: Representation::Boxed,
        };
        let returns: Vec<_> = returns
            .into_iter()
            .map(|value| value.unwrap_or(undefined))
            .collect();
        let ty = returns.first().copied().unwrap_or(undefined);
        if returns.iter().any(|item| *item != ty) {
            return self.fail(
                "lowering.mixed_return_types",
                Some(self.syntax.span),
                "function returns incompatible Core IR value types",
            );
        }
        if ty != undefined && sequence_can_fallthrough(&self.syntax.body) {
            return self.fail(
                "lowering.implicit_return_type",
                Some(self.syntax.span),
                "function has a value-returning path and a reachable undefined fallthrough path",
            );
        }
        Ok(Some(ReturnSignature {
            semantic_type: ty.semantic,
            representation: ty.representation,
        }))
    }

    fn collect_return_types(
        &self,
        statements: &[SyntaxStatement],
        output: &mut Vec<Option<ValueType>>,
    ) -> Result<(), LoweringDiagnostic> {
        for statement in statements {
            match statement {
                SyntaxStatement::Return { value, .. } => {
                    output.push(
                        value
                            .as_ref()
                            .map(|value| self.infer_expression_type(value))
                            .transpose()?,
                    );
                }
                SyntaxStatement::Block { body, .. } => self.collect_return_types(body, output)?,
                SyntaxStatement::If {
                    consequent,
                    alternate,
                    ..
                } => {
                    self.collect_return_types(std::slice::from_ref(consequent), output)?;
                    if let Some(alternate) = alternate {
                        self.collect_return_types(std::slice::from_ref(alternate), output)?;
                    }
                }
                SyntaxStatement::While { body, .. } | SyntaxStatement::For { body, .. } => {
                    self.collect_return_types(std::slice::from_ref(body), output)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn infer_expression_type(
        &self,
        expression: &SyntaxExpression,
    ) -> Result<ValueType, LoweringDiagnostic> {
        match expression {
            SyntaxExpression::Literal { value, .. } => Ok(literal_type(value)),
            SyntaxExpression::Identifier {
                name,
                binding,
                span,
            } => {
                let Some(binding) = binding else {
                    if name == "undefined" {
                        return Ok(ValueType {
                            semantic: SemanticType::Undefined,
                            representation: Representation::Boxed,
                        });
                    }
                    return self.fail(
                        "lowering.unresolved_global",
                        Some(*span),
                        format!("unresolved global {name:?} has no Core IR contract"),
                    );
                };
                if let Some(local) = self.local_bindings.get(binding) {
                    if local.kind == BindingKind::Const {
                        let initializer =
                            self.const_initializers.get(binding).ok_or_else(|| {
                                diag(
                                    self.syntax,
                                    "lowering.const_initializer_missing",
                                    Some(*span),
                                    "const initializer missing",
                                )
                            })?;
                        return self.infer_const_type(
                            binding,
                            initializer,
                            &mut HashSet::new(),
                            *span,
                        );
                    }
                    return Ok(ValueType::DYNAMIC);
                }
                if self.captures_by_binding.contains_key(binding) {
                    return Ok(ValueType::DYNAMIC);
                }
                self.fail(
                    "lowering.binding_not_in_environment",
                    Some(*span),
                    format!("unknown binding {binding:?}"),
                )
            }
            SyntaxExpression::Parenthesized { expression, .. } => {
                self.infer_expression_type(expression)
            }
            SyntaxExpression::Binary { operator, .. } => match operator {
                SyntaxBinaryOperator::Add => Ok(ValueType::DYNAMIC),
                SyntaxBinaryOperator::StrictEqual
                | SyntaxBinaryOperator::StrictNotEqual
                | SyntaxBinaryOperator::LessThan
                | SyntaxBinaryOperator::GreaterThan => Ok(ValueType::BOOLEAN),
            },
            SyntaxExpression::Unary {
                operator,
                argument,
                span,
            } => match operator {
                SyntaxUnaryOperator::LogicalNot => Ok(ValueType::BOOLEAN),
                SyntaxUnaryOperator::Typeof => Ok(ValueType::STRING),
                SyntaxUnaryOperator::Positive | SyntaxUnaryOperator::Negate => {
                    if matches!(
                        argument.as_ref(),
                        SyntaxExpression::Literal {
                            value: SyntaxLiteral::Number(_),
                            ..
                        }
                    ) {
                        Ok(ValueType::NUMBER)
                    } else {
                        self.fail(
                            "lowering.dynamic_unary_numeric",
                            Some(*span),
                            "dynamic unary numeric operation is unsupported",
                        )
                    }
                }
            },
            SyntaxExpression::Call { .. } | SyntaxExpression::Member { .. } => {
                Ok(ValueType::DYNAMIC)
            }
            SyntaxExpression::Assign { value, .. } => self.infer_expression_type(value),
            SyntaxExpression::Update { span, .. } => self.fail(
                "lowering.update_expression_unsupported",
                Some(*span),
                "update expression unsupported",
            ),
            SyntaxExpression::Function { .. } => Ok(ValueType::FUNCTION),
            SyntaxExpression::Unsupported { syntax, span } => self.fail(
                "lowering.unsupported_expression",
                Some(*span),
                format!("syntax {syntax:?} unsupported"),
            ),
        }
    }

    fn infer_const_type(
        &self,
        binding: &str,
        expression: &SyntaxExpression,
        visiting: &mut HashSet<String>,
        span: SourceSpan,
    ) -> Result<ValueType, LoweringDiagnostic> {
        if !visiting.insert(binding.to_owned()) {
            return self.fail(
                "lowering.const_initializer_cycle",
                Some(span),
                format!("cyclic const initializer for {binding:?}"),
            );
        }
        let result = match expression {
            SyntaxExpression::Identifier {
                binding: Some(other),
                ..
            } if self
                .local_bindings
                .get(other)
                .is_some_and(|item| item.kind == BindingKind::Const) =>
            {
                let initializer = self.const_initializers.get(other).ok_or_else(|| {
                    diag(
                        self.syntax,
                        "lowering.const_initializer_missing",
                        Some(span),
                        "const initializer missing",
                    )
                })?;
                self.infer_const_type(other, initializer, visiting, span)
            }
            _ => self.infer_expression_type(expression),
        };
        visiting.remove(binding);
        result
    }
}

fn dynamic_effects() -> Effects {
    Effects {
        may_allocate: true,
        may_gc: true,
        may_throw: true,
        may_suspend: false,
        may_call_host: true,
        may_reenter: true,
        may_mutate_shape: true,
    }
}

fn literal_type(literal: &SyntaxLiteral) -> ValueType {
    match literal {
        SyntaxLiteral::Null => ValueType {
            semantic: SemanticType::Null,
            representation: Representation::Boxed,
        },
        SyntaxLiteral::Boolean(_) => ValueType::BOOLEAN,
        SyntaxLiteral::Number(_) => ValueType::NUMBER,
        SyntaxLiteral::String(_) => ValueType::STRING,
        SyntaxLiteral::Bigint(_) => ValueType {
            semantic: SemanticType::Bigint,
            representation: Representation::BigintRef,
        },
    }
}

fn number_literal_json(value: f64) -> JsonValue {
    if value.is_nan() {
        JsonValue::String("NaN".to_owned())
    } else if value == f64::INFINITY {
        JsonValue::String("Infinity".to_owned())
    } else if value == f64::NEG_INFINITY {
        JsonValue::String("-Infinity".to_owned())
    } else if value == 0.0 && value.is_sign_negative() {
        JsonValue::String("-0".to_owned())
    } else {
        json!(value)
    }
}

fn collect_const_initializers(statements: &[SyntaxStatement]) -> HashMap<String, SyntaxExpression> {
    fn visit(statements: &[SyntaxStatement], output: &mut HashMap<String, SyntaxExpression>) {
        for statement in statements {
            match statement {
                SyntaxStatement::Variable {
                    kind: SyntaxVariableKind::Const,
                    declarations,
                    ..
                } => {
                    for declaration in declarations {
                        if let (Some(binding), Some(initializer)) =
                            (&declaration.binding, &declaration.initializer)
                        {
                            output.insert(binding.clone(), initializer.clone());
                        }
                    }
                }
                SyntaxStatement::Block { body, .. } => visit(body, output),
                SyntaxStatement::If {
                    consequent,
                    alternate,
                    ..
                } => {
                    visit(std::slice::from_ref(consequent), output);
                    if let Some(alternate) = alternate {
                        visit(std::slice::from_ref(alternate), output);
                    }
                }
                SyntaxStatement::While { body, .. } | SyntaxStatement::For { body, .. } => {
                    visit(std::slice::from_ref(body), output);
                }
                _ => {}
            }
        }
    }
    let mut output = HashMap::new();
    visit(statements, &mut output);
    output
}

fn statements_need_effects(
    statements: &[SyntaxStatement],
    effectful_bindings: &HashSet<String>,
) -> bool {
    statements
        .iter()
        .any(|statement| statement_needs_effects(statement, effectful_bindings))
}

fn statement_needs_effects(
    statement: &SyntaxStatement,
    effectful_bindings: &HashSet<String>,
) -> bool {
    match statement {
        SyntaxStatement::Block { body, .. } => statements_need_effects(body, effectful_bindings),
        SyntaxStatement::Variable {
            kind, declarations, ..
        } => {
            *kind != SyntaxVariableKind::Const
                || declarations.iter().any(|item| {
                    item.initializer.as_ref().is_some_and(|expression| {
                        expression_needs_effects(expression, effectful_bindings)
                    })
                })
        }
        SyntaxStatement::FunctionDeclaration { .. } => true,
        SyntaxStatement::Expression { expression, .. } => {
            expression_needs_effects(expression, effectful_bindings)
        }
        SyntaxStatement::If {
            test,
            consequent,
            alternate,
            ..
        } => {
            expression_needs_effects(test, effectful_bindings)
                || statement_needs_effects(consequent, effectful_bindings)
                || alternate
                    .as_ref()
                    .is_some_and(|item| statement_needs_effects(item, effectful_bindings))
        }
        SyntaxStatement::While { test, body, .. } => {
            expression_needs_effects(test, effectful_bindings)
                || statement_needs_effects(body, effectful_bindings)
        }
        SyntaxStatement::For {
            initializer,
            test,
            update,
            body,
            ..
        } => {
            initializer
                .as_ref()
                .is_some_and(|initializer| match initializer {
                    SyntaxForInitializer::Variable {
                        kind, declarations, ..
                    } => {
                        *kind != SyntaxVariableKind::Const
                            || declarations.iter().any(|item| {
                                item.initializer.as_ref().is_some_and(|expression| {
                                    expression_needs_effects(expression, effectful_bindings)
                                })
                            })
                    }
                    SyntaxForInitializer::Expression(expression) => {
                        expression_needs_effects(expression, effectful_bindings)
                    }
                })
                || test.as_ref().is_some_and(|expression| {
                    expression_needs_effects(expression, effectful_bindings)
                })
                || update.as_ref().is_some_and(|expression| {
                    expression_needs_effects(expression, effectful_bindings)
                })
                || statement_needs_effects(body, effectful_bindings)
        }
        SyntaxStatement::Return { value, .. } => value
            .as_ref()
            .is_some_and(|expression| expression_needs_effects(expression, effectful_bindings)),
        SyntaxStatement::Throw { .. } => true,
        SyntaxStatement::Empty { .. } | SyntaxStatement::Unsupported { .. } => false,
    }
}

fn expression_needs_effects(
    expression: &SyntaxExpression,
    effectful_bindings: &HashSet<String>,
) -> bool {
    match expression {
        SyntaxExpression::Literal { .. } => false,
        SyntaxExpression::Identifier {
            binding: None,
            name,
            ..
        } => name != "undefined",
        SyntaxExpression::Identifier {
            binding: Some(binding),
            ..
        } => effectful_bindings.contains(binding),
        SyntaxExpression::Binary {
            operator,
            left,
            right,
            ..
        } => {
            !matches!(
                operator,
                SyntaxBinaryOperator::StrictEqual | SyntaxBinaryOperator::StrictNotEqual
            ) || expression_needs_effects(left, effectful_bindings)
                || expression_needs_effects(right, effectful_bindings)
        }
        SyntaxExpression::Unary {
            operator, argument, ..
        } => {
            if *operator == SyntaxUnaryOperator::Typeof
                && matches!(
                    argument.as_ref(),
                    SyntaxExpression::Identifier {
                        name,
                        binding: None,
                        ..
                    } if name != "undefined"
                )
            {
                // typeof on an unresolvable reference is a pure operation and
                // produces the string "undefined" without a global lookup.
                false
            } else {
                (matches!(
                    operator,
                    SyntaxUnaryOperator::Positive | SyntaxUnaryOperator::Negate
                ) && !matches!(
                    argument.as_ref(),
                    SyntaxExpression::Literal {
                        value: SyntaxLiteral::Number(_),
                        ..
                    }
                )) || expression_needs_effects(argument, effectful_bindings)
            }
        }
        SyntaxExpression::Call { .. }
        | SyntaxExpression::Member { .. }
        | SyntaxExpression::Assign { .. }
        | SyntaxExpression::Update { .. }
        | SyntaxExpression::Function { .. } => true,
        SyntaxExpression::Parenthesized { expression, .. } => {
            expression_needs_effects(expression, effectful_bindings)
        }
        SyntaxExpression::Unsupported { .. } => false,
    }
}

fn sequence_can_fallthrough(statements: &[SyntaxStatement]) -> bool {
    let mut reachable = true;
    for statement in statements {
        if !reachable {
            break;
        }
        reachable = statement_can_fallthrough(statement);
    }
    reachable
}

fn statement_can_fallthrough(statement: &SyntaxStatement) -> bool {
    match statement {
        SyntaxStatement::Return { .. } | SyntaxStatement::Throw { .. } => false,
        SyntaxStatement::Block { body, .. } => sequence_can_fallthrough(body),
        SyntaxStatement::If {
            consequent,
            alternate,
            ..
        } => {
            statement_can_fallthrough(consequent)
                || alternate
                    .as_ref()
                    .is_none_or(|item| statement_can_fallthrough(item))
        }
        SyntaxStatement::While { .. } | SyntaxStatement::For { .. } => true,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::SemanticBinding;
    use ogma_core_ir::{Environment, FunctionParameter};
    use ogma_frontend::{SourceFile, SyntaxDeclarator, SyntaxMemberKey};

    fn source(text: &str) -> SourceFile {
        SourceFile::new("lowering-tests", "memory:///lowering-tests.js", None, text)
            .expect("source is valid")
    }

    fn whole_span(source: &SourceFile) -> SourceSpan {
        source.span(0, source.text().len())
    }

    fn function_syntax(
        source: &SourceFile,
        body: Vec<SyntaxStatement>,
        parameters: Vec<String>,
    ) -> ResolvedSyntaxFunction {
        ResolvedSyntaxFunction {
            id: "fn_run".to_owned(),
            name: "run".to_owned(),
            scope: "scope_1".to_owned(),
            parent_function_scope: Some("scope_0".to_owned()),
            parameters,
            captures: Vec::new(),
            body,
            span: whole_span(source),
            is_arrow: false,
            is_async: false,
            is_generator: false,
            has_private_self_name: false,
        }
    }

    fn binding(id: &str, name: &str, kind: BindingKind) -> Binding {
        Binding {
            id: id.to_owned(),
            name: name.to_owned(),
            kind,
            mutable: matches!(
                kind,
                BindingKind::Var | BindingKind::Let | BindingKind::Parameter | BindingKind::Catch
            ),
            semantic_type: SemanticType::Dynamic,
            representation: Representation::Boxed,
        }
    }

    fn parameter(id: &str, name: &str, binding: &str) -> FunctionParameter {
        FunctionParameter {
            id: id.to_owned(),
            kind: "value".to_owned(),
            semantic_type: SemanticType::Dynamic,
            representation: Representation::Boxed,
            name: name.to_owned(),
            binding: binding.to_owned(),
        }
    }

    fn semantic_module(
        source: &SourceFile,
        syntax: &ResolvedSyntaxFunction,
        bindings: Vec<Binding>,
        parameters: Vec<FunctionParameter>,
    ) -> SemanticModule {
        let semantic_bindings = bindings
            .iter()
            .cloned()
            .map(|ir| SemanticBinding {
                ir,
                lexical_scope: syntax.scope.clone(),
                declaration: syntax.span,
            })
            .collect();
        SemanticModule {
            source_id: source.id().as_str().to_owned(),
            source_sha256: source.content_sha256().to_owned(),
            functions: vec![FunctionSemantics {
                id: syntax.id.clone(),
                name: syntax.name.clone(),
                environment: Environment {
                    id: syntax.scope.clone(),
                    parent: syntax.parent_function_scope.clone(),
                    bindings,
                },
                parameters,
                captures: Vec::new(),
                span: syntax.span,
                parent_function_scope: syntax.parent_function_scope.clone(),
                is_arrow: syntax.is_arrow,
                is_async: syntax.is_async,
                is_generator: syntax.is_generator,
            }],
            bindings: semantic_bindings,
        }
    }

    fn identifier(name: &str, binding: &str, span: SourceSpan) -> SyntaxExpression {
        SyntaxExpression::Identifier {
            name: name.to_owned(),
            binding: Some(binding.to_owned()),
            span,
        }
    }

    fn source_scope(source: &Source) -> Option<&str> {
        match source {
            Source::User { scope, .. } | Source::Generated { scope, .. } => scope.as_deref(),
        }
    }

    #[test]
    fn lowers_literal_return_to_verified_core_ir() {
        let source = source("function run() { return 42; }");
        let span = whole_span(&source);
        let literal_start = source.text().find("42").expect("literal exists");
        let literal_span = source.span(literal_start, literal_start + 2);
        let syntax = function_syntax(
            &source,
            vec![SyntaxStatement::Return {
                value: Some(SyntaxExpression::Literal {
                    value: SyntaxLiteral::Number(42.0),
                    span: literal_span,
                }),
                span,
            }],
            Vec::new(),
        );
        let module = semantic_module(&source, &syntax, Vec::new(), Vec::new());
        let documents = lower(
            &source,
            &module,
            &ResolvedSyntaxProgram {
                functions: vec![syntax],
                module: Default::default(),
            },
        )
        .expect("literal return lowers");
        assert_eq!(documents.len(), 1);
        assert_eq!(
            documents[0].function.blocks[0].instructions[0].op,
            "js.constant"
        );
        assert_eq!(
            documents[0]
                .function
                .return_signature
                .as_ref()
                .map(|result| result.semantic_type),
            Some(SemanticType::Number)
        );
        assert!(verify_document(&documents[0]).is_empty());
    }

    #[test]
    fn lowers_if_branches_with_effect_token_merge_and_binding_provenance() {
        let source = source(
            "function run() { let value = 1; if (true) { value = 2; } else { value = 3; } return value; }",
        );
        let whole = whole_span(&source);
        let binding = binding("binding_value", "value", BindingKind::Let);
        let declaration = SyntaxStatement::Variable {
            kind: SyntaxVariableKind::Let,
            declarations: vec![SyntaxDeclarator {
                binding: Some(binding.id.clone()),
                name: binding.name.clone(),
                initializer: Some(SyntaxExpression::Literal {
                    value: SyntaxLiteral::Number(1.0),
                    span: whole,
                }),
                span: whole,
            }],
            span: whole,
        };
        let assignment = |number: f64, scope: &str| SyntaxStatement::Block {
            scope: Some(scope.to_owned()),
            body: vec![SyntaxStatement::Expression {
                expression: SyntaxExpression::Assign {
                    target: SyntaxAssignmentTarget::Identifier {
                        name: "value".to_owned(),
                        binding: Some(binding.id.clone()),
                        span: whole,
                    },
                    value: Box::new(SyntaxExpression::Literal {
                        value: SyntaxLiteral::Number(number),
                        span: whole,
                    }),
                    span: whole,
                },
                span: whole,
            }],
            span: whole,
        };
        let syntax = function_syntax(
            &source,
            vec![
                declaration,
                SyntaxStatement::If {
                    test: SyntaxExpression::Literal {
                        value: SyntaxLiteral::Boolean(true),
                        span: whole,
                    },
                    consequent: Box::new(assignment(2.0, "scope_2")),
                    alternate: Some(Box::new(assignment(3.0, "scope_3"))),
                    span: whole,
                },
                SyntaxStatement::Return {
                    value: Some(identifier("value", &binding.id, whole)),
                    span: whole,
                },
            ],
            Vec::new(),
        );
        let module = semantic_module(&source, &syntax, vec![binding.clone()], Vec::new());
        let documents = lower(
            &source,
            &module,
            &ResolvedSyntaxProgram {
                functions: vec![syntax],
                module: Default::default(),
            },
        )
        .expect("branching mutable binding lowers");
        let function = &documents[0].function;
        assert!(function.effect_entry.is_some());
        assert!(verify_document(&documents[0]).is_empty());

        let store_scopes: Vec<_> = function
            .blocks
            .iter()
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| {
                instruction.op == "env.store"
                    && instruction
                        .attributes
                        .get("binding")
                        .and_then(JsonValue::as_str)
                        == Some(binding.id.as_str())
            })
            .filter_map(|instruction| source_scope(&instruction.source))
            .collect();
        assert!(!store_scopes.is_empty());
        assert!(store_scopes.iter().all(|scope| *scope == "scope_1"));
        let binding_provenance: Vec<_> = function
            .blocks
            .iter()
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| instruction.op == "env.store")
            .filter_map(|instruction| match &instruction.source {
                Source::User { bindings, .. } | Source::Generated { bindings, .. } => {
                    bindings.first().map(String::as_str)
                }
            })
            .collect();
        assert!(binding_provenance.contains(&binding.id.as_str()));

        let branch = function
            .blocks
            .iter()
            .find_map(|block| match &block.terminator {
                Terminator::Branch {
                    then_edge,
                    else_edge,
                    ..
                } => Some((then_edge, else_edge)),
                _ => None,
            })
            .expect("if branch exists");
        assert_eq!(branch.0.arguments.len(), 1);
        assert_eq!(branch.1.arguments.len(), 1);

        let merge_targets: Vec<_> = function
            .blocks
            .iter()
            .filter_map(|block| match &block.terminator {
                Terminator::Jump { target, .. } => Some(target.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(merge_targets.len(), 2);
        assert_eq!(merge_targets[0], merge_targets[1]);
        let merge = function
            .blocks
            .iter()
            .find(|block| block.id == merge_targets[0])
            .expect("branch merge block exists");
        assert!(
            merge
                .parameters
                .iter()
                .any(|parameter| matches!(parameter, BlockParameter::Effect(_)))
        );
    }

    #[test]
    fn evaluates_computed_assignment_reference_before_right_hand_side() {
        let source = source("function run(obj, key, rhs) { obj[key] = rhs; }");
        let assignment_start = source
            .text()
            .rfind("obj[key]")
            .expect("assignment target exists");
        let obj_span = source.span(assignment_start, assignment_start + 3);
        let key_span = source.span(assignment_start + 4, assignment_start + 7);
        let rhs_start = source.text().rfind("rhs").expect("rhs reference exists");
        let rhs_span = source.span(rhs_start, rhs_start + 3);
        let assignment_span = source.span(assignment_start, rhs_start + 3);
        let parameter_bindings = vec![
            binding("binding_obj", "obj", BindingKind::Parameter),
            binding("binding_key", "key", BindingKind::Parameter),
            binding("binding_rhs", "rhs", BindingKind::Parameter),
        ];
        let parameters = vec![
            parameter("param_run_0", "obj", "binding_obj"),
            parameter("param_run_1", "key", "binding_key"),
            parameter("param_run_2", "rhs", "binding_rhs"),
        ];
        let syntax = function_syntax(
            &source,
            vec![SyntaxStatement::Expression {
                expression: SyntaxExpression::Assign {
                    target: SyntaxAssignmentTarget::Member {
                        object: Box::new(identifier("obj", "binding_obj", obj_span)),
                        key: SyntaxMemberKey::Computed(Box::new(identifier(
                            "key",
                            "binding_key",
                            key_span,
                        ))),
                        span: source.span(assignment_start, assignment_start + 7),
                    },
                    value: Box::new(identifier("rhs", "binding_rhs", rhs_span)),
                    span: assignment_span,
                },
                span: assignment_span,
            }],
            vec![
                "binding_obj".to_owned(),
                "binding_key".to_owned(),
                "binding_rhs".to_owned(),
            ],
        );
        let module = semantic_module(&source, &syntax, parameter_bindings, parameters);
        let documents = lower(
            &source,
            &module,
            &ResolvedSyntaxProgram {
                functions: vec![syntax],
                module: Default::default(),
            },
        )
        .expect("computed member assignment lowers");
        let function = &documents[0].function;
        let loads: Vec<_> = function
            .blocks
            .iter()
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| instruction.op == "env.load")
            .filter_map(|instruction| {
                instruction
                    .attributes
                    .get("binding")
                    .and_then(JsonValue::as_str)
            })
            .collect();
        assert_eq!(
            loads,
            vec!["binding_obj", "binding_key", "binding_rhs"],
            "LHS object and key must be evaluated before the RHS"
        );
        assert!(
            function.blocks[0]
                .instructions
                .iter()
                .any(|instruction| { instruction.op == "js.set_element" })
        );
        assert!(verify_document(&documents[0]).is_empty());
    }

    #[test]
    fn rejects_stale_semantic_source_and_unsupported_syntax() {
        let source = source("function run() {}");
        let span = whole_span(&source);
        let syntax = function_syntax(
            &source,
            vec![SyntaxStatement::Unsupported {
                syntax: "try_statement".to_owned(),
                span,
            }],
            Vec::new(),
        );
        let mut module = semantic_module(&source, &syntax, Vec::new(), Vec::new());
        module.source_sha256 = "0".repeat(64);
        let stale = lower(
            &source,
            &module,
            &ResolvedSyntaxProgram {
                functions: vec![syntax.clone()],
                module: Default::default(),
            },
        )
        .expect_err("mismatched semantic revision must be rejected");
        assert_eq!(stale[0].code, "lowering.source_identity_mismatch");

        let module = semantic_module(&source, &syntax, Vec::new(), Vec::new());
        let unsupported = lower(
            &source,
            &module,
            &ResolvedSyntaxProgram {
                functions: vec![syntax],
                module: Default::default(),
            },
        )
        .expect_err("unsupported statement must fail closed");
        assert_eq!(unsupported[0].code, "lowering.unsupported_statement");
    }

    #[test]
    fn lowers_loop_cfg_with_verified_backedge() {
        let source = source("function run() { while (false) {} }");
        let span = whole_span(&source);
        let syntax = function_syntax(
            &source,
            vec![SyntaxStatement::While {
                test: SyntaxExpression::Literal {
                    value: SyntaxLiteral::Boolean(false),
                    span,
                },
                body: Box::new(SyntaxStatement::Empty { span }),
                span,
            }],
            Vec::new(),
        );
        let module = semantic_module(&source, &syntax, Vec::new(), Vec::new());
        let documents = lower(
            &source,
            &module,
            &ResolvedSyntaxProgram {
                functions: vec![syntax],
                module: Default::default(),
            },
        )
        .expect("while loop lowers to verified CFG");
        let function = &documents[0].function;
        assert_eq!(function.blocks.len(), 4);
        let header = function
            .blocks
            .iter()
            .find(|block| matches!(block.terminator, Terminator::Branch { .. }))
            .expect("loop header branch exists");
        let body_id = match &header.terminator {
            Terminator::Branch { then_edge, .. } => then_edge.target.as_str(),
            _ => unreachable!(),
        };
        let body = function
            .blocks
            .iter()
            .find(|block| block.id == body_id)
            .expect("loop body block exists");
        assert!(matches!(
            body.terminator,
            Terminator::Jump { ref target, .. } if target == &header.id
        ));
        assert!(verify_document(&documents[0]).is_empty());
    }

    #[test]
    fn lowers_calls_with_explicit_receiver_effects_and_typed_result() {
        let source = source("function run(callback) { return callback(7); }");
        let span = whole_span(&source);
        let callback = binding("binding_callback", "callback", BindingKind::Parameter);
        let param = parameter("param_run_0", "callback", &callback.id);
        let call_span = span;
        let syntax = function_syntax(
            &source,
            vec![SyntaxStatement::Return {
                value: Some(SyntaxExpression::Call {
                    callee: Box::new(identifier("callback", &callback.id, span)),
                    arguments: vec![SyntaxExpression::Literal {
                        value: SyntaxLiteral::Number(7.0),
                        span,
                    }],
                    span: call_span,
                }),
                span,
            }],
            vec![callback.id.clone()],
        );
        let module = semantic_module(&source, &syntax, vec![callback], vec![param]);
        let documents = lower(
            &source,
            &module,
            &ResolvedSyntaxProgram {
                functions: vec![syntax],
                module: Default::default(),
            },
        )
        .expect("call lowering succeeds");
        let function = &documents[0].function;
        let call = function
            .blocks
            .iter()
            .flat_map(|block| block.instructions.iter())
            .find(|instruction| instruction.op == "js.call")
            .expect("js.call emitted");
        assert_eq!(call.operands.len(), 3);
        assert!(call.effects.may_throw);
        assert!(call.effects.may_call_host);
        assert!(call.effects.may_reenter);
        assert!(call.effect_in.is_some());
        assert!(call.effect_out.is_some());
        assert_eq!(
            function
                .return_signature
                .as_ref()
                .map(|result| result.semantic_type),
            Some(SemanticType::Dynamic)
        );
        assert!(verify_document(&documents[0]).is_empty());
    }

    #[test]
    fn lowers_throw_to_after_throw_completion() {
        let source = source("function run() { throw 'failure'; }");
        let span = whole_span(&source);
        let syntax = function_syntax(
            &source,
            vec![SyntaxStatement::Throw {
                value: SyntaxExpression::Literal {
                    value: SyntaxLiteral::String("failure".to_owned()),
                    span,
                },
                span,
            }],
            Vec::new(),
        );
        let module = semantic_module(&source, &syntax, Vec::new(), Vec::new());
        let documents = lower(
            &source,
            &module,
            &ResolvedSyntaxProgram {
                functions: vec![syntax],
                module: Default::default(),
            },
        )
        .expect("throw completion lowers");
        let function = &documents[0].function;
        let block = &function.blocks[0];
        assert_eq!(
            block.instructions.last().map(|item| item.op.as_str()),
            Some("js.throw")
        );
        assert!(matches!(
            block.terminator,
            Terminator::Unreachable {
                reason: UnreachableReason::AfterThrow,
                ..
            }
        ));
        assert!(verify_document(&documents[0]).is_empty());
    }

    #[test]
    fn lowers_initialized_const_closure_capture_by_value() {
        let source = source("function run() { { const value = 1; return () => value; } }");
        let span = whole_span(&source);
        let binding = binding("binding_const_capture", "value", BindingKind::Const);
        let parent = function_syntax(
            &source,
            vec![SyntaxStatement::Block {
                scope: Some("scope_const_block".to_owned()),
                body: vec![
                    SyntaxStatement::Variable {
                        kind: SyntaxVariableKind::Const,
                        declarations: vec![SyntaxDeclarator {
                            binding: Some(binding.id.clone()),
                            name: binding.name.clone(),
                            initializer: Some(SyntaxExpression::Literal {
                                value: SyntaxLiteral::Number(1.0),
                                span,
                            }),
                            span,
                        }],
                        span,
                    },
                    SyntaxStatement::Return {
                        value: Some(SyntaxExpression::Function {
                            function: "fn_const_child".to_owned(),
                            captures: vec![binding.id.clone()],
                            span,
                        }),
                        span,
                    },
                ],
                span,
            }],
            Vec::new(),
        );
        let child = ResolvedSyntaxFunction {
            id: "fn_const_child".to_owned(),
            name: "<const-arrow>".to_owned(),
            scope: "scope_const_child".to_owned(),
            parent_function_scope: Some(parent.scope.clone()),
            parameters: Vec::new(),
            captures: vec![binding.id.clone()],
            body: vec![SyntaxStatement::Return {
                value: Some(identifier("value", &binding.id, span)),
                span,
            }],
            span,
            is_arrow: true,
            is_async: false,
            is_generator: false,
            has_private_self_name: false,
        };
        let capture = Capture {
            id: "capture_const_value".to_owned(),
            source_environment: parent.scope.clone(),
            binding: binding.id.clone(),
            name: binding.name.clone(),
            mutable: false,
            semantic_type: SemanticType::Dynamic,
            representation: Representation::Boxed,
            mode: CaptureMode::Value,
        };
        let parent_semantics = FunctionSemantics {
            id: parent.id.clone(),
            name: parent.name.clone(),
            environment: Environment {
                id: parent.scope.clone(),
                parent: parent.parent_function_scope.clone(),
                bindings: vec![binding.clone()],
            },
            parameters: Vec::new(),
            captures: Vec::new(),
            span,
            parent_function_scope: parent.parent_function_scope.clone(),
            is_arrow: false,
            is_async: false,
            is_generator: false,
        };
        let child_semantics = FunctionSemantics {
            id: child.id.clone(),
            name: child.name.clone(),
            environment: Environment {
                id: child.scope.clone(),
                parent: child.parent_function_scope.clone(),
                bindings: Vec::new(),
            },
            parameters: Vec::new(),
            captures: vec![capture],
            span,
            parent_function_scope: child.parent_function_scope.clone(),
            is_arrow: true,
            is_async: false,
            is_generator: false,
        };
        let module = SemanticModule {
            source_id: source.id().as_str().to_owned(),
            source_sha256: source.content_sha256().to_owned(),
            functions: vec![parent_semantics, child_semantics],
            bindings: vec![SemanticBinding {
                ir: binding.clone(),
                lexical_scope: "scope_const_block".to_owned(),
                declaration: span,
            }],
        };
        let documents = lower(
            &source,
            &module,
            &ResolvedSyntaxProgram {
                functions: vec![parent, child],
                module: Default::default(),
            },
        )
        .expect("initialized const value can be captured without a block environment");
        let closure = documents[0].function.blocks[0]
            .instructions
            .iter()
            .find(|instruction| instruction.op == "closure.create")
            .expect("closure creation exists");
        let descriptors = closure
            .attributes
            .get("captures")
            .and_then(JsonValue::as_array)
            .unwrap();
        assert_eq!(descriptors.len(), 1);
        assert_eq!(
            descriptors[0].get("binding").and_then(JsonValue::as_str),
            Some(binding.id.as_str())
        );
        assert_eq!(
            descriptors[0].get("mode").and_then(JsonValue::as_str),
            Some("value")
        );
        assert!(verify_document(&documents[0]).is_empty());
        assert!(verify_document(&documents[1]).is_empty());
    }

    #[test]
    fn rejects_hoisted_closure_capture_of_uninitialized_const() {
        let source =
            source("function run() { function child() { return value; } const value = 1; }");
        let span = whole_span(&source);
        let value_binding = binding("binding_hoisted_const", "value", BindingKind::Const);
        let parent = function_syntax(
            &source,
            vec![
                SyntaxStatement::FunctionDeclaration {
                    function: "fn_hoisted_child".to_owned(),
                    binding: Some("binding_child".to_owned()),
                    name: "child".to_owned(),
                    span,
                },
                SyntaxStatement::Variable {
                    kind: SyntaxVariableKind::Const,
                    declarations: vec![SyntaxDeclarator {
                        binding: Some(value_binding.id.clone()),
                        name: value_binding.name.clone(),
                        initializer: Some(SyntaxExpression::Literal {
                            value: SyntaxLiteral::Number(1.0),
                            span,
                        }),
                        span,
                    }],
                    span,
                },
            ],
            Vec::new(),
        );
        let child = ResolvedSyntaxFunction {
            id: "fn_hoisted_child".to_owned(),
            name: "child".to_owned(),
            scope: "scope_hoisted_child".to_owned(),
            parent_function_scope: Some(parent.scope.clone()),
            parameters: Vec::new(),
            captures: vec![value_binding.id.clone()],
            body: vec![SyntaxStatement::Return {
                value: Some(identifier("value", &value_binding.id, span)),
                span,
            }],
            span,
            is_arrow: false,
            is_async: false,
            is_generator: false,
            has_private_self_name: false,
        };
        let capture = Capture {
            id: "capture_hoisted_const".to_owned(),
            source_environment: parent.scope.clone(),
            binding: value_binding.id.clone(),
            name: value_binding.name.clone(),
            mutable: false,
            semantic_type: SemanticType::Dynamic,
            representation: Representation::Boxed,
            mode: CaptureMode::Value,
        };
        let parent_semantics = FunctionSemantics {
            id: parent.id.clone(),
            name: parent.name.clone(),
            environment: Environment {
                id: parent.scope.clone(),
                parent: parent.parent_function_scope.clone(),
                bindings: vec![
                    value_binding.clone(),
                    binding("binding_child", "child", BindingKind::Var),
                ],
            },
            parameters: Vec::new(),
            captures: Vec::new(),
            span,
            parent_function_scope: parent.parent_function_scope.clone(),
            is_arrow: false,
            is_async: false,
            is_generator: false,
        };
        let child_semantics = FunctionSemantics {
            id: child.id.clone(),
            name: child.name.clone(),
            environment: Environment {
                id: child.scope.clone(),
                parent: child.parent_function_scope.clone(),
                bindings: Vec::new(),
            },
            parameters: Vec::new(),
            captures: vec![capture],
            span,
            parent_function_scope: child.parent_function_scope.clone(),
            is_arrow: false,
            is_async: false,
            is_generator: false,
        };
        let module = SemanticModule {
            source_id: source.id().as_str().to_owned(),
            source_sha256: source.content_sha256().to_owned(),
            functions: vec![parent_semantics, child_semantics],
            bindings: vec![
                SemanticBinding {
                    ir: value_binding,
                    lexical_scope: parent.scope.clone(),
                    declaration: span,
                },
                SemanticBinding {
                    ir: binding("binding_child", "child", BindingKind::Var),
                    lexical_scope: parent.scope.clone(),
                    declaration: span,
                },
            ],
        };
        let errors = lower(
            &source,
            &module,
            &ResolvedSyntaxProgram {
                functions: vec![parent, child],
                module: Default::default(),
            },
        )
        .expect_err("hoisted closure cannot snapshot a const still in its TDZ");
        assert_eq!(errors[0].code, "lowering.const_capture_tdz");
    }

    #[test]
    fn lowers_mutable_closure_capture_as_shared_environment_cell() {
        let source =
            source("function run() { let value = 1; return function child() { return value; }; }");
        let span = whole_span(&source);
        let binding = binding("binding_value", "value", BindingKind::Let);
        let parent = function_syntax(
            &source,
            vec![
                SyntaxStatement::Variable {
                    kind: SyntaxVariableKind::Let,
                    declarations: vec![SyntaxDeclarator {
                        binding: Some(binding.id.clone()),
                        name: binding.name.clone(),
                        initializer: Some(SyntaxExpression::Literal {
                            value: SyntaxLiteral::Number(1.0),
                            span,
                        }),
                        span,
                    }],
                    span,
                },
                SyntaxStatement::Return {
                    value: Some(SyntaxExpression::Function {
                        function: "fn_child".to_owned(),
                        captures: vec![binding.id.clone()],
                        span,
                    }),
                    span,
                },
            ],
            Vec::new(),
        );
        let child = ResolvedSyntaxFunction {
            id: "fn_child".to_owned(),
            name: "child".to_owned(),
            scope: "scope_2".to_owned(),
            parent_function_scope: Some(parent.scope.clone()),
            parameters: Vec::new(),
            captures: vec![binding.id.clone()],
            body: vec![SyntaxStatement::Return {
                value: Some(identifier("value", &binding.id, span)),
                span,
            }],
            span,
            is_arrow: false,
            is_async: false,
            is_generator: false,
            has_private_self_name: false,
        };
        let capture = Capture {
            id: "capture_value".to_owned(),
            source_environment: parent.scope.clone(),
            binding: binding.id.clone(),
            name: binding.name.clone(),
            mutable: true,
            semantic_type: SemanticType::Dynamic,
            representation: Representation::Boxed,
            mode: CaptureMode::Cell,
        };
        let parent_semantics = FunctionSemantics {
            id: parent.id.clone(),
            name: parent.name.clone(),
            environment: Environment {
                id: parent.scope.clone(),
                parent: parent.parent_function_scope.clone(),
                bindings: vec![binding.clone()],
            },
            parameters: Vec::new(),
            captures: Vec::new(),
            span,
            parent_function_scope: parent.parent_function_scope.clone(),
            is_arrow: false,
            is_async: false,
            is_generator: false,
        };
        let child_semantics = FunctionSemantics {
            id: child.id.clone(),
            name: child.name.clone(),
            environment: Environment {
                id: child.scope.clone(),
                parent: child.parent_function_scope.clone(),
                bindings: Vec::new(),
            },
            parameters: Vec::new(),
            captures: vec![capture.clone()],
            span,
            parent_function_scope: child.parent_function_scope.clone(),
            is_arrow: false,
            is_async: false,
            is_generator: false,
        };
        let module = SemanticModule {
            source_id: source.id().as_str().to_owned(),
            source_sha256: source.content_sha256().to_owned(),
            functions: vec![parent_semantics, child_semantics],
            bindings: vec![SemanticBinding {
                ir: binding.clone(),
                lexical_scope: parent.scope.clone(),
                declaration: span,
            }],
        };
        let documents = lower(
            &source,
            &module,
            &ResolvedSyntaxProgram {
                functions: vec![parent, child],
                module: Default::default(),
            },
        )
        .expect("captured mutable closure lowers");
        assert_eq!(documents.len(), 2);
        let closure = documents[0]
            .function
            .blocks
            .iter()
            .flat_map(|block| block.instructions.iter())
            .find(|instruction| instruction.op == "closure.create")
            .expect("closure.create emitted");
        let captures = closure
            .attributes
            .get("captures")
            .and_then(JsonValue::as_array)
            .expect("capture descriptors emitted");
        assert_eq!(captures.len(), 1);
        assert_eq!(
            captures[0].get("binding").and_then(JsonValue::as_str),
            Some(binding.id.as_str())
        );
        assert_eq!(
            captures[0].get("mode").and_then(JsonValue::as_str),
            Some("cell")
        );
        assert!(verify_document(&documents[0]).is_empty());
        assert!(verify_document(&documents[1]).is_empty());
    }

    #[test]
    fn typeof_unresolvable_identifier_returns_undefined_without_global_lookup() {
        let source = source("function run() { return typeof missing; }");
        let span = whole_span(&source);
        let syntax = function_syntax(
            &source,
            vec![SyntaxStatement::Return {
                value: Some(SyntaxExpression::Unary {
                    operator: SyntaxUnaryOperator::Typeof,
                    argument: Box::new(SyntaxExpression::Identifier {
                        name: "missing".to_owned(),
                        binding: None,
                        span,
                    }),
                    span,
                }),
                span,
            }],
            Vec::new(),
        );
        let module = semantic_module(&source, &syntax, Vec::new(), Vec::new());
        let documents = lower(
            &source,
            &module,
            &ResolvedSyntaxProgram {
                functions: vec![syntax],
                module: Default::default(),
            },
        )
        .expect("typeof unresolved identifier is valid JavaScript");
        let instructions = &documents[0].function.blocks[0].instructions;
        assert_eq!(instructions.len(), 1);
        assert_eq!(instructions[0].op, "js.constant");
        assert_eq!(
            instructions[0]
                .attributes
                .get("value")
                .and_then(JsonValue::as_str),
            Some("undefined")
        );
        assert!(documents[0].function.effect_entry.is_none());
        assert!(verify_document(&documents[0]).is_empty());
    }

    #[test]
    fn lowers_greater_than_with_dynamic_relational_effects() {
        let source = source("function run(left, right) { return left > right; }");
        let span = whole_span(&source);
        let left = binding("binding_left", "left", BindingKind::Parameter);
        let right = binding("binding_right", "right", BindingKind::Parameter);
        let syntax = function_syntax(
            &source,
            vec![SyntaxStatement::Return {
                value: Some(SyntaxExpression::Binary {
                    operator: SyntaxBinaryOperator::GreaterThan,
                    left: Box::new(identifier("left", &left.id, span)),
                    right: Box::new(identifier("right", &right.id, span)),
                    span,
                }),
                span,
            }],
            vec![left.id.clone(), right.id.clone()],
        );
        let module = semantic_module(
            &source,
            &syntax,
            vec![left.clone(), right.clone()],
            vec![
                parameter("param_run_0", "left", &left.id),
                parameter("param_run_1", "right", &right.id),
            ],
        );
        let documents = lower(
            &source,
            &module,
            &ResolvedSyntaxProgram {
                functions: vec![syntax],
                module: Default::default(),
            },
        )
        .expect("greater-than comparison lowers");
        let function = &documents[0].function;
        let operation = function
            .blocks
            .iter()
            .flat_map(|block| block.instructions.iter())
            .find(|instruction| instruction.op == "js.greater_than")
            .expect("greater-than operation emitted");
        assert!(operation.effects.may_throw);
        assert!(operation.effects.may_call_host);
        assert!(operation.effect_in.is_some());
        assert!(operation.effect_out.is_some());
        assert_eq!(
            function
                .return_signature
                .as_ref()
                .map(|signature| signature.semantic_type),
            Some(SemanticType::Boolean)
        );
        assert!(verify_document(&documents[0]).is_empty());
    }

    #[test]
    fn lowers_uninitialized_let_and_hoisted_var_without_losing_tdz_semantics() {
        let let_source = source("function run() { let local; return local; }");
        let span = whole_span(&let_source);
        let local = binding("binding_local", "local", BindingKind::Let);
        let syntax = function_syntax(
            &let_source,
            vec![
                SyntaxStatement::Variable {
                    kind: SyntaxVariableKind::Let,
                    declarations: vec![SyntaxDeclarator {
                        binding: Some(local.id.clone()),
                        name: local.name.clone(),
                        initializer: None,
                        span,
                    }],
                    span,
                },
                SyntaxStatement::Return {
                    value: Some(identifier("local", &local.id, span)),
                    span,
                },
            ],
            Vec::new(),
        );
        let module = semantic_module(&let_source, &syntax, vec![local.clone()], Vec::new());
        let documents = lower(
            &let_source,
            &module,
            &ResolvedSyntaxProgram {
                functions: vec![syntax],
                module: Default::default(),
            },
        )
        .expect("uninitialized let is initialized to undefined");
        let instructions: Vec<_> = documents[0]
            .function
            .blocks
            .iter()
            .flat_map(|block| block.instructions.iter())
            .collect();
        assert!(instructions.iter().any(|instruction| {
            instruction.op == "js.constant"
                && instruction
                    .attributes
                    .get("literal")
                    .and_then(JsonValue::as_str)
                    == Some("undefined")
        }));
        assert!(instructions.iter().any(|instruction| {
            instruction.op == "env.store"
                && instruction
                    .attributes
                    .get("binding")
                    .and_then(JsonValue::as_str)
                    == Some(local.id.as_str())
        }));
        assert!(instructions.iter().any(|instruction| {
            instruction.op == "env.load"
                && instruction
                    .attributes
                    .get("binding")
                    .and_then(JsonValue::as_str)
                    == Some(local.id.as_str())
        }));
        assert!(verify_document(&documents[0]).is_empty());

        let var_source = source("function run() { return hoisted; var hoisted; }");
        let var_span = whole_span(&var_source);
        let hoisted = binding("binding_hoisted", "hoisted", BindingKind::Var);
        let var_syntax = function_syntax(
            &var_source,
            vec![
                SyntaxStatement::Return {
                    value: Some(identifier("hoisted", &hoisted.id, var_span)),
                    span: var_span,
                },
                SyntaxStatement::Variable {
                    kind: SyntaxVariableKind::Var,
                    declarations: vec![SyntaxDeclarator {
                        binding: Some(hoisted.id.clone()),
                        name: hoisted.name.clone(),
                        initializer: None,
                        span: var_span,
                    }],
                    span: var_span,
                },
            ],
            Vec::new(),
        );
        let var_module =
            semantic_module(&var_source, &var_syntax, vec![hoisted.clone()], Vec::new());
        let var_documents = lower(
            &var_source,
            &var_module,
            &ResolvedSyntaxProgram {
                functions: vec![var_syntax],
                module: Default::default(),
            },
        )
        .expect("var binding is initialized before user statements");
        let var_instructions: Vec<_> = var_documents[0]
            .function
            .blocks
            .iter()
            .flat_map(|block| block.instructions.iter())
            .collect();
        let undefined_store = var_instructions
            .iter()
            .position(|instruction| {
                instruction.op == "env.store"
                    && instruction
                        .attributes
                        .get("binding")
                        .and_then(JsonValue::as_str)
                        == Some(hoisted.id.as_str())
            })
            .expect("hoisted var initialized at function entry");
        let load = var_instructions
            .iter()
            .position(|instruction| {
                instruction.op == "env.load"
                    && instruction
                        .attributes
                        .get("binding")
                        .and_then(JsonValue::as_str)
                        == Some(hoisted.id.as_str())
            })
            .expect("hoisted var read is lowered");
        assert!(undefined_store < load);
        assert!(verify_document(&var_documents[0]).is_empty());
    }

    #[test]
    fn rejects_const_read_before_its_initializer() {
        let source = source("function run() { return value; const value = 1; }");
        let span = whole_span(&source);
        let value = binding("binding_value", "value", BindingKind::Const);
        let syntax = function_syntax(
            &source,
            vec![
                SyntaxStatement::Return {
                    value: Some(identifier("value", &value.id, span)),
                    span,
                },
                SyntaxStatement::Variable {
                    kind: SyntaxVariableKind::Const,
                    declarations: vec![SyntaxDeclarator {
                        binding: Some(value.id.clone()),
                        name: value.name.clone(),
                        initializer: Some(SyntaxExpression::Literal {
                            value: SyntaxLiteral::Number(1.0),
                            span,
                        }),
                        span,
                    }],
                    span,
                },
            ],
            Vec::new(),
        );
        let module = semantic_module(&source, &syntax, vec![value], Vec::new());
        let errors = lower(
            &source,
            &module,
            &ResolvedSyntaxProgram {
                functions: vec![syntax],
                module: Default::default(),
            },
        )
        .expect_err("read before const initialization is a TDZ error");
        assert_eq!(errors[0].code, "lowering.const_tdz");
    }

    #[test]
    fn preserves_frontend_source_identity_in_core_ir_source_table() {
        let source = source("const value = 1;");
        let file = core_source_file(&source).expect("relative memory URI lowers");
        assert_eq!(file.id, source.id().as_str());
        assert_eq!(file.sha256.as_deref(), Some(source.content_sha256()));
    }

    #[test]
    fn preserves_nested_binding_identity_in_function_scope_metadata() {
        let source = source("function run() { { let inner = 1; } }");
        let span = whole_span(&source);
        let inner = binding("binding_inner", "inner", BindingKind::Let);
        let syntax = function_syntax(
            &source,
            vec![SyntaxStatement::Block {
                scope: Some("scope_inner_block".to_owned()),
                body: vec![SyntaxStatement::Variable {
                    kind: SyntaxVariableKind::Let,
                    declarations: vec![SyntaxDeclarator {
                        binding: Some(inner.id.clone()),
                        name: inner.name.clone(),
                        initializer: Some(SyntaxExpression::Literal {
                            value: SyntaxLiteral::Number(1.0),
                            span,
                        }),
                        span,
                    }],
                    span,
                }],
                span,
            }],
            Vec::new(),
        );
        let mut module = semantic_module(&source, &syntax, vec![inner.clone()], Vec::new());
        module.bindings[0].lexical_scope = "scope_inner_block".to_owned();
        let documents = lower(
            &source,
            &module,
            &ResolvedSyntaxProgram {
                functions: vec![syntax],
                module: Default::default(),
            },
        )
        .expect("block-scoped declaration lowers");
        let store = documents[0].function.blocks[0]
            .instructions
            .iter()
            .find(|instruction| {
                instruction.op == "env.store"
                    && instruction
                        .attributes
                        .get("binding")
                        .and_then(JsonValue::as_str)
                        == Some(inner.id.as_str())
            })
            .expect("block binding initializer store exists");
        let scope = match &store.source {
            Source::User { scope, .. } | Source::Generated { scope, .. } => scope.as_deref(),
        };
        assert_eq!(scope, Some("scope_1"));
        let source_bindings = match &store.source {
            Source::User { bindings, .. } | Source::Generated { bindings, .. } => bindings,
        };
        assert!(source_bindings.contains(&inner.id));
        assert!(verify_document(&documents[0]).is_empty());
    }

    #[test]
    fn preserves_memory_uri_paths_and_rejects_absolute_file_uris() {
        let memory = SourceFile::new(
            "memory-source",
            "memory:///src/game/main.js",
            None,
            "const value = 1;",
        )
        .expect("memory source is valid");
        let file = core_source_file(&memory).expect("memory URI maps to a relative path");
        assert_eq!(file.path, "src/game/main.js");

        let relative = SourceFile::new(
            "relative-source",
            "src/game/main.js",
            None,
            "const value = 1;",
        )
        .expect("relative source is valid");
        let file = core_source_file(&relative).expect("plain relative path is preserved");
        assert_eq!(file.path, "src/game/main.js");

        let absolute = SourceFile::new(
            "absolute-source",
            "file:///workspace/project/src/game/main.js",
            None,
            "const value = 1;",
        )
        .expect("source descriptor accepts a URI");
        let error = core_source_file(&absolute)
            .expect_err("absolute file URIs cannot be represented as repository-relative paths");
        assert_eq!(error.code, "lowering.source_path");
    }

    #[test]
    fn rejects_block_scoped_closure_capture_without_environment_materialization() {
        let source = source("function run() { { let value = 1; return () => value; } }");
        let span = whole_span(&source);
        let binding = binding("binding_block_value", "value", BindingKind::Let);
        let parent = function_syntax(
            &source,
            vec![SyntaxStatement::Block {
                scope: Some("scope_2".to_owned()),
                body: vec![
                    SyntaxStatement::Variable {
                        kind: SyntaxVariableKind::Let,
                        declarations: vec![SyntaxDeclarator {
                            binding: Some(binding.id.clone()),
                            name: binding.name.clone(),
                            initializer: Some(SyntaxExpression::Literal {
                                value: SyntaxLiteral::Number(1.0),
                                span,
                            }),
                            span,
                        }],
                        span,
                    },
                    SyntaxStatement::Return {
                        value: Some(SyntaxExpression::Function {
                            function: "fn_block_child".to_owned(),
                            captures: vec![binding.id.clone()],
                            span,
                        }),
                        span,
                    },
                ],
                span,
            }],
            Vec::new(),
        );
        let child = ResolvedSyntaxFunction {
            id: "fn_block_child".to_owned(),
            name: "<block-child>".to_owned(),
            scope: "scope_3".to_owned(),
            parent_function_scope: Some(parent.scope.clone()),
            parameters: Vec::new(),
            captures: vec![binding.id.clone()],
            body: vec![SyntaxStatement::Return {
                value: Some(identifier("value", &binding.id, span)),
                span,
            }],
            span,
            is_arrow: true,
            is_async: false,
            is_generator: false,
            has_private_self_name: false,
        };
        let capture = Capture {
            id: "capture_block_value".to_owned(),
            source_environment: parent.scope.clone(),
            binding: binding.id.clone(),
            name: binding.name.clone(),
            mutable: true,
            semantic_type: SemanticType::Dynamic,
            representation: Representation::Boxed,
            mode: CaptureMode::Cell,
        };
        let parent_semantics = FunctionSemantics {
            id: parent.id.clone(),
            name: parent.name.clone(),
            environment: Environment {
                id: parent.scope.clone(),
                parent: parent.parent_function_scope.clone(),
                bindings: vec![binding.clone()],
            },
            parameters: Vec::new(),
            captures: Vec::new(),
            span,
            parent_function_scope: parent.parent_function_scope.clone(),
            is_arrow: false,
            is_async: false,
            is_generator: false,
        };
        let child_semantics = FunctionSemantics {
            id: child.id.clone(),
            name: child.name.clone(),
            environment: Environment {
                id: child.scope.clone(),
                parent: child.parent_function_scope.clone(),
                bindings: Vec::new(),
            },
            parameters: Vec::new(),
            captures: vec![capture],
            span,
            parent_function_scope: child.parent_function_scope.clone(),
            is_arrow: true,
            is_async: false,
            is_generator: false,
        };
        let module = SemanticModule {
            source_id: source.id().as_str().to_owned(),
            source_sha256: source.content_sha256().to_owned(),
            functions: vec![parent_semantics, child_semantics],
            bindings: vec![SemanticBinding {
                ir: binding,
                lexical_scope: "scope_2".to_owned(),
                declaration: span,
            }],
        };
        let error = lower(
            &source,
            &module,
            &ResolvedSyntaxProgram {
                functions: vec![parent, child],
                module: Default::default(),
            },
        )
        .expect_err("block-scoped capture requires a per-block environment");
        assert_eq!(error[0].code, "lowering.block_capture_lifetime");
    }

    #[test]
    fn lowers_c_style_for_loop_with_effect_backedge() {
        let source =
            source("function run() { for (let index = 0; index < 2; index = index + 1) {} }");
        let span = whole_span(&source);
        let index = binding("binding_index", "index", BindingKind::Let);
        let initializer = SyntaxForInitializer::Variable {
            kind: SyntaxVariableKind::Let,
            declarations: vec![SyntaxDeclarator {
                binding: Some(index.id.clone()),
                name: index.name.clone(),
                initializer: Some(SyntaxExpression::Literal {
                    value: SyntaxLiteral::Number(0.0),
                    span,
                }),
                span,
            }],
            span,
        };
        let test = SyntaxExpression::Binary {
            operator: SyntaxBinaryOperator::LessThan,
            left: Box::new(identifier("index", &index.id, span)),
            right: Box::new(SyntaxExpression::Literal {
                value: SyntaxLiteral::Number(2.0),
                span,
            }),
            span,
        };
        let update = SyntaxExpression::Assign {
            target: SyntaxAssignmentTarget::Identifier {
                name: "index".to_owned(),
                binding: Some(index.id.clone()),
                span,
            },
            value: Box::new(SyntaxExpression::Binary {
                operator: SyntaxBinaryOperator::Add,
                left: Box::new(identifier("index", &index.id, span)),
                right: Box::new(SyntaxExpression::Literal {
                    value: SyntaxLiteral::Number(1.0),
                    span,
                }),
                span,
            }),
            span,
        };
        let syntax = function_syntax(
            &source,
            vec![SyntaxStatement::For {
                scope: Some("scope_for".to_owned()),
                initializer: Some(initializer),
                test: Some(test),
                update: Some(update),
                body: Box::new(SyntaxStatement::Empty { span }),
                span,
            }],
            Vec::new(),
        );
        let mut module = semantic_module(&source, &syntax, vec![index.clone()], Vec::new());
        module.bindings[0].lexical_scope = "scope_for".to_owned();
        let documents = lower(
            &source,
            &module,
            &ResolvedSyntaxProgram {
                functions: vec![syntax],
                module: Default::default(),
            },
        )
        .expect("C-style for loop lowers to verified CFG");
        let function = &documents[0].function;
        assert_eq!(function.blocks.len(), 5);
        let initializer_store = function.blocks[0]
            .instructions
            .iter()
            .find(|instruction| {
                instruction.op == "env.store"
                    && instruction
                        .attributes
                        .get("binding")
                        .and_then(JsonValue::as_str)
                        == Some(index.id.as_str())
            })
            .expect("for-loop binding initialized in its lexical scope");
        let initializer_scope = match &initializer_store.source {
            Source::User { scope, .. } | Source::Generated { scope, .. } => scope.as_deref(),
        };
        assert_eq!(initializer_scope, Some("scope_1"));
        let initializer_bindings = match &initializer_store.source {
            Source::User { bindings, .. } | Source::Generated { bindings, .. } => bindings,
        };
        assert!(initializer_bindings.contains(&index.id));
        let header = function
            .blocks
            .iter()
            .find(|block| matches!(block.terminator, Terminator::Branch { .. }))
            .expect("for loop header branch exists");
        let body_id = match &header.terminator {
            Terminator::Branch { then_edge, .. } => then_edge.target.as_str(),
            _ => unreachable!(),
        };
        let body = function
            .blocks
            .iter()
            .find(|block| block.id == body_id)
            .expect("for loop body exists");
        let update_id = match &body.terminator {
            Terminator::Jump { target, .. } => target.as_str(),
            _ => panic!("for body must jump to update block"),
        };
        let update_block = function
            .blocks
            .iter()
            .find(|block| block.id == update_id)
            .expect("for update block exists");
        assert!(matches!(
            update_block.terminator,
            Terminator::Jump { ref target, .. } if target == &header.id
        ));
        assert!(
            header
                .parameters
                .iter()
                .any(|parameter| { matches!(parameter, BlockParameter::Effect(_)) })
        );
        assert!(verify_document(&documents[0]).is_empty());
    }

    #[test]
    fn hoists_function_declarations_before_the_first_read() {
        let source = source("function run() { return child; function child() { return 7; } }");
        let span = whole_span(&source);
        let child_binding = binding("binding_child", "child", BindingKind::Var);
        let parent = function_syntax(
            &source,
            vec![
                SyntaxStatement::Return {
                    value: Some(identifier("child", &child_binding.id, span)),
                    span,
                },
                SyntaxStatement::FunctionDeclaration {
                    function: "fn_child".to_owned(),
                    binding: Some(child_binding.id.clone()),
                    name: "child".to_owned(),
                    span,
                },
            ],
            Vec::new(),
        );
        let child = ResolvedSyntaxFunction {
            id: "fn_child".to_owned(),
            name: "child".to_owned(),
            scope: "scope_2".to_owned(),
            parent_function_scope: Some(parent.scope.clone()),
            parameters: Vec::new(),
            captures: Vec::new(),
            body: vec![SyntaxStatement::Return {
                value: Some(SyntaxExpression::Literal {
                    value: SyntaxLiteral::Number(7.0),
                    span,
                }),
                span,
            }],
            span,
            is_arrow: false,
            is_async: false,
            is_generator: false,
            has_private_self_name: false,
        };
        let parent_semantics = FunctionSemantics {
            id: parent.id.clone(),
            name: parent.name.clone(),
            environment: Environment {
                id: parent.scope.clone(),
                parent: parent.parent_function_scope.clone(),
                bindings: vec![child_binding.clone()],
            },
            parameters: Vec::new(),
            captures: Vec::new(),
            span,
            parent_function_scope: parent.parent_function_scope.clone(),
            is_arrow: false,
            is_async: false,
            is_generator: false,
        };
        let child_semantics = FunctionSemantics {
            id: child.id.clone(),
            name: child.name.clone(),
            environment: Environment {
                id: child.scope.clone(),
                parent: child.parent_function_scope.clone(),
                bindings: Vec::new(),
            },
            parameters: Vec::new(),
            captures: Vec::new(),
            span,
            parent_function_scope: child.parent_function_scope.clone(),
            is_arrow: false,
            is_async: false,
            is_generator: false,
        };
        let module = SemanticModule {
            source_id: source.id().as_str().to_owned(),
            source_sha256: source.content_sha256().to_owned(),
            functions: vec![parent_semantics, child_semantics],
            bindings: vec![SemanticBinding {
                ir: child_binding.clone(),
                lexical_scope: parent.scope.clone(),
                declaration: span,
            }],
        };
        let documents = lower(
            &source,
            &module,
            &ResolvedSyntaxProgram {
                functions: vec![parent, child],
                module: Default::default(),
            },
        )
        .expect("hoisted function declaration lowers");
        assert_eq!(documents.len(), 2);
        let instructions = &documents[0].function.blocks[0].instructions;
        let closure = instructions
            .iter()
            .position(|instruction| instruction.op == "closure.create")
            .expect("function closure is created before user statements");
        let store = instructions
            .iter()
            .position(|instruction| {
                instruction.op == "env.store"
                    && instruction
                        .attributes
                        .get("binding")
                        .and_then(JsonValue::as_str)
                        == Some(child_binding.id.as_str())
                    && instruction
                        .result
                        .as_ref()
                        .is_some_and(|result| result.semantic_type == SemanticType::Function)
            })
            .expect("hoisted function binding receives a closure");
        let load = instructions
            .iter()
            .position(|instruction| {
                instruction.op == "env.load"
                    && instruction
                        .attributes
                        .get("binding")
                        .and_then(JsonValue::as_str)
                        == Some(child_binding.id.as_str())
            })
            .expect("the earlier source return reads the initialized binding");
        assert!(closure < store && store < load);
        assert!(verify_document(&documents[0]).is_empty());
        assert!(verify_document(&documents[1]).is_empty());
    }

    #[test]
    fn preserves_const_binding_identity_on_the_ssa_definition() {
        let source = source("function run() { const value = 1; return value; }");
        let span = whole_span(&source);
        let value = binding("binding_const_value", "value", BindingKind::Const);
        let syntax = function_syntax(
            &source,
            vec![
                SyntaxStatement::Variable {
                    kind: SyntaxVariableKind::Const,
                    declarations: vec![SyntaxDeclarator {
                        binding: Some(value.id.clone()),
                        name: value.name.clone(),
                        initializer: Some(SyntaxExpression::Literal {
                            value: SyntaxLiteral::Number(1.0),
                            span,
                        }),
                        span,
                    }],
                    span,
                },
                SyntaxStatement::Return {
                    value: Some(identifier("value", &value.id, span)),
                    span,
                },
            ],
            Vec::new(),
        );
        let module = semantic_module(&source, &syntax, vec![value.clone()], Vec::new());
        let documents = lower(
            &source,
            &module,
            &ResolvedSyntaxProgram {
                functions: vec![syntax],
                module: Default::default(),
            },
        )
        .expect("const initializer lowers to an SSA value");
        let definition = documents[0]
            .function
            .blocks
            .iter()
            .flat_map(|block| block.instructions.iter())
            .find(|instruction| instruction.op == "js.constant")
            .expect("const literal SSA definition exists");
        let bindings = match &definition.source {
            Source::User { bindings, .. } | Source::Generated { bindings, .. } => bindings,
        };
        assert!(bindings.contains(&value.id));
        assert!(verify_document(&documents[0]).is_empty());
    }
}
