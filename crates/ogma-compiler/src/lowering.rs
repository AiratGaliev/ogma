//! Canonical AST/semantic to Core IR v1 lowering.
//!
//! The compiler consumes the frontend-owned syntax projection. Oxc types and
//! lifetimes do not cross this crate boundary; emitted documents are verified
//! against the canonical Core IR v1 model before they are returned.

use std::collections::{BTreeMap, HashMap, HashSet};

use ogma_core_ir::{
    BasicBlock, Binding, BindingKind, BlockParameter, Capture, CaptureMode, Edge, Effects,
    EffectDefinition, ExceptionFlow, Function, FunctionDocument, Instruction, Representation,
    ReturnSignature, SemanticType, Source, SourceFile as CoreSourceFile,
    SourceLanguage, SourcePosition as CoreSourcePosition, Terminator, UnreachableReason,
    ValueDefinition, verify_document,
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
    let source_file = match core_source_file(source) {
        Ok(file) => file,
        Err(diagnostic) => return Err(vec![diagnostic]),
    };
    let semantic_by_id: HashMap<_, _> = semantic
        .functions
        .iter()
        .map(|function| (function.id.as_str(), function))
        .collect();
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
            source,
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

fn core_source_file(source: &SourceFile) -> Result<CoreSourceFile, LoweringDiagnostic> {
    let uri = source.uri();
    let mut path = if let Some((scheme, rest)) = uri.split_once("://") {
        if scheme.is_empty() {
            String::new()
        } else {
            rest.trim_start_matches('/').to_owned()
        }
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
        id: format!("src_{}", &digest[..16]),
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

struct FunctionCompiler<'a> {
    source: &'a SourceFile,
    source_file: &'a CoreSourceFile,
    semantic_module: &'a SemanticModule,
    syntax_registry: &'a HashMap<&'a str, &'a ResolvedSyntaxFunction>,
    semantic: &'a FunctionSemantics,
    syntax: &'a ResolvedSyntaxFunction,
    local_bindings: HashMap<String, Binding>,
    captures_by_binding: HashMap<String, Capture>,
    const_initializers: HashMap<String, SyntaxExpression>,
    const_values: HashMap<String, ValueRef>,
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
        source: &'a SourceFile,
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
                        "capture of block-scoped binding {:?} requires per-block/per-iteration environment materialization",
                        binding.ir.name
                    ),
                ));
            }
            if binding.ir.kind == BindingKind::Const {
                return Err(diag(
                    syntax,
                    "lowering.const_capture_lifetime",
                    Some(binding.declaration),
                    format!(
                        "capture of immutable binding {:?} requires a materialized value environment slot",
                        binding.ir.name
                    ),
                ));
            }
        }

        let const_initializers = collect_const_initializers(&syntax.body);
        let effect_enabled = semantic.environment.bindings.iter().any(|binding| {
            matches!(
                binding.kind,
                BindingKind::Var
                    | BindingKind::Let
                    | BindingKind::Parameter
                    | BindingKind::Catch
            )
        }) || !semantic.captures.is_empty()
            || statements_need_effects(&syntax.body);

        Ok(Self {
            source,
            source_file,
            semantic_module,
            syntax_registry,
            semantic,
            syntax,
            local_bindings,
            captures_by_binding,
            const_initializers,
            const_values: HashMap::new(),
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
            self.terminate_return(
                None,
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
                source: self.generated_source("entry block is finalized during lowering", Vec::new()),
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
                source: self.generated_source("basic block terminator is finalized during lowering", Vec::new()),
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
            self.blocks[block].parameters.last().and_then(|parameter| match parameter {
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
        self.blocks[block].instructions.push(Instruction {
            id: self.next_instruction_id(),
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
        });
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
            .unwrap_or_else(|| self.generated_source("materialize JavaScript constant", Vec::new()));
        self.emit_instruction(
            "js.constant",
            Vec::new(),
            Some(ty),
            attributes,
            Effects::default(),
            source,
        )?
        .ok_or_else(|| diag(self.syntax, "lowering.constant_result", span, "js.constant produced no value"))
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
        .ok_or_else(|| diag(self.syntax, "lowering.env_load_result", Some(span), "env.load produced no value"))
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
            (None, None) => self.generated_source("initialize lexical binding", vec![binding_id.to_owned()]),
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
        .ok_or_else(|| diag(self.syntax, "lowering.env_store_result", span, "env.store produced no assigned value"))
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
        .ok_or_else(|| diag(self.syntax, "lowering.to_boolean_result", Some(span), "js.to_boolean produced no value"))
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
        .ok_or_else(|| diag(self.syntax, "lowering.binary_result", Some(span), format!("{op} produced no value")))
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
                .ok_or_else(|| diag(self.syntax, "lowering.property_result", Some(span), "js.get_property produced no value"))
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
                .ok_or_else(|| diag(self.syntax, "lowering.element_result", Some(span), "js.get_element produced no value"))
            }
        }
    }

    fn emit_member_set(
        &mut self,
        object: ValueRef,
        key: &SyntaxMemberKey,
        value: ValueRef,
        span: SourceSpan,
    ) -> Result<ValueRef, LoweringDiagnostic> {
        match key {
            SyntaxMemberKey::Static(key) => {
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
                .ok_or_else(|| diag(self.syntax, "lowering.set_property_result", Some(span), "js.set_property produced no value"))
            }
            SyntaxMemberKey::Computed(key) => {
                let key = self.emit_expression(key)?;
                self.emit_instruction(
                    "js.set_element",
                    vec![object.id, key.id, value.id.clone()],
                    Some(value.ty),
                    BTreeMap::new(),
                    dynamic_effects(),
                    self.user_source(span, Vec::new()),
                )?
                .ok_or_else(|| diag(self.syntax, "lowering.set_element_result", Some(span), "js.set_element produced no value"))
            }
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
                if binding.kind == BindingKind::Const {
                    return self.fail(
                        "lowering.const_capture_lifetime",
                        Some(span),
                        format!("closure captures const binding {:?}, whose SSA value is not materialized as an environment slot", binding.name),
                    );
                }
                let Some(source_binding) = self.semantic_module.bindings.iter()
                    .find(|item| item.ir.id == binding.id)
                else {
                    return self.fail(
                        "lowering.capture_source_missing",
                        Some(span),
                        format!("no source metadata for captured binding {:?}", binding.id),
                    );
                };
                if source_binding.lexical_scope != self.semantic.environment.id {
                    return self.fail(
                        "lowering.block_capture_lifetime",
                        Some(source_binding.declaration),
                        format!("captured binding {:?} is not owned by the enclosing function environment", binding.name),
                    );
                }
                binding.id.clone()
            } else if let Some(parent_capture) = self.captures_by_binding.get(&capture.binding) {
                parent_capture.id.clone()
            } else {
                return self.fail(
                    "lowering.capture_unavailable",
                    Some(span),
                    format!("enclosing function cannot provide captured binding {:?}", capture.binding),
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
        attributes.insert("function".to_owned(), JsonValue::String(child_id.to_owned()));
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
        .ok_or_else(|| diag(self.syntax, "lowering.closure_result", Some(span), "closure.create produced no function value"))
    }

    fn emit_expression(&mut self, expression: &SyntaxExpression) -> Result<ValueRef, LoweringDiagnostic> {
        match expression {
            SyntaxExpression::Literal { value, span } => self.emit_literal(value, *span),
            SyntaxExpression::Identifier { name, binding, span } => {
                if let Some(binding) = binding {
                    if let Some(value) = self.const_values.get(binding) {
                        return Ok(value.clone());
                    }
                    if self.local_bindings.get(binding).is_some_and(|item| item.kind == BindingKind::Const) {
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
                    format!("unresolved global {name:?} has no Core IR host/global binding contract"),
                )
            }
            SyntaxExpression::Parenthesized { expression, .. } => self.emit_expression(expression),
            SyntaxExpression::Binary { operator, left, right, span } => {
                let left = self.emit_expression(left)?;
                let right = self.emit_expression(right)?;
                match operator {
                    SyntaxBinaryOperator::Add => self.emit_binary(
                        "js.add", left, right, ValueType::DYNAMIC, dynamic_effects(), *span,
                    ),
                    SyntaxBinaryOperator::StrictEqual => self.emit_binary(
                        "js.strict_equal", left, right, ValueType::BOOLEAN, Effects::default(), *span,
                    ),
                    SyntaxBinaryOperator::StrictNotEqual => {
                        let equal = self.emit_binary(
                            "js.strict_equal", left, right, ValueType::BOOLEAN, Effects::default(), *span,
                        )?;
                        let false_value = self.emit_literal(&SyntaxLiteral::Boolean(false), *span)?;
                        self.emit_binary(
                            "js.strict_equal", equal, false_value, ValueType::BOOLEAN, Effects::default(), *span,
                        )
                    }
                    SyntaxBinaryOperator::LessThan => self.emit_binary(
                        "js.less_than", left, right, ValueType::BOOLEAN, dynamic_effects(), *span,
                    ),
                    SyntaxBinaryOperator::GreaterThan => self.fail(
                        "lowering.greater_than_not_in_core_ir",
                        Some(*span),
                        "greater-than requires its own Core IR operation to preserve ECMAScript coercion order",
                    ),
                }
            }
            SyntaxExpression::Unary { operator, argument, span } => match operator {
                SyntaxUnaryOperator::LogicalNot => {
                    let value = self.emit_expression(argument)?;
                    let boolean = self.emit_to_boolean(value, *span)?;
                    let false_value = self.emit_literal(&SyntaxLiteral::Boolean(false), *span)?;
                    self.emit_binary(
                        "js.strict_equal", boolean, false_value, ValueType::BOOLEAN, Effects::default(), *span,
                    )
                }
                SyntaxUnaryOperator::Typeof => {
                    let value = self.emit_expression(argument)?;
                    self.emit_instruction(
                        "js.typeof",
                        vec![value.id],
                        Some(ValueType::STRING),
                        BTreeMap::new(),
                        Effects::default(),
                        self.user_source(*span, Vec::new()),
                    )?
                    .ok_or_else(|| diag(self.syntax, "lowering.typeof_result", Some(*span), "js.typeof produced no value"))
                }
                SyntaxUnaryOperator::Positive => {
                    if let SyntaxExpression::Literal { value: SyntaxLiteral::Number(number), .. } = argument.as_ref() {
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
                    .ok_or_else(|| diag(self.syntax, "lowering.to_number_result", Some(*span), "js.to_number produced no value"))
                }
                SyntaxUnaryOperator::Negate => {
                    if let SyntaxExpression::Literal { value: SyntaxLiteral::Number(number), .. } = argument.as_ref() {
                        return self.emit_literal(&SyntaxLiteral::Number(-*number), *span);
                    }
                    self.fail(
                        "lowering.unary_negation_unsupported",
                        Some(*span),
                        "dynamic unary negation requires a dedicated Core IR operation",
                    )
                }
            },
            SyntaxExpression::Call { callee, arguments, span } => {
                let (callee_value, this_value) = match callee.as_ref() {
                    SyntaxExpression::Member { object, key, span: member_span } => {
                        let object_value = self.emit_expression(object)?;
                        let callee_value = self.emit_member_get(object_value.clone(), key, *member_span)?;
                        (callee_value, object_value)
                    }
                    _ => {
                        let callee_value = self.emit_expression(callee)?;
                        let this_value = self.emit_undefined("materialize undefined call receiver")?;
                        (callee_value, this_value)
                    }
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
                .ok_or_else(|| diag(self.syntax, "lowering.call_result", Some(*span), "js.call produced no value"))
            }
            SyntaxExpression::Member { object, key, span } => {
                let object = self.emit_expression(object)?;
                self.emit_member_get(object, key, *span)
            }
            SyntaxExpression::Assign { target, value, span } => {
                let value = self.emit_expression(value)?;
                self.emit_assignment_target(target, value, *span)
            }
            SyntaxExpression::Update { target, span, .. } => self.fail(
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

    fn emit_literal(&mut self, literal: &SyntaxLiteral, span: SourceSpan) -> Result<ValueRef, LoweringDiagnostic> {
        match literal {
            SyntaxLiteral::Null => self.emit_constant(
                "null", JsonValue::Null,
                ValueType { semantic: SemanticType::Null, representation: Representation::Boxed },
                Some(span),
            ),
            SyntaxLiteral::Boolean(value) => self.emit_constant("boolean", json!(value), ValueType::BOOLEAN, Some(span)),
            SyntaxLiteral::Number(value) => self.emit_constant("number", number_literal_json(*value), ValueType::NUMBER, Some(span)),
            SyntaxLiteral::String(value) => self.emit_constant("string", JsonValue::String(value.clone()), ValueType::STRING, Some(span)),
            SyntaxLiteral::Bigint(value) => self.emit_constant(
                "bigint", JsonValue::String(value.clone()),
                ValueType { semantic: SemanticType::Bigint, representation: Representation::BigintRef },
                Some(span),
            ),
        }
    }

    fn emit_assignment_target(
        &mut self,
        target: &SyntaxAssignmentTarget,
        value: ValueRef,
        span: SourceSpan,
    ) -> Result<ValueRef, LoweringDiagnostic> {
        match target {
            SyntaxAssignmentTarget::Identifier { binding: Some(binding), .. } => {
                self.emit_env_store(binding, value, Some(span), None)
            }
            SyntaxAssignmentTarget::Identifier { name, binding: None, .. } => self.fail(
                "lowering.unresolved_assignment_target",
                Some(span),
                format!("assignment to unresolved global {name:?} is unsupported"),
            ),
            SyntaxAssignmentTarget::Member { object, key, .. } => {
                let object = self.emit_expression(object)?;
                self.emit_member_set(object, key, value, span)
            }
            SyntaxAssignmentTarget::Unsupported { syntax, span } => self.fail(
                "lowering.unsupported_assignment_target",
                Some(*span),
                format!("assignment target {syntax:?} is unsupported"),
            ),
        }
    }

    fn compile_sequence(&mut self, statements: &[SyntaxStatement]) -> Result<(), LoweringDiagnostic> {
        self.emit_hoisted_functions(statements)?;
        for statement in statements {
            if self.current.is_none() {
                break;
            }
            self.compile_statement(statement)?;
        }
        Ok(())
    }

    fn emit_hoisted_functions(&mut self, statements: &[SyntaxStatement]) -> Result<(), LoweringDiagnostic> {
        for statement in statements {
            if let SyntaxStatement::FunctionDeclaration { function, binding: Some(binding), span, .. } = statement {
                if self.hoisted_functions.insert(binding.clone()) {
                    let closure = self.emit_closure(function, *span)?;
                    self.emit_env_store(binding, closure, Some(*span), None)?;
                }
            }
        }
        Ok(())
    }

    fn compile_statement(&mut self, statement: &SyntaxStatement) -> Result<(), LoweringDiagnostic> {
        match statement {
            SyntaxStatement::Block { body, .. } => self.compile_sequence(body),
            SyntaxStatement::Variable { kind, declarations, span } => {
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
                                let value = self.emit_expression(initializer)?;
                                self.emit_env_store(binding_id, value, Some(declaration.span), None)?;
                            }
                        }
                        SyntaxVariableKind::Let => {
                            let value = if let Some(initializer) = &declaration.initializer {
                                self.emit_expression(initializer)?
                            } else {
                                self.emit_undefined("initialize uninitialized let declaration")?
                            };
                            self.emit_env_store(binding_id, value, Some(declaration.span), None)?;
                        }
                        SyntaxVariableKind::Const => {
                            let Some(initializer) = &declaration.initializer else {
                                return self.fail(
                                    "lowering.const_without_initializer",
                                    Some(declaration.span),
                                    "const declaration has no initializer",
                                );
                            };
                            let value = self.emit_expression(initializer)?;
                            self.const_values.insert(binding_id.to_owned(), value);
                        }
                        SyntaxVariableKind::Unsupported => unreachable!(),
                    }
                }
                Ok(())
            }
            SyntaxStatement::FunctionDeclaration { binding: Some(binding), .. } => {
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
            SyntaxStatement::FunctionDeclaration { binding: None, span, .. } => self.fail(
                "lowering.function_declaration_binding_missing",
                Some(*span),
                "function declaration has no resolved lexical binding",
            ),
            SyntaxStatement::Expression { expression, .. } => {
                self.emit_expression(expression)?;
                Ok(())
            }
            SyntaxStatement::If { test, consequent, alternate, span } => {
                self.compile_if(test, consequent, alternate.as_deref(), *span)
            }
            SyntaxStatement::While { test, body, span } => self.compile_while(test, body, *span),
            SyntaxStatement::For { initializer, test, update, body, span, .. } => {
                self.compile_for(initializer.as_ref(), test.as_ref(), update.as_ref(), body, *span)
            }
            SyntaxStatement::Return { value, span } => {
                let value = value.as_ref().map(|value| self.emit_expression(value)).transpose()?;
                self.terminate_return(value.as_ref().map(|value| value.id.clone()), self.user_source(*span, Vec::new()))
            }
            SyntaxStatement::Throw { value, span } => {
                let value = self.emit_expression(value)?;
                self.emit_instruction(
                    "js.throw",
                    vec![value.id],
                    None,
                    BTreeMap::new(),
                    Effects { may_throw: true, ..Effects::default() },
                    self.user_source(*span, Vec::new()),
                )?;
                let Some(block) = self.current else {
                    return self.fail("lowering.throw_block", Some(*span), "throw instruction has no current block");
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
            return self.fail("lowering.if_without_block", Some(span), "if statement has no active predecessor");
        };
        let then_block = self.create_block();
        let else_block = self.create_block();
        let arguments = predecessor.effect.clone().into_iter().collect();
        self.blocks[predecessor.block].terminator = Terminator::Branch {
            condition: condition.id,
            then_edge: Edge { target: self.block_id(then_block), arguments: arguments.clone() },
            else_edge: Edge { target: self.block_id(else_block), arguments },
            source: self.user_source(span, Vec::new()),
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
            return self.fail("lowering.while_without_block", Some(span), "while loop has no active predecessor");
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
            return self.fail("lowering.while_header", Some(span), "while header terminated during test evaluation");
        };
        let effect_args = header_cursor.effect.into_iter().collect();
        self.blocks[header_cursor.block].terminator = Terminator::Branch {
            condition: condition.id,
            then_edge: Edge { target: self.block_id(body_block), arguments: effect_args.clone() },
            else_edge: Edge { target: self.block_id(exit_block), arguments: effect_args },
            source: self.user_source(span, Vec::new()),
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
                SyntaxForInitializer::Variable { kind, declarations, span } => {
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
            return self.fail("lowering.for_without_block", Some(span), "for loop has no active predecessor");
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
            return self.fail("lowering.for_header", Some(span), "for header terminated during condition evaluation");
        };
        let effect_args = header_cursor.effect.into_iter().collect();
        self.blocks[header_cursor.block].terminator = Terminator::Branch {
            condition: condition.id,
            then_edge: Edge { target: self.block_id(body_block), arguments: effect_args.clone() },
            else_edge: Edge { target: self.block_id(exit_block), arguments: effect_args },
            source: self.user_source(span, Vec::new()),
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
            return self.fail("lowering.jump_without_block", Some(span), "jump has no active source block");
        };
        self.blocks[block].terminator = Terminator::Jump {
            target: target.to_owned(),
            arguments: self.current_effect.clone().into_iter().collect(),
            source: self.user_source(span, Vec::new()),
        };
        self.current = None;
        self.current_effect = None;
        Ok(())
    }

    fn terminate_return(&mut self, value: Option<String>, source: Source) -> Result<(), LoweringDiagnostic> {
        let Some(block) = self.current else {
            return self.fail("lowering.return_without_block", None, "return has no active source block");
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
                    ty: ValueType { semantic: parameter.semantic_type, representation: parameter.representation },
                },
                None,
                Some("initialize function parameter environment binding"),
            )?;
        }
        Ok(())
    }

    fn emit_var_initializers(&mut self) -> Result<(), LoweringDiagnostic> {
        let var_ids: Vec<_> = self.local_bindings.values()
            .filter(|binding| binding.kind == BindingKind::Var)
            .map(|binding| binding.id.clone())
            .collect();
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
        if returns.is_empty() {
            return Ok(None);
        }
        let first = returns[0];
        if returns.iter().any(|item| *item != first) {
            return self.fail(
                "lowering.mixed_return_types",
                Some(self.syntax.span),
                "function returns incompatible Core IR value types or mixes value and void returns",
            );
        }
        let Some(ty) = first else {
            return Ok(None);
        };
        if sequence_can_fallthrough(&self.syntax.body) {
            return self.fail(
                "lowering.implicit_return_type",
                Some(self.syntax.span),
                "function has a value-returning path and a reachable fallthrough path",
            );
        }
        Ok(Some(ReturnSignature { semantic_type: ty.semantic, representation: ty.representation }))
    }

    fn collect_return_types(
        &self,
        statements: &[SyntaxStatement],
        output: &mut Vec<Option<ValueType>>,
    ) -> Result<(), LoweringDiagnostic> {
        for statement in statements {
            match statement {
                SyntaxStatement::Return { value, .. } => {
                    output.push(value.as_ref().map(|value| self.infer_expression_type(value)).transpose()?);
                }
                SyntaxStatement::Block { body, .. } => self.collect_return_types(body, output)?,
                SyntaxStatement::If { consequent, alternate, .. } => {
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

    fn infer_expression_type(&self, expression: &SyntaxExpression) -> Result<ValueType, LoweringDiagnostic> {
        match expression {
            SyntaxExpression::Literal { value, .. } => Ok(literal_type(value)),
            SyntaxExpression::Identifier { name, binding, span } => {
                let Some(binding) = binding else {
                    if name == "undefined" {
                        return Ok(ValueType { semantic: SemanticType::Undefined, representation: Representation::Boxed });
                    }
                    return self.fail("lowering.unresolved_global", Some(*span), format!("unresolved global {name:?} has no Core IR contract"));
                };
                if let Some(local) = self.local_bindings.get(binding) {
                    if local.kind == BindingKind::Const {
                        let initializer = self.const_initializers.get(binding).ok_or_else(|| {
                            diag(self.syntax, "lowering.const_initializer_missing", Some(*span), "const initializer missing")
                        })?;
                        return self.infer_const_type(binding, initializer, &mut HashSet::new(), *span);
                    }
                    return Ok(ValueType::DYNAMIC);
                }
                if self.captures_by_binding.contains_key(binding) {
                    return Ok(ValueType::DYNAMIC);
                }
                self.fail("lowering.binding_not_in_environment", Some(*span), format!("unknown binding {binding:?}"))
            }
            SyntaxExpression::Parenthesized { expression, .. } => self.infer_expression_type(expression),
            SyntaxExpression::Binary { operator, .. } => match operator {
                SyntaxBinaryOperator::Add => Ok(ValueType::DYNAMIC),
                SyntaxBinaryOperator::StrictEqual | SyntaxBinaryOperator::StrictNotEqual | SyntaxBinaryOperator::LessThan => Ok(ValueType::BOOLEAN),
                SyntaxBinaryOperator::GreaterThan => self.fail("lowering.greater_than_not_in_core_ir", None, "greater-than requires its own Core IR operation"),
            },
            SyntaxExpression::Unary { operator, argument, span } => match operator {
                SyntaxUnaryOperator::LogicalNot => Ok(ValueType::BOOLEAN),
                SyntaxUnaryOperator::Typeof => Ok(ValueType::STRING),
                SyntaxUnaryOperator::Positive | SyntaxUnaryOperator::Negate => {
                    if matches!(argument.as_ref(), SyntaxExpression::Literal { value: SyntaxLiteral::Number(_), .. }) {
                        Ok(ValueType::NUMBER)
                    } else {
                        self.fail("lowering.dynamic_unary_numeric", Some(*span), "dynamic unary numeric operation is unsupported")
                    }
                }
            },
            SyntaxExpression::Call { .. } | SyntaxExpression::Member { .. } => Ok(ValueType::DYNAMIC),
            SyntaxExpression::Assign { value, .. } => self.infer_expression_type(value),
            SyntaxExpression::Update { span, .. } => self.fail("lowering.update_expression_unsupported", Some(*span), "update expression unsupported"),
            SyntaxExpression::Function { .. } => Ok(ValueType::FUNCTION),
            SyntaxExpression::Unsupported { syntax, span } => self.fail("lowering.unsupported_expression", Some(*span), format!("syntax {syntax:?} unsupported")),
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
            return self.fail("lowering.const_initializer_cycle", Some(span), format!("cyclic const initializer for {binding:?}"));
        }
        let result = match expression {
            SyntaxExpression::Identifier { binding: Some(other), .. }
                if self.local_bindings.get(other).is_some_and(|item| item.kind == BindingKind::Const) =>
            {
                let initializer = self.const_initializers.get(other).ok_or_else(|| {
                    diag(self.syntax, "lowering.const_initializer_missing", Some(span), "const initializer missing")
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
        SyntaxLiteral::Null => ValueType { semantic: SemanticType::Null, representation: Representation::Boxed },
        SyntaxLiteral::Boolean(_) => ValueType::BOOLEAN,
        SyntaxLiteral::Number(_) => ValueType::NUMBER,
        SyntaxLiteral::String(_) => ValueType::STRING,
        SyntaxLiteral::Bigint(_) => ValueType { semantic: SemanticType::Bigint, representation: Representation::BigintRef },
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
                SyntaxStatement::Variable { kind: SyntaxVariableKind::Const, declarations, .. } => {
                    for declaration in declarations {
                        if let (Some(binding), Some(initializer)) = (&declaration.binding, &declaration.initializer) {
                            output.insert(binding.clone(), initializer.clone());
                        }
                    }
                }
                SyntaxStatement::Block { body, .. } => visit(body, output),
                SyntaxStatement::If { consequent, alternate, .. } => {
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

fn statements_need_effects(statements: &[SyntaxStatement]) -> bool {
    statements.iter().any(statement_needs_effects)
}

fn statement_needs_effects(statement: &SyntaxStatement) -> bool {
    match statement {
        SyntaxStatement::Block { body, .. } => statements_need_effects(body),
        SyntaxStatement::Variable { kind, declarations, .. } => {
            *kind != SyntaxVariableKind::Const || declarations.iter().any(|item| item.initializer.as_ref().is_some_and(expression_needs_effects))
        }
        SyntaxStatement::FunctionDeclaration { .. } => true,
        SyntaxStatement::Expression { expression, .. } => expression_needs_effects(expression),
        SyntaxStatement::If { test, consequent, alternate, .. } => {
            expression_needs_effects(test)
                || statement_needs_effects(consequent)
                || alternate.as_ref().is_some_and(|item| statement_needs_effects(item))
        }
        SyntaxStatement::While { test, body, .. } => expression_needs_effects(test) || statement_needs_effects(body),
        SyntaxStatement::For { initializer, test, update, body, .. } => {
            initializer.as_ref().is_some_and(|initializer| match initializer {
                SyntaxForInitializer::Variable { kind, declarations, .. } => {
                    *kind != SyntaxVariableKind::Const || declarations.iter().any(|item| item.initializer.as_ref().is_some_and(expression_needs_effects))
                }
                SyntaxForInitializer::Expression(expression) => expression_needs_effects(expression),
            }) || test.as_ref().is_some_and(expression_needs_effects)
                || update.as_ref().is_some_and(expression_needs_effects)
                || statement_needs_effects(body)
        }
        SyntaxStatement::Return { value, .. } => value.as_ref().is_some_and(expression_needs_effects),
        SyntaxStatement::Throw { .. } => true,
        SyntaxStatement::Empty { .. } | SyntaxStatement::Unsupported { .. } => false,
    }
}

fn expression_needs_effects(expression: &SyntaxExpression) -> bool {
    match expression {
        SyntaxExpression::Literal { .. } => false,
        SyntaxExpression::Identifier { binding: None, name, .. } => name != "undefined",
        SyntaxExpression::Identifier { binding: Some(_), .. } => true,
        SyntaxExpression::Binary { operator, left, right, .. } => {
            !matches!(operator, SyntaxBinaryOperator::StrictEqual | SyntaxBinaryOperator::StrictNotEqual)
                || expression_needs_effects(left)
                || expression_needs_effects(right)
        }
        SyntaxExpression::Unary { operator, argument, .. } => {
            matches!(operator, SyntaxUnaryOperator::Positive | SyntaxUnaryOperator::Negate)
                && expression_needs_effects(argument)
        }
        SyntaxExpression::Call { .. }
        | SyntaxExpression::Member { .. }
        | SyntaxExpression::Assign { .. }
        | SyntaxExpression::Update { .. }
        | SyntaxExpression::Function { .. } => true,
        SyntaxExpression::Parenthesized { expression, .. } => expression_needs_effects(expression),
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
        SyntaxStatement::If { consequent, alternate, .. } => {
            statement_can_fallthrough(consequent)
                || alternate.as_ref().is_none_or(|item| statement_can_fallthrough(item))
        }
        SyntaxStatement::While { .. } | SyntaxStatement::For { .. } => true,
        _ => true,
    }
}
