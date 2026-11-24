//! Owned Rust representation and validation for canonical function-level Core IR v1.
#![deny(missing_docs)]

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

/// Identifier of the canonical Core IR v1 contract.
pub const CONTRACT_ID: &str = "ogma.core-ir/1.0.0";
/// Contract name in serialized documents.
pub const CONTRACT_NAME: &str = "ogma.core-ir";
/// Contract version in serialized documents.
pub const CONTRACT_VERSION: &str = "1.0.0";

fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// Owned serialized Core IR function document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FunctionDocument {
    /// Contract name.
    pub contract: String,
    /// Contract version.
    pub version: String,
    /// Function payload.
    pub function: Function,
}

/// JavaScript semantic value type.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticType {
    /// Unknown JavaScript value.
    Dynamic,
    /// Undefined.
    Undefined,
    /// Null.
    Null,
    /// Boolean.
    Boolean,
    /// Number.
    Number,
    /// String.
    String,
    /// Symbol.
    Symbol,
    /// BigInt.
    Bigint,
    /// Ordinary object.
    Object,
    /// Array.
    Array,
    /// Function.
    Function,
    /// Exception.
    Exception,
}

/// Abstract compiler value representation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Representation {
    /// Tagged JavaScript value.
    Boxed,
    /// Unboxed boolean.
    I1,
    /// Signed 32-bit integer.
    I32,
    /// Unsigned 32-bit integer.
    U32,
    /// IEEE-754 binary64.
    F64,
    /// Opaque string reference.
    StringRef,
    /// Opaque symbol reference.
    SymbolRef,
    /// Opaque BigInt reference.
    BigintRef,
    /// Opaque object reference.
    ObjectRef,
    /// Opaque array reference.
    ArrayRef,
    /// Opaque function reference.
    FunctionRef,
    /// Opaque exception reference.
    ExceptionRef,
}

/// SSA value definition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValueDefinition {
    /// Stable SSA identity.
    pub id: String,
    /// Must be value.
    pub kind: String,
    /// JavaScript semantic type.
    pub semantic_type: SemanticType,
    /// Abstract representation.
    pub representation: Representation,
}

/// Effect token definition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectDefinition {
    /// Stable effect identity.
    pub id: String,
    /// Must be effect.
    pub kind: String,
}

/// SSA value or effect parameter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BlockParameter {
    /// Value parameter.
    Value(ValueDefinition),
    /// Effect parameter.
    Effect(EffectDefinition),
}

/// Declared function return type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReturnSignature {
    /// JavaScript semantic type.
    pub semantic_type: SemanticType,
    /// Abstract representation.
    pub representation: Representation,
}

/// One-based source line and zero-based UTF-16 column.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePosition {
    /// One-based line.
    pub line: u32,
    /// Zero-based UTF-16 code-unit column.
    pub column: u32,
}

/// Inlining provenance frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceFrame {
    /// Inlined function identity.
    pub function: String,
    /// Source-file identity.
    pub file: String,
    /// Inclusive start position.
    pub start: SourcePosition,
    /// Exclusive end position.
    pub end: SourcePosition,
}

/// Source provenance for an instruction or terminator.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "origin", deny_unknown_fields)]
pub enum Source {
    /// User-authored source range.
    #[serde(rename = "user")]
    User {
        /// Source-file identity.
        file: String,
        /// Inclusive start.
        start: SourcePosition,
        /// Exclusive end.
        end: SourcePosition,
        /// Lexical environment identity, if known.
        #[serde(deserialize_with = "required_option")]
        scope: Option<String>,
        /// Relevant lexical binding identities.
        bindings: Vec<String>,
        /// Suspension identity, when applicable.
        #[serde(deserialize_with = "required_option")]
        suspension: Option<String>,
        /// Inlining provenance.
        inline_chain: Vec<SourceFrame>,
    },
    /// Compiler-generated operation without a fabricated source range.
    #[serde(rename = "generated")]
    Generated {
        /// Non-empty generation reason.
        reason: String,
        /// Lexical environment identity, if known.
        #[serde(deserialize_with = "required_option")]
        scope: Option<String>,
        /// Relevant lexical binding identities.
        bindings: Vec<String>,
        /// Suspension identity, when applicable.
        #[serde(deserialize_with = "required_option")]
        suspension: Option<String>,
        /// Inlining provenance.
        inline_chain: Vec<SourceFrame>,
    },
}

/// Source language.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceLanguage {
    /// JavaScript.
    Javascript,
    /// TypeScript.
    Typescript,
    /// JSON.
    Json,
    /// Synthetic compiler source.
    Synthetic,
}

/// Source-file identity and normalized path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceFile {
    /// Stable source-file identity.
    pub id: String,
    /// Repository-relative POSIX path.
    pub path: String,
    /// Source language.
    pub language: SourceLanguage,
    /// Optional SHA-256 digest of exact source bytes.
    pub sha256: Option<String>,
}

/// JavaScript binding declaration kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingKind {
    /// Function-scoped var.
    Var,
    /// Block-scoped mutable let.
    Let,
    /// Immutable const.
    Const,
    /// Mutable function parameter.
    Parameter,
    /// Mutable catch binding.
    Catch,
    /// Immutable import binding.
    Import,
}

/// Lexical binding descriptor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    /// Stable binding identity.
    pub id: String,
    /// Source name.
    pub name: String,
    /// Declaration kind.
    pub kind: BindingKind,
    /// Whether assignment is permitted.
    pub mutable: bool,
    /// Semantic value type.
    pub semantic_type: SemanticType,
    /// Abstract representation.
    pub representation: Representation,
}

/// Closure capture mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    /// Shared mutable lexical cell.
    Cell,
    /// Copied immutable value.
    Value,
}

/// Captured outer lexical binding.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    /// Stable capture identity in this function.
    pub id: String,
    /// Owning outer environment.
    pub source_environment: String,
    /// Original binding identity.
    pub binding: String,
    /// Source name.
    pub name: String,
    /// Whether the original binding is mutable.
    pub mutable: bool,
    /// Semantic value type.
    pub semantic_type: SemanticType,
    /// Abstract representation.
    pub representation: Representation,
    /// Cell or value capture.
    pub mode: CaptureMode,
}

/// Function lexical environment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    /// Stable environment identity.
    pub id: String,
    /// Parent environment, or null.
    #[serde(deserialize_with = "required_option")]
    pub parent: Option<String>,
    /// Bindings owned or visible in this environment.
    pub bindings: Vec<Binding>,
}

/// Function parameter and its lexical binding.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FunctionParameter {
    /// SSA parameter identity.
    pub id: String,
    /// Must be value.
    pub kind: String,
    /// Semantic value type.
    pub semantic_type: SemanticType,
    /// Abstract representation.
    pub representation: Representation,
    /// Source parameter name.
    pub name: String,
    /// Stable lexical binding identity.
    pub binding: String,
}

/// Conservative effect summary.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Effects {
    /// May allocate.
    pub may_allocate: bool,
    /// May reach GC.
    pub may_gc: bool,
    /// May throw.
    pub may_throw: bool,
    /// May suspend.
    pub may_suspend: bool,
    /// May call host capability.
    pub may_call_host: bool,
    /// May re-enter Ogma.
    pub may_reenter: bool,
    /// May mutate object shape.
    pub may_mutate_shape: bool,
}

impl Effects {
    /// Whether the instruction participates in effect-token SSA.
    pub fn is_effectful(self) -> bool {
        self.may_allocate
            || self.may_gc
            || self.may_throw
            || self.may_suspend
            || self.may_call_host
            || self.may_reenter
            || self.may_mutate_shape
    }

    fn covers(self, required: Self) -> bool {
        (!required.may_allocate || self.may_allocate)
            && (!required.may_gc || self.may_gc)
            && (!required.may_throw || self.may_throw)
            && (!required.may_suspend || self.may_suspend)
            && (!required.may_call_host || self.may_call_host)
            && (!required.may_reenter || self.may_reenter)
            && (!required.may_mutate_shape || self.may_mutate_shape)
    }
}

/// Exceptional continuation of a potentially throwing instruction.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum ExceptionFlow {
    /// Propagate to the caller.
    #[serde(rename = "propagate")]
    Propagate,
    /// Transfer control to a local handler.
    #[serde(rename = "edge")]
    Edge {
        /// Handler block.
        target: String,
        /// Values passed to the handler's ordinary block parameters.
        arguments: Vec<String>,
    },
}

/// Semantic IR operation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instruction {
    /// Stable instruction identity.
    pub id: String,
    /// Operation name from Core IR v1.
    pub op: String,
    /// SSA operands in evaluation order.
    pub operands: Vec<String>,
    /// Result definition, or null.
    #[serde(deserialize_with = "required_option")]
    pub result: Option<ValueDefinition>,
    /// Operation-specific attributes.
    pub attributes: BTreeMap<String, serde_json::Value>,
    /// Conservative effect summary.
    pub effects: Effects,
    /// Source provenance.
    pub source: Source,
    /// Effect-token input, or null.
    #[serde(deserialize_with = "required_option")]
    pub effect_in: Option<String>,
    /// Effect-token output, or null.
    #[serde(deserialize_with = "required_option")]
    pub effect_out: Option<String>,
    /// Exceptional continuation, or null.
    #[serde(deserialize_with = "required_option")]
    pub exception: Option<ExceptionFlow>,
}

/// Normal control-flow edge.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    /// Destination block.
    pub target: String,
    /// Values passed to destination block parameters.
    pub arguments: Vec<String>,
}

/// Basic-block terminator.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Terminator {
    /// Unconditional jump.
    #[serde(rename = "jump")]
    Jump {
        /// Destination block.
        target: String,
        /// Destination block arguments.
        arguments: Vec<String>,
        /// Source provenance.
        source: Source,
    },
    /// Conditional branch.
    #[serde(rename = "branch")]
    Branch {
        /// Boolean condition.
        condition: String,
        /// True edge.
        #[serde(rename = "then")]
        then_edge: Edge,
        /// False edge.
        #[serde(rename = "else")]
        else_edge: Edge,
        /// Source provenance.
        source: Source,
    },
    /// Return from function.
    #[serde(rename = "return")]
    Return {
        /// Returned value, or null for void.
        value: Option<String>,
        /// Source provenance.
        source: Source,
    },
    /// Block with no normal continuation.
    #[serde(rename = "unreachable")]
    Unreachable {
        /// Provenance of unreachability.
        reason: UnreachableReason,
        /// Source provenance.
        source: Source,
    },
}

/// Why a block has no normal continuation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnreachableReason {
    /// Immediately follows a throw instruction.
    AfterThrow,
    /// Proven unreachable by the compiler.
    ProvenUnreachable,
}

/// Basic block.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BasicBlock {
    /// Stable block identity.
    pub id: String,
    /// SSA value/effect parameters.
    pub parameters: Vec<BlockParameter>,
    /// Implicit exception parameter for handler entry, or null.
    #[serde(deserialize_with = "required_option")]
    pub exception_parameter: Option<ValueDefinition>,
    /// Ordered instructions.
    pub instructions: Vec<Instruction>,
    /// Block terminator.
    pub terminator: Terminator,
}

/// Function-level Core IR.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Function {
    /// Stable function identity.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Function parameters.
    pub parameters: Vec<FunctionParameter>,
    /// Optional return signature.
    #[serde(rename = "return", deserialize_with = "required_option")]
    pub return_signature: Option<ReturnSignature>,
    /// Source files referenced by this function.
    pub source_files: Vec<SourceFile>,
    /// Lexical environment.
    pub environment: Environment,
    /// Captured outer bindings.
    pub captures: Vec<Capture>,
    /// Entry block identity.
    pub entry_block: String,
    /// Entry effect token, or null for pure functions.
    #[serde(deserialize_with = "required_option")]
    pub effect_entry: Option<String>,
    /// Function blocks.
    pub blocks: Vec<BasicBlock>,
}

/// Structured verifier diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    /// Stable diagnostic code.
    pub code: &'static str,
    /// JSONPath-like location.
    pub path: String,
    /// Human-readable explanation.
    pub message: String,
}

impl Diagnostic {
    fn new(code: &'static str, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            path: path.into(),
            message: message.into(),
        }
    }
}

/// Parse and verify a canonical Core IR JSON document.
pub fn parse_and_verify(input: &str) -> Result<FunctionDocument, Vec<Diagnostic>> {
    let document = serde_json::from_str::<FunctionDocument>(input)
        .map_err(|error| vec![Diagnostic::new("schema.decode", "$", error.to_string())])?;
    let diagnostics = verify_document(&document);
    if diagnostics.is_empty() {
        Ok(document)
    } else {
        Err(diagnostics)
    }
}

fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    id.len() <= 128
        && (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | ':' | '-'))
}

fn valid_representation(ty: SemanticType, rep: Representation) -> bool {
    use Representation as R;
    use SemanticType as T;
    match ty {
        T::Dynamic | T::Undefined | T::Null => rep == R::Boxed,
        T::Boolean => matches!(rep, R::Boxed | R::I1),
        T::Number => matches!(rep, R::Boxed | R::I32 | R::U32 | R::F64),
        T::String => matches!(rep, R::Boxed | R::StringRef),
        T::Symbol => matches!(rep, R::Boxed | R::SymbolRef),
        T::Bigint => matches!(rep, R::Boxed | R::BigintRef),
        T::Object => matches!(rep, R::Boxed | R::ObjectRef),
        T::Array => matches!(rep, R::Boxed | R::ArrayRef | R::ObjectRef),
        T::Function => matches!(rep, R::Boxed | R::FunctionRef | R::ObjectRef),
        T::Exception => matches!(rep, R::Boxed | R::ExceptionRef),
    }
}

fn operation_effects(op: &str) -> Option<Effects> {
    use Effects as E;
    let dynamic = E {
        may_allocate: true,
        may_gc: true,
        may_throw: true,
        may_suspend: false,
        may_call_host: true,
        may_reenter: true,
        may_mutate_shape: true,
    };
    match op {
        "js.constant" | "js.to_boolean" | "js.strict_equal" | "js.typeof" => Some(E::default()),
        "js.add" | "js.get_property" | "js.set_property" | "js.get_element" | "js.set_element"
        | "js.call" | "js.construct" | "js.to_number" | "js.to_string" => Some(dynamic),
        "env.load" | "env.store" => Some(E {
            may_throw: true,
            ..E::default()
        }),
        "closure.create" => Some(E {
            may_allocate: true,
            may_gc: true,
            ..E::default()
        }),
        "host.call" => Some(E {
            may_allocate: true,
            may_gc: true,
            may_throw: true,
            may_call_host: true,
            may_reenter: true,
            may_mutate_shape: true,
            may_suspend: false,
        }),
        "async.await" => Some(E {
            may_allocate: true,
            may_gc: true,
            may_throw: true,
            may_suspend: true,
            ..E::default()
        }),
        "js.throw" => Some(E {
            may_throw: true,
            ..E::default()
        }),
        _ => None,
    }
}

fn operation_arity(op: &str) -> Option<(usize, Option<usize>)> {
    match op {
        "js.constant" | "env.load" | "closure.create" => Some((0, Some(0))),
        "js.add" | "js.strict_equal" => Some((2, Some(2))),
        "js.get_property" | "js.to_boolean" | "js.to_number" | "js.to_string" | "js.typeof"
        | "async.await" | "js.throw" => Some((1, Some(1))),
        "js.set_property" | "js.get_element" => Some((2, Some(2))),
        "js.set_element" => Some((3, Some(3))),
        "js.call" | "js.construct" => Some((2, None)),
        "host.call" => Some((0, None)),
        "js.throw" => Some((1, Some(1))),
        "env.store" => Some((1, Some(1))),
        _ => None,
    }
}

#[derive(Clone, Debug)]
struct Definition {
    block: Option<String>,
    index: isize,
    effect: bool,
    ty: Option<SemanticType>,
    rep: Option<Representation>,
}

fn define(
    definitions: &mut HashMap<String, Definition>,
    errors: &mut Vec<Diagnostic>,
    id: &str,
    def: Definition,
    path: String,
) {
    if !valid_id(id) {
        errors.push(Diagnostic::new(
            "id.invalid",
            path.clone(),
            format!("invalid identity {id:?}"),
        ));
    }
    if definitions.insert(id.to_owned(), def).is_some() {
        errors.push(Diagnostic::new(
            "ssa.duplicate_definition",
            path,
            format!("duplicate SSA identity {id:?}"),
        ));
    }
}

fn check_dominance(
    value: &str,
    block: &str,
    index: isize,
    definitions: &HashMap<String, Definition>,
    dominators: &HashMap<String, HashSet<String>>,
    path: String,
    errors: &mut Vec<Diagnostic>,
) {
    let Some(def) = definitions.get(value) else {
        errors.push(Diagnostic::new(
            "ssa.undefined_use",
            path,
            format!("undefined SSA value {value:?}"),
        ));
        return;
    };
    let Some(def_block) = &def.block else { return };
    if def_block == block {
        if def.index >= index {
            errors.push(Diagnostic::new(
                "ssa.use_before_definition",
                path,
                format!("{value:?} is not defined before this use"),
            ));
        }
    } else if !dominators
        .get(block)
        .is_some_and(|set| set.contains(def_block))
    {
        errors.push(Diagnostic::new(
            "ssa.dominance",
            path,
            format!("{value:?} does not dominate this use"),
        ));
    }
}

fn verify_edge(
    edge: &Edge,
    source_block: &str,
    source_index: isize,
    blocks: &HashMap<String, &BasicBlock>,
    definitions: &HashMap<String, Definition>,
    dominators: &HashMap<String, HashSet<String>>,
    path: String,
    errors: &mut Vec<Diagnostic>,
) {
    let Some(target) = blocks.get(&edge.target) else {
        errors.push(Diagnostic::new(
            "cfg.edge_target",
            format!("{path}.target"),
            format!("unknown target block {:?}", edge.target),
        ));
        return;
    };
    if edge.arguments.len() != target.parameters.len() {
        errors.push(Diagnostic::new(
            "cfg.edge_arity",
            format!("{path}.arguments"),
            "edge arguments do not match target parameter count",
        ));
        return;
    }
    for (index, (argument, parameter)) in edge.arguments.iter().zip(&target.parameters).enumerate()
    {
        let argument_path = format!("{path}.arguments[{index}]");
        if let Some(definition) = definitions.get(argument) {
            match parameter {
                BlockParameter::Effect(_) if !definition.effect => errors.push(Diagnostic::new(
                    "cfg.edge_argument_type",
                    argument_path.clone(),
                    "effect parameter requires an effect token",
                )),
                BlockParameter::Value(value)
                    if definition.effect
                        || (definition.ty, definition.rep)
                            != (Some(value.semantic_type), Some(value.representation)) =>
                {
                    errors.push(Diagnostic::new(
                        "cfg.edge_argument_type",
                        argument_path.clone(),
                        "value argument type/representation differs from target parameter",
                    ))
                }
                _ => {}
            }
        }
        check_dominance(
            argument,
            source_block,
            source_index,
            definitions,
            dominators,
            argument_path,
            errors,
        );
    }
}

fn source_bindings(
    source: &Source,
) -> (&Option<String>, &[String], &Option<String>, &[SourceFrame]) {
    match source {
        Source::User {
            scope,
            bindings,
            suspension,
            inline_chain,
            ..
        }
        | Source::Generated {
            scope,
            bindings,
            suspension,
            inline_chain,
            ..
        } => (scope, bindings, suspension, inline_chain),
    }
}

fn verify_source(
    source: &Source,
    files: &HashMap<String, &SourceFile>,
    allowed_bindings: &HashSet<String>,
    environment_id: &str,
    path: String,
    errors: &mut Vec<Diagnostic>,
) {
    match source {
        Source::User {
            file, start, end, ..
        } => {
            if !files.contains_key(file) {
                errors.push(Diagnostic::new(
                    "source.unknown_file",
                    format!("{path}.file"),
                    format!("unknown source file {file:?}"),
                ));
            }
            if start.line == 0 || end.line == 0 || start > end {
                errors.push(Diagnostic::new(
                    "source.invalid_span",
                    path.clone(),
                    "source range must use one-based lines and end no earlier than start",
                ));
            }
        }
        Source::Generated { reason, .. } if reason.trim().is_empty() => {
            errors.push(Diagnostic::new(
                "source.generated_reason",
                format!("{path}.reason"),
                "generated provenance requires a reason",
            ))
        }
        _ => {}
    }
    let (scope, bindings, suspension, inline_chain) = source_bindings(source);
    if scope
        .as_deref()
        .is_some_and(|scope| scope != environment_id)
    {
        errors.push(Diagnostic::new(
            "source.unknown_scope",
            format!("{path}.scope"),
            "scope must reference this function environment or null",
        ));
    }
    for (index, binding) in bindings.iter().enumerate() {
        if !allowed_bindings.contains(binding) {
            errors.push(Diagnostic::new(
                "source.unknown_binding",
                format!("{path}.bindings[{index}]"),
                format!("unknown binding {binding:?}"),
            ));
        }
    }
    if let Some(suspension) = suspension {
        if !valid_id(suspension) {
            errors.push(Diagnostic::new(
                "source.suspension_id",
                format!("{path}.suspension"),
                "invalid suspension identity",
            ));
        }
    }
    for (index, frame) in inline_chain.iter().enumerate() {
        if !valid_id(&frame.function)
            || !files.contains_key(&frame.file)
            || frame.start.line == 0
            || frame.end.line == 0
            || frame.start > frame.end
        {
            errors.push(Diagnostic::new(
                "source.inline_frame",
                format!("{path}.inline_chain[{index}]"),
                "invalid inlining provenance frame",
            ));
        }
    }
}

/// Validate the typed function model and its cross-object semantic invariants.
pub fn verify_document(document: &FunctionDocument) -> Vec<Diagnostic> {
    let mut errors = Vec::new();
    if document.contract != CONTRACT_NAME || document.version != CONTRACT_VERSION {
        errors.push(Diagnostic::new(
            "contract.version",
            "$.version",
            "expected ogma.core-ir version 1.0.0",
        ));
        return errors;
    }
    let function = &document.function;
    let root = "$.function";
    if !valid_id(&function.id) {
        errors.push(Diagnostic::new(
            "id.invalid",
            format!("{root}.id"),
            "invalid function identity",
        ));
    }
    if !valid_id(&function.environment.id) {
        errors.push(Diagnostic::new(
            "environment.id",
            format!("{root}.environment.id"),
            "invalid environment identity",
        ));
    }
    if function
        .environment
        .parent
        .as_deref()
        .is_some_and(|id| !valid_id(id))
    {
        errors.push(Diagnostic::new(
            "environment.parent_id",
            format!("{root}.environment.parent"),
            "invalid parent environment identity",
        ));
    }
    if function.name.trim().is_empty() {
        errors.push(Diagnostic::new(
            "function.name",
            format!("{root}.name"),
            "function name cannot be empty",
        ));
    }
    if function.environment.id == function.environment.parent.as_deref().unwrap_or_default() {
        errors.push(Diagnostic::new(
            "environment.parent_cycle",
            format!("{root}.environment.parent"),
            "environment cannot be its own parent",
        ));
    }

    let mut files: HashMap<String, &SourceFile> = HashMap::new();
    for (index, file) in function.source_files.iter().enumerate() {
        let path = format!("{root}.source_files[{index}]");
        if !valid_id(&file.id) || files.insert(file.id.clone(), file).is_some() {
            errors.push(Diagnostic::new(
                "source.duplicate_file",
                format!("{path}.id"),
                "invalid or duplicate source identity",
            ));
        }
        let segments: Vec<_> = file.path.split('/').collect();
        if file.path.is_empty()
            || file.path.starts_with('/')
            || file.path.contains('\\')
            || segments
                .iter()
                .any(|part| part.is_empty() || *part == "." || *part == "..")
        {
            errors.push(Diagnostic::new(
                "source.path",
                format!("{path}.path"),
                "source path must be normalized repository-relative POSIX",
            ));
        }
        if let Some(digest) = &file.sha256 {
            if digest.len() != 64
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                errors.push(Diagnostic::new(
                    "source.sha256",
                    format!("{path}.sha256"),
                    "SHA-256 must be 64 lowercase hexadecimal characters",
                ));
            }
        }
    }

    let mut bindings: HashMap<String, &Binding> = HashMap::new();
    for (index, binding) in function.environment.bindings.iter().enumerate() {
        let path = format!("{root}.environment.bindings[{index}]");
        if !valid_id(&binding.id) || bindings.insert(binding.id.clone(), binding).is_some() {
            errors.push(Diagnostic::new(
                "environment.binding_id",
                format!("{path}.id"),
                "invalid or duplicate binding identity",
            ));
        }
        let should_be_mutable = matches!(
            binding.kind,
            BindingKind::Var | BindingKind::Let | BindingKind::Parameter | BindingKind::Catch
        );
        if binding.mutable != should_be_mutable {
            errors.push(Diagnostic::new(
                "environment.binding_mutability",
                format!("{path}.mutable"),
                "binding mutability does not match declaration kind",
            ));
        }
        if !valid_representation(binding.semantic_type, binding.representation) {
            errors.push(Diagnostic::new(
                "representation.unsupported",
                format!("{path}.representation"),
                "invalid representation for semantic type",
            ));
        }
    }

    let mut captures: HashMap<String, &Capture> = HashMap::new();
    let mut capture_ids = HashSet::new();
    let mut captured_bindings = HashSet::new();
    for (index, capture) in function.captures.iter().enumerate() {
        let path = format!("{root}.captures[{index}]");
        if !valid_id(&capture.id) || !valid_id(&capture.binding) {
            errors.push(Diagnostic::new(
                "id.invalid",
                path.clone(),
                "invalid capture identity",
            ));
        }
        if !capture_ids.insert(capture.id.clone())
            || !captured_bindings.insert(capture.binding.clone())
        {
            errors.push(Diagnostic::new(
                "closure.duplicate_capture",
                path.clone(),
                "capture identity and imported binding must be unique",
            ));
        }
        if !valid_id(&capture.source_environment) {
            errors.push(Diagnostic::new(
                "id.invalid",
                format!("{path}.source_environment"),
                "invalid capture source environment identity",
            ));
        }
        if capture.source_environment == function.environment.id {
            errors.push(Diagnostic::new(
                "closure.capture_origin",
                format!("{path}.source_environment"),
                "captured binding must originate in another environment",
            ));
        }
        if capture.mutable && capture.mode != CaptureMode::Cell {
            errors.push(Diagnostic::new(
                "closure.capture_mode",
                format!("{path}.mode"),
                "mutable captures require cell mode",
            ));
        }
        if !capture.mutable && capture.mode != CaptureMode::Value {
            errors.push(Diagnostic::new(
                "closure.capture_mode",
                format!("{path}.mode"),
                "immutable captures require value mode",
            ));
        }
        if !valid_representation(capture.semantic_type, capture.representation) {
            errors.push(Diagnostic::new(
                "representation.unsupported",
                format!("{path}.representation"),
                "invalid capture representation",
            ));
        }
        for alias in [&capture.id, &capture.binding] {
            if bindings.contains_key(alias) {
                errors.push(Diagnostic::new(
                    "closure.capture_identifier_collision",
                    path.clone(),
                    "capture aliases collide with local binding identities",
                ));
            }
            if captures.insert(alias.clone(), capture).is_some() {
                errors.push(Diagnostic::new(
                    "closure.capture_identifier_collision",
                    path.clone(),
                    "capture aliases must be unambiguous",
                ));
            }
        }
    }

    let mut parameter_bindings = HashSet::new();
    for (index, parameter) in function.parameters.iter().enumerate() {
        let path = format!("{root}.parameters[{index}]");
        if parameter.kind != "value"
            || !valid_id(&parameter.id)
            || !parameter_bindings.insert(parameter.binding.clone())
        {
            errors.push(Diagnostic::new(
                "function.parameter",
                path.clone(),
                "invalid or duplicate value parameter",
            ));
        }
        if !valid_representation(parameter.semantic_type, parameter.representation) {
            errors.push(Diagnostic::new(
                "representation.unsupported",
                format!("{path}.representation"),
                "invalid parameter representation",
            ));
        }
        match bindings.get(&parameter.binding) {
            Some(binding)
                if binding.kind == BindingKind::Parameter
                    && binding.name == parameter.name
                    && binding.semantic_type == parameter.semantic_type
                    && binding.representation == parameter.representation => {}
            _ => errors.push(Diagnostic::new(
                "function.parameter_binding",
                format!("{path}.binding"),
                "parameter must match a parameter lexical binding",
            )),
        }
    }
    let declared_parameter_bindings: HashSet<String> = bindings
        .iter()
        .filter_map(|(id, binding)| (binding.kind == BindingKind::Parameter).then_some(id.clone()))
        .collect();
    if declared_parameter_bindings != parameter_bindings {
        errors.push(Diagnostic::new(
            "function.parameter_binding",
            format!("{root}.environment.bindings"),
            "parameter bindings and function parameter list must match exactly",
        ));
    }
    if let Some(signature) = &function.return_signature {
        if !valid_representation(signature.semantic_type, signature.representation) {
            errors.push(Diagnostic::new(
                "representation.unsupported",
                format!("{root}.return"),
                "invalid return representation",
            ));
        }
    }

    let allowed_bindings: HashSet<String> =
        bindings.keys().chain(captures.keys()).cloned().collect();
    let blocks: HashMap<String, &BasicBlock> = function
        .blocks
        .iter()
        .map(|block| (block.id.clone(), block))
        .collect();
    if blocks.len() != function.blocks.len() {
        errors.push(Diagnostic::new(
            "cfg.duplicate_block",
            format!("{root}.blocks"),
            "block identities must be unique",
        ));
    }
    if !valid_id(&function.entry_block) || !blocks.contains_key(&function.entry_block) {
        errors.push(Diagnostic::new(
            "cfg.entry_block",
            format!("{root}.entry_block"),
            "entry block identity is invalid or does not exist",
        ));
    }

    let mut definitions = HashMap::new();
    for (index, parameter) in function.parameters.iter().enumerate() {
        define(
            &mut definitions,
            &mut errors,
            &parameter.id,
            Definition {
                block: None,
                index: -1,
                effect: false,
                ty: Some(parameter.semantic_type),
                rep: Some(parameter.representation),
            },
            format!("{root}.parameters[{index}].id"),
        );
    }
    let mut instruction_ids = HashSet::new();
    let mut suspension_ids = HashSet::new();
    let mut successors: HashMap<String, Vec<String>> = HashMap::new();
    let mut exception_targets = HashSet::new();
    let mut any_effectful = false;

    for (block_index, block) in function.blocks.iter().enumerate() {
        let path = format!("{root}.blocks[{block_index}]");
        if !valid_id(&block.id) {
            errors.push(Diagnostic::new(
                "id.invalid",
                format!("{path}.id"),
                "invalid block identity",
            ));
        }
        let mut effect_count = 0usize;
        for (parameter_index, parameter) in block.parameters.iter().enumerate() {
            match parameter {
                BlockParameter::Value(value) => {
                    if value.kind != "value"
                        || !valid_representation(value.semantic_type, value.representation)
                    {
                        errors.push(Diagnostic::new(
                            "representation.unsupported",
                            format!("{path}.parameters[{parameter_index}]"),
                            "invalid block value parameter",
                        ));
                    }
                    define(
                        &mut definitions,
                        &mut errors,
                        &value.id,
                        Definition {
                            block: Some(block.id.clone()),
                            index: -1,
                            effect: false,
                            ty: Some(value.semantic_type),
                            rep: Some(value.representation),
                        },
                        format!("{path}.parameters[{parameter_index}].id"),
                    );
                }
                BlockParameter::Effect(effect) => {
                    effect_count += 1;
                    if effect.kind != "effect" {
                        errors.push(Diagnostic::new(
                            "effect.parameter",
                            format!("{path}.parameters[{parameter_index}]"),
                            "effect parameter kind must be effect",
                        ));
                    }
                    define(
                        &mut definitions,
                        &mut errors,
                        &effect.id,
                        Definition {
                            block: Some(block.id.clone()),
                            index: -1,
                            effect: true,
                            ty: None,
                            rep: None,
                        },
                        format!("{path}.parameters[{parameter_index}].id"),
                    );
                }
            }
        }
        if effect_count > 1
            || (effect_count == 1
                && !matches!(block.parameters.last(), Some(BlockParameter::Effect(_))))
        {
            errors.push(Diagnostic::new(
                "effect.block_parameter",
                format!("{path}.parameters"),
                "one effect parameter may appear only as the final block parameter",
            ));
        }
        if let Some(exception) = &block.exception_parameter {
            if exception.kind != "value"
                || exception.semantic_type != SemanticType::Exception
                || exception.representation != Representation::ExceptionRef
            {
                errors.push(Diagnostic::new(
                    "exception.parameter",
                    format!("{path}.exception_parameter"),
                    "exception parameter must be exception/exception_ref",
                ));
            }
            define(
                &mut definitions,
                &mut errors,
                &exception.id,
                Definition {
                    block: Some(block.id.clone()),
                    index: -1,
                    effect: false,
                    ty: Some(SemanticType::Exception),
                    rep: Some(Representation::ExceptionRef),
                },
                format!("{path}.exception_parameter.id"),
            );
        }

        for (instruction_index, instruction) in block.instructions.iter().enumerate() {
            let instruction_path = format!("{path}.instructions[{instruction_index}]");
            if !valid_id(&instruction.id) || !instruction_ids.insert(instruction.id.clone()) {
                errors.push(Diagnostic::new(
                    "instruction.identity",
                    format!("{instruction_path}.id"),
                    "invalid or duplicate instruction identity",
                ));
            }
            verify_source(
                &instruction.source,
                &files,
                &allowed_bindings,
                &function.environment.id,
                format!("{instruction_path}.source"),
                &mut errors,
            );
            if let Some(suspension) = source_bindings(&instruction.source).2 {
                if !suspension_ids.insert(suspension.clone()) {
                    errors.push(Diagnostic::new(
                        "source.suspension_duplicate",
                        format!("{instruction_path}.source.suspension"),
                        "suspension identity is duplicated within the function",
                    ));
                }
            }
            let Some(required_effects) = operation_effects(&instruction.op) else {
                errors.push(Diagnostic::new(
                    "operation.unsupported",
                    format!("{instruction_path}.op"),
                    format!("unsupported Core IR v1 operation {:?}", instruction.op),
                ));
                continue;
            };
            if !instruction.effects.covers(required_effects) {
                errors.push(Diagnostic::new(
                    "effect.underreported",
                    format!("{instruction_path}.effects"),
                    "operation omits required conservative effects",
                ));
            }
            any_effectful |= instruction.effects.is_effectful();
            if let Some((minimum, maximum)) = operation_arity(&instruction.op) {
                if instruction.operands.len() < minimum
                    || maximum.is_some_and(|max| instruction.operands.len() > max)
                {
                    errors.push(Diagnostic::new(
                        "operation.arity",
                        format!("{instruction_path}.operands"),
                        "invalid operand count",
                    ));
                }
            }
            let must_have_result = !matches!(instruction.op.as_str(), "js.throw");
            if must_have_result && instruction.result.is_none() {
                errors.push(Diagnostic::new(
                    "operation.result_missing",
                    format!("{instruction_path}.result"),
                    "operation must define its result",
                ));
            }
            if !must_have_result && instruction.result.is_some() {
                errors.push(Diagnostic::new(
                    "operation.result_unexpected",
                    format!("{instruction_path}.result"),
                    "js.throw cannot define a value",
                ));
            }
            if let Some(result) = &instruction.result {
                if result.kind != "value"
                    || !valid_representation(result.semantic_type, result.representation)
                {
                    errors.push(Diagnostic::new(
                        "representation.unsupported",
                        format!("{instruction_path}.result"),
                        "invalid result definition",
                    ));
                }
                let expected_type = match instruction.op.as_str() {
                    "js.add" | "js.get_property" | "js.get_element" | "js.call"
                    | "js.construct" | "host.call" | "async.await" => Some(SemanticType::Dynamic),
                    "js.to_boolean" | "js.strict_equal" => Some(SemanticType::Boolean),
                    "js.to_number" => Some(SemanticType::Number),
                    "js.to_string" | "js.typeof" => Some(SemanticType::String),
                    "closure.create" => Some(SemanticType::Function),
                    _ => None,
                };
                if expected_type.is_some_and(|ty| result.semantic_type != ty) {
                    errors.push(Diagnostic::new(
                        "operation.result_type",
                        format!("{instruction_path}.result.semantic_type"),
                        "result semantic type does not match operation",
                    ));
                }
                define(
                    &mut definitions,
                    &mut errors,
                    &result.id,
                    Definition {
                        block: Some(block.id.clone()),
                        index: instruction_index as isize,
                        effect: false,
                        ty: Some(result.semantic_type),
                        rep: Some(result.representation),
                    },
                    format!("{instruction_path}.result.id"),
                );
            }
            if let Some(effect_out) = &instruction.effect_out {
                define(
                    &mut definitions,
                    &mut errors,
                    effect_out,
                    Definition {
                        block: Some(block.id.clone()),
                        index: instruction_index as isize,
                        effect: true,
                        ty: None,
                        rep: None,
                    },
                    format!("{instruction_path}.effect_out"),
                );
            }
            let expected_attributes: &[&str] = match instruction.op.as_str() {
                "js.constant" => &["literal", "value"],
                "js.get_property" | "js.set_property" => &["key"],
                "env.load" | "env.store" => &["binding"],
                "closure.create" => &["function", "captures"],
                "host.call" => &["symbol"],
                _ => &[],
            };
            if instruction
                .attributes
                .keys()
                .any(|key| !expected_attributes.contains(&key.as_str()))
                || expected_attributes
                    .iter()
                    .any(|key| !instruction.attributes.contains_key(*key))
            {
                errors.push(Diagnostic::new(
                    "operation.attributes",
                    format!("{instruction_path}.attributes"),
                    "operation attributes do not match the v1 contract",
                ));
            }
            if instruction.op == "js.constant" {
                let literal = instruction
                    .attributes
                    .get("literal")
                    .and_then(serde_json::Value::as_str);
                let value = instruction.attributes.get("value");
                let valid = match literal {
                    Some("undefined" | "null") => value.is_some_and(serde_json::Value::is_null),
                    Some("boolean") => value.is_some_and(serde_json::Value::is_boolean),
                    Some("string") => value.is_some_and(serde_json::Value::is_string),
                    Some("bigint") => {
                        value
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|number| {
                                let digits = number.strip_prefix('-').unwrap_or(number);
                                digits == "0"
                                    || (!digits.is_empty()
                                        && !digits.starts_with('0')
                                        && digits.bytes().all(|byte| byte.is_ascii_digit()))
                            })
                    }
                    Some("number") => value.is_some_and(|number| {
                        if let Some(special) = number.as_str() {
                            matches!(special, "NaN" | "Infinity" | "-Infinity" | "-0")
                        } else if let Some(integer) = number.as_i64() {
                            (integer as f64) as i128 == integer as i128
                        } else if let Some(integer) = number.as_u64() {
                            (integer as f64) as u128 == integer as u128
                        } else {
                            number.as_f64().is_some_and(f64::is_finite)
                        }
                    }),
                    _ => false,
                };
                if !valid {
                    errors.push(Diagnostic::new(
                        "operation.literal_value",
                        format!("{instruction_path}.attributes.value"),
                        "invalid JavaScript literal payload",
                    ));
                }
                if let (Some(literal), Some(result)) = (literal, instruction.result.as_ref()) {
                    let expected = match literal {
                        "undefined" => Some(SemanticType::Undefined),
                        "null" => Some(SemanticType::Null),
                        "boolean" => Some(SemanticType::Boolean),
                        "number" => Some(SemanticType::Number),
                        "bigint" => Some(SemanticType::Bigint),
                        "string" => Some(SemanticType::String),
                        _ => None,
                    };
                    if expected != Some(result.semantic_type) {
                        errors.push(Diagnostic::new(
                            "operation.literal_type",
                            format!("{instruction_path}.result.semantic_type"),
                            "literal tag and result semantic type differ",
                        ));
                    }
                }
            }
            if matches!(instruction.op.as_str(), "env.load" | "env.store") {
                let binding_id = instruction
                    .attributes
                    .get("binding")
                    .and_then(serde_json::Value::as_str);
                let local = binding_id.and_then(|id| bindings.get(id).copied());
                let capture = binding_id.and_then(|id| captures.get(id).copied());
                if local.is_none() && capture.is_none() {
                    errors.push(Diagnostic::new(
                        "environment.binding_id",
                        format!("{instruction_path}.attributes.binding"),
                        "unknown local or captured binding",
                    ));
                }
                if instruction.op == "env.store" {
                    if local.is_some_and(|binding| !binding.mutable)
                        || capture.is_some_and(|capture| capture.mode == CaptureMode::Value)
                    {
                        errors.push(Diagnostic::new(
                            "environment.binding_mutability",
                            format!("{instruction_path}.attributes.binding"),
                            "cannot store to immutable or by-value captured binding",
                        ));
                    }
                    if let (Some(operand), Some(result)) =
                        (instruction.operands.first(), instruction.result.as_ref())
                    {
                        if let Some(definition) = definitions.get(operand) {
                            if (definition.ty, definition.rep)
                                != (Some(result.semantic_type), Some(result.representation))
                            {
                                errors.push(Diagnostic::new(
                                    "operation.result_type",
                                    format!("{instruction_path}.result"),
                                    "env.store result must preserve its operand type",
                                ));
                            }
                        }
                    }
                } else if let Some(result) = instruction.result.as_ref() {
                    let expected = local
                        .map(|binding| (binding.semantic_type, binding.representation))
                        .or_else(|| {
                            capture.map(|capture| (capture.semantic_type, capture.representation))
                        });
                    if expected.is_some_and(|expected| {
                        expected != (result.semantic_type, result.representation)
                    }) {
                        errors.push(Diagnostic::new(
                            "operation.result_type",
                            format!("{instruction_path}.result"),
                            "env.load result must match its binding",
                        ));
                    }
                }
            }
            if matches!(
                instruction.op.as_str(),
                "js.set_property" | "js.set_element"
            ) {
                let value_index = if instruction.op == "js.set_property" {
                    1
                } else {
                    2
                };
                if let (Some(operand), Some(result)) = (
                    instruction.operands.get(value_index),
                    instruction.result.as_ref(),
                ) {
                    if definitions.get(operand).is_some_and(|definition| {
                        (definition.ty, definition.rep)
                            != (Some(result.semantic_type), Some(result.representation))
                    }) {
                        errors.push(Diagnostic::new(
                            "operation.result_type",
                            format!("{instruction_path}.result"),
                            "property setter result must preserve the assigned value type",
                        ));
                    }
                }
            }
            if instruction.op == "closure.create" {
                if instruction
                    .attributes
                    .get("function")
                    .and_then(serde_json::Value::as_str)
                    .is_none_or(|id| !valid_id(id))
                {
                    errors.push(Diagnostic::new(
                        "closure.function_id",
                        format!("{instruction_path}.attributes.function"),
                        "invalid closure function identity",
                    ));
                }
                let descriptors = instruction
                    .attributes
                    .get("captures")
                    .and_then(serde_json::Value::as_array);
                if descriptors.is_none() {
                    errors.push(Diagnostic::new(
                        "closure.capture_list",
                        format!("{instruction_path}.attributes.captures"),
                        "closure capture descriptors must be an array",
                    ));
                } else {
                    let mut seen = HashSet::new();
                    for (capture_index, descriptor) in descriptors.unwrap().iter().enumerate() {
                        let binding = descriptor
                            .get("binding")
                            .and_then(serde_json::Value::as_str);
                        let mode = descriptor.get("mode").and_then(serde_json::Value::as_str);
                        if binding.is_none()
                            || !matches!(mode, Some("cell" | "value"))
                            || descriptor
                                .as_object()
                                .is_none_or(|object| object.len() != 2)
                        {
                            errors.push(Diagnostic::new(
                                "closure.capture_descriptor",
                                format!("{instruction_path}.attributes.captures[{capture_index}]"),
                                "capture descriptor must contain only binding and mode",
                            ));
                        } else if !seen.insert(binding.unwrap()) {
                            errors.push(Diagnostic::new(
                                "closure.duplicate_capture",
                                format!("{instruction_path}.attributes.captures[{capture_index}]"),
                                "duplicate captured binding",
                            ));
                        } else if let Some(capture) = captures.get(binding.unwrap()) {
                            if (mode == Some("cell") && !capture.mutable)
                                || (mode == Some("value") && capture.mutable)
                            {
                                errors.push(Diagnostic::new("closure.capture_mode", format!("{instruction_path}.attributes.captures[{capture_index}].mode"), "capture mode does not match binding mutability"));
                            }
                        } else if let Some(binding_info) = bindings.get(binding.unwrap()) {
                            if (mode == Some("cell") && !binding_info.mutable)
                                || (mode == Some("value") && binding_info.mutable)
                            {
                                errors.push(Diagnostic::new("closure.capture_mode", format!("{instruction_path}.attributes.captures[{capture_index}].mode"), "capture mode does not match binding mutability"));
                            }
                        } else {
                            errors.push(Diagnostic::new("closure.capture_binding", format!("{instruction_path}.attributes.captures[{capture_index}].binding"), "closure references an unknown local or captured binding"));
                        }
                    }
                }
            }
            if instruction.op == "host.call"
                && instruction
                    .attributes
                    .get("symbol")
                    .and_then(serde_json::Value::as_str)
                    .is_none_or(|id| !valid_id(id))
            {
                errors.push(Diagnostic::new(
                    "host.symbol_id",
                    format!("{instruction_path}.attributes.symbol"),
                    "invalid host capability symbol identity",
                ));
            }
            for (operand_index, operand) in instruction.operands.iter().enumerate() {
                if definitions
                    .get(operand)
                    .is_some_and(|definition| definition.effect)
                {
                    errors.push(Diagnostic::new(
                        "effect.as_value",
                        format!("{instruction_path}.operands[{operand_index}]"),
                        "effect tokens cannot be used as JavaScript operands",
                    ));
                }
            }
            match (instruction.effects.may_throw, &instruction.exception) {
                (true, None) => errors.push(Diagnostic::new(
                    "exception.metadata_missing",
                    format!("{instruction_path}.exception"),
                    "potentially throwing instruction requires exception flow",
                )),
                (false, Some(_)) => errors.push(Diagnostic::new(
                    "exception.unexpected",
                    format!("{instruction_path}.exception"),
                    "non-throwing instruction cannot have exception flow",
                )),
                _ => {}
            }
            if let Some(ExceptionFlow::Edge { target, .. }) = &instruction.exception {
                exception_targets.insert(target.clone());
                successors
                    .entry(block.id.clone())
                    .or_default()
                    .push(target.clone());
                if blocks
                    .get(target)
                    .is_none_or(|target| target.exception_parameter.is_none())
                {
                    errors.push(Diagnostic::new(
                        "exception.handler_shape",
                        format!("{instruction_path}.exception.target"),
                        "exception edge must target a block with exception_parameter",
                    ));
                }
            }
            if instruction.op == "js.throw" {
                if instruction_index + 1 != block.instructions.len()
                    || !matches!(
                        block.terminator,
                        Terminator::Unreachable {
                            reason: UnreachableReason::AfterThrow,
                            ..
                        }
                    )
                {
                    errors.push(Diagnostic::new(
                        "exception.throw_shape",
                        instruction_path.clone(),
                        "js.throw must be final and terminate with unreachable/after_throw",
                    ));
                }
            }
        }

        match &block.terminator {
            Terminator::Jump { target, source, .. } => {
                successors
                    .entry(block.id.clone())
                    .or_default()
                    .push(target.clone());
                verify_source(
                    source,
                    &files,
                    &allowed_bindings,
                    &function.environment.id,
                    format!("{path}.terminator.source"),
                    &mut errors,
                );
                if blocks
                    .get(target)
                    .is_some_and(|target| target.exception_parameter.is_some())
                {
                    errors.push(Diagnostic::new(
                        "exception.handler_shape",
                        format!("{path}.terminator.target"),
                        "normal edge cannot enter an exception handler",
                    ));
                }
            }
            Terminator::Branch {
                then_edge,
                else_edge,
                source,
                ..
            } => {
                successors
                    .entry(block.id.clone())
                    .or_default()
                    .extend([then_edge.target.clone(), else_edge.target.clone()]);
                verify_source(
                    source,
                    &files,
                    &allowed_bindings,
                    &function.environment.id,
                    format!("{path}.terminator.source"),
                    &mut errors,
                );
                for edge in [then_edge, else_edge] {
                    if blocks
                        .get(&edge.target)
                        .is_some_and(|target| target.exception_parameter.is_some())
                    {
                        errors.push(Diagnostic::new(
                            "exception.handler_shape",
                            format!("{path}.terminator"),
                            "normal edge cannot enter an exception handler",
                        ));
                    }
                }
            }
            Terminator::Return { source, .. } | Terminator::Unreachable { source, .. } => {
                verify_source(
                    source,
                    &files,
                    &allowed_bindings,
                    &function.environment.id,
                    format!("{path}.terminator.source"),
                    &mut errors,
                );
            }
        }
    }

    for block in &function.blocks {
        if block.exception_parameter.is_some() && !exception_targets.contains(&block.id) {
            errors.push(Diagnostic::new(
                "exception.handler_unreachable",
                format!("{root}.blocks.{}", block.id),
                "exception parameter has no exceptional predecessor",
            ));
        }
    }

    let mut predecessors: HashMap<String, HashSet<String>> = HashMap::new();
    for (from, targets) in &successors {
        for target in targets {
            if blocks.contains_key(target) {
                predecessors
                    .entry(target.clone())
                    .or_default()
                    .insert(from.clone());
            }
        }
    }
    let mut reachable = HashSet::new();
    let mut queue = VecDeque::from([function.entry_block.clone()]);
    while let Some(block_id) = queue.pop_front() {
        if !reachable.insert(block_id.clone()) {
            continue;
        }
        for target in successors.get(&block_id).into_iter().flatten() {
            if blocks.contains_key(target) {
                queue.push_back(target.clone());
            }
        }
    }
    for block_id in blocks.keys() {
        if !reachable.contains(block_id) {
            errors.push(Diagnostic::new(
                "cfg.unreachable_block",
                format!("{root}.blocks"),
                format!("block {block_id:?} is unreachable from entry"),
            ));
        }
    }
    let mut dominators: HashMap<String, HashSet<String>> = HashMap::new();
    for block_id in &reachable {
        dominators.insert(
            block_id.clone(),
            if block_id == &function.entry_block {
                HashSet::from([block_id.clone()])
            } else {
                reachable.clone()
            },
        );
    }
    loop {
        let mut changed = false;
        for block_id in reachable.iter().filter(|id| **id != function.entry_block) {
            let preds: Vec<_> = predecessors
                .get(block_id)
                .into_iter()
                .flatten()
                .filter(|predecessor| reachable.contains(*predecessor))
                .collect();
            let mut set = preds
                .first()
                .and_then(|predecessor| dominators.get(*predecessor).cloned())
                .unwrap_or_default();
            for predecessor in preds.iter().skip(1) {
                if let Some(parent) = dominators.get(*predecessor) {
                    set.retain(|candidate| parent.contains(candidate));
                }
            }
            set.insert(block_id.clone());
            if dominators.get(block_id) != Some(&set) {
                dominators.insert(block_id.clone(), set);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    for block in &function.blocks {
        let base = format!("{root}.blocks.{}", block.id);
        for (index, instruction) in block.instructions.iter().enumerate() {
            let path = format!("{base}.instructions[{index}]");
            for (operand_index, operand) in instruction.operands.iter().enumerate() {
                check_dominance(
                    operand,
                    &block.id,
                    index as isize,
                    &definitions,
                    &dominators,
                    format!("{path}.operands[{operand_index}]"),
                    &mut errors,
                );
            }
            if let Some(effect_in) = &instruction.effect_in {
                check_dominance(
                    effect_in,
                    &block.id,
                    index as isize,
                    &definitions,
                    &dominators,
                    format!("{path}.effect_in"),
                    &mut errors,
                );
                if !definitions
                    .get(effect_in)
                    .is_some_and(|definition| definition.effect)
                {
                    errors.push(Diagnostic::new(
                        "effect.input",
                        format!("{path}.effect_in"),
                        "effect_in must refer to an effect definition",
                    ));
                }
            }
            if let Some(ExceptionFlow::Edge { target, arguments }) = &instruction.exception {
                let edge = Edge {
                    target: target.clone(),
                    arguments: arguments.clone(),
                };
                let exception_use_index = index as isize + 1;
                if instruction
                    .result
                    .as_ref()
                    .is_some_and(|result| arguments.iter().any(|argument| argument == &result.id))
                {
                    errors.push(Diagnostic::new(
                        "exception.edge_result",
                        format!("{path}.exception.arguments"),
                        "throwing instruction result is unavailable on its exceptional edge",
                    ));
                }
                verify_edge(
                    &edge,
                    &block.id,
                    exception_use_index,
                    &blocks,
                    &definitions,
                    &dominators,
                    format!("{path}.exception"),
                    &mut errors,
                );
            }
        }
        let terminator_index = block.instructions.len() as isize;
        match &block.terminator {
            Terminator::Jump {
                target, arguments, ..
            } => verify_edge(
                &Edge {
                    target: target.clone(),
                    arguments: arguments.clone(),
                },
                &block.id,
                terminator_index,
                &blocks,
                &definitions,
                &dominators,
                format!("{base}.terminator"),
                &mut errors,
            ),
            Terminator::Branch {
                condition,
                then_edge,
                else_edge,
                ..
            } => {
                check_dominance(
                    condition,
                    &block.id,
                    terminator_index,
                    &definitions,
                    &dominators,
                    format!("{base}.terminator.condition"),
                    &mut errors,
                );
                if !definitions
                    .get(condition)
                    .is_some_and(|definition| definition.ty == Some(SemanticType::Boolean))
                {
                    errors.push(Diagnostic::new(
                        "cfg.branch_condition",
                        format!("{base}.terminator.condition"),
                        "branch condition must be a boolean SSA value",
                    ));
                }
                verify_edge(
                    then_edge,
                    &block.id,
                    terminator_index,
                    &blocks,
                    &definitions,
                    &dominators,
                    format!("{base}.terminator.then"),
                    &mut errors,
                );
                verify_edge(
                    else_edge,
                    &block.id,
                    terminator_index,
                    &blocks,
                    &definitions,
                    &dominators,
                    format!("{base}.terminator.else"),
                    &mut errors,
                );
            }
            Terminator::Return {
                value: Some(value), ..
            } => {
                check_dominance(
                    value,
                    &block.id,
                    terminator_index,
                    &definitions,
                    &dominators,
                    format!("{base}.terminator.value"),
                    &mut errors,
                );
                if let Some(signature) = &function.return_signature {
                    if definitions.get(value).is_some_and(|definition| {
                        (definition.ty, definition.rep)
                            != (
                                Some(signature.semantic_type),
                                Some(signature.representation),
                            )
                    }) {
                        errors.push(Diagnostic::new(
                            "function.return_type",
                            format!("{base}.terminator.value"),
                            "returned value differs from declared return type",
                        ));
                    }
                } else {
                    errors.push(Diagnostic::new(
                        "function.return_type",
                        format!("{base}.terminator.value"),
                        "void function cannot return a value",
                    ));
                }
            }
            Terminator::Return { value: None, .. } if function.return_signature.is_some() => errors
                .push(Diagnostic::new(
                    "function.return_type",
                    format!("{base}.terminator.value"),
                    "function with a return signature must return a value",
                )),
            Terminator::Unreachable {
                reason: UnreachableReason::AfterThrow,
                ..
            } if !block
                .instructions
                .last()
                .is_some_and(|instruction| instruction.op == "js.throw") =>
            {
                errors.push(Diagnostic::new(
                    "exception.throw_terminator",
                    format!("{base}.terminator"),
                    "after_throw must immediately follow js.throw",
                ))
            }
            _ => {}
        }
    }

    if any_effectful {
        if function.effect_entry.is_none() {
            errors.push(Diagnostic::new(
                "effect.entry_missing",
                format!("{root}.effect_entry"),
                "effectful function requires effect_entry",
            ));
        }
        for block in &function.blocks {
            let path = format!("{root}.blocks.{}", block.id);
            let Some(BlockParameter::Effect(entry)) = block.parameters.last() else {
                errors.push(Diagnostic::new(
                    "effect.block_parameter",
                    format!("{path}.parameters"),
                    "every block in an effectful function needs a final effect parameter",
                ));
                continue;
            };
            if block.id == function.entry_block
                && function.effect_entry.as_deref() != Some(&entry.id)
            {
                errors.push(Diagnostic::new(
                    "effect.entry_mismatch",
                    format!("{root}.effect_entry"),
                    "effect_entry must equal the entry block effect parameter",
                ));
            }
            let mut current = entry.id.as_str();
            for (index, instruction) in block.instructions.iter().enumerate() {
                let instruction_path = format!("{path}.instructions[{index}]");
                if instruction.effects.is_effectful() {
                    if instruction.effect_in.as_deref() != Some(current)
                        || instruction.effect_out.is_none()
                    {
                        errors.push(Diagnostic::new("effect.chain", instruction_path.clone(), "effectful instruction must consume current token and produce next token"));
                    }
                    if let Some(next) = instruction.effect_out.as_deref() {
                        if !definitions
                            .get(next)
                            .is_some_and(|definition| definition.effect)
                        {
                            errors.push(Diagnostic::new(
                                "effect.output",
                                format!("{instruction_path}.effect_out"),
                                "effect_out must define an effect token",
                            ));
                        }
                        current = next;
                    }
                    if let Some(ExceptionFlow::Edge { arguments, .. }) = &instruction.exception {
                        if arguments.last().map(String::as_str) != Some(current) {
                            errors.push(Diagnostic::new(
                                "effect.exception_edge",
                                format!("{instruction_path}.exception.arguments"),
                                "exception edge must forward effect_out as its final argument",
                            ));
                        }
                    }
                } else if instruction.effect_in.is_some() || instruction.effect_out.is_some() {
                    errors.push(Diagnostic::new(
                        "effect.pure_instruction",
                        instruction_path,
                        "pure instruction cannot use effect tokens",
                    ));
                }
            }
            match &block.terminator {
                Terminator::Jump { arguments, .. }
                    if arguments.last().map(String::as_str) != Some(current) =>
                {
                    errors.push(Diagnostic::new(
                        "effect.edge",
                        format!("{path}.terminator.arguments"),
                        "jump must forward current effect token",
                    ))
                }
                Terminator::Branch {
                    then_edge,
                    else_edge,
                    ..
                } if then_edge.arguments.last().map(String::as_str) != Some(current)
                    || else_edge.arguments.last().map(String::as_str) != Some(current) =>
                {
                    errors.push(Diagnostic::new(
                        "effect.edge",
                        format!("{path}.terminator"),
                        "both branch edges must forward current effect token",
                    ))
                }
                _ => {}
            }
        }
    } else {
        if function.effect_entry.is_some()
            || function.blocks.iter().any(|block| {
                block
                    .parameters
                    .iter()
                    .any(|param| matches!(param, BlockParameter::Effect(_)))
            })
        {
            errors.push(Diagnostic::new(
                "effect.unexpected",
                format!("{root}.effect_entry"),
                "pure function cannot declare effect tokens",
            ));
        }
        for block in &function.blocks {
            for instruction in &block.instructions {
                if instruction.effect_in.is_some() || instruction.effect_out.is_some() {
                    errors.push(Diagnostic::new(
                        "effect.pure_instruction",
                        format!("{root}.blocks.{}.instructions", block.id),
                        "pure function cannot use effect tokens",
                    ));
                }
            }
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Path};

    #[test]
    fn contract_identifier_is_stable() {
        assert_eq!(CONTRACT_ID, "ogma.core-ir/1.0.0");
    }

    #[test]
    fn existing_canonical_corpus_is_accepted_or_rejected_as_declared() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/core-ir");
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("manifest.json")).expect("manifest"))
                .expect("valid manifest JSON");
        for fixture in manifest["fixtures"].as_array().expect("fixture list") {
            let relative = fixture["path"].as_str().expect("path");
            let expected = fixture["expect"].as_str().expect("expectation");
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../")
                .join(relative);
            let input =
                fs::read_to_string(&path).unwrap_or_else(|error| panic!("{relative}: {error}"));
            let result = parse_and_verify(&input);
            match expected {
                "accepted" => assert!(result.is_ok(), "{relative}: {result:?}"),
                "rejected" => assert!(result.is_err(), "{relative} was accepted"),
                other => panic!("unknown fixture expectation {other:?}"),
            }
        }
    }

    #[test]
    fn typed_model_round_trips_a_positive_fixture() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/core-ir/fixtures/positive-ssa-branch.json");
        let input = fs::read_to_string(path).expect("fixture exists");
        let doc = parse_and_verify(&input).expect("fixture verifies");
        let encoded = serde_json::to_string(&doc).expect("typed model serializes");
        assert_eq!(
            doc,
            parse_and_verify(&encoded).expect("serialized model verifies")
        );
    }

    #[test]
    fn rejects_non_normalized_source_paths() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/core-ir/fixtures/positive-ssa-branch.json");
        let mut doc: FunctionDocument =
            serde_json::from_slice(&fs::read(path).expect("fixture")).expect("typed fixture");
        doc.function.source_files[0].path = "../outside.js".to_owned();
        assert!(
            verify_document(&doc)
                .iter()
                .any(|error| error.code == "source.path")
        );
    }
}
