//! Portable OGMB v1 container and register-bytecode verification boundary.
//!
//! This crate has no Runtime or compiler dependency. It owns the stable
//! container contract and validates decoded register instructions before a
//! compiler or interpreter consumes an artifact.

#![deny(missing_docs)]

use serde::Deserialize;
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

/// Current version of the portable OGMB container contract.
pub const FORMAT_VERSION: u16 = 1;
/// Registered decoder identity for the initial M1 register encoding.
pub const REGISTER_ENCODING_ID: &str = "ogma.register.m1.v1";

const HEADER_SIZE: usize = 48;
const DIRECTORY_ENTRY_SIZE: usize = 32;
const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;
const MAX_JSON_ARRAY_ITEMS: usize = 1_000_000;
const MAX_JSON_STRING_BYTES: usize = MAX_FILE_BYTES;
const MAX_SECTIONS: usize = 64;
const MAX_ALIGNMENT: u32 = 64;
const REQUIRED_SECTIONS: [u32; 4] = [1, 2, 3, 4];

type ParsedContainer = (Value, Vec<u8>, Vec<Value>, Vec<Value>, Option<Value>);

const OP_CONSTANT: u8 = 0x31;
const OP_UNARY: u8 = 0x32;
const OP_BINARY: u8 = 0x33;
const OP_CALL: u8 = 0x34;
const OP_JUMP: u8 = 0x37;
const OP_BRANCH: u8 = 0x38;
const OP_RETURN: u8 = 0x39;
const OP_THROW: u8 = 0x3a;

/// Structured diagnostic returned by the bytecode boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    /// Stable machine-readable diagnostic code.
    pub code: String,
    /// JSONPath-like field or byte offset.
    pub path: String,
    /// Human-readable explanation.
    pub message: String,
}

impl Diagnostic {
    fn new(code: &str, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            path: path.into(),
            message: message.into(),
        }
    }
}

/// JavaScript or compiler-internal kind of a constant instruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ConstantKind {
    /// JavaScript undefined.
    Undefined = 0,
    /// JavaScript null.
    Null = 1,
    /// JavaScript boolean.
    Boolean = 2,
    /// JavaScript Number, including canonical special-value strings.
    Number = 3,
    /// JavaScript BigInt represented by a canonical decimal string.
    BigInt = 4,
    /// JavaScript string.
    String = 5,
    /// Uninitialized lexical binding marker used for TDZ checks.
    Uninitialized = 6,
    /// Capability identifier consumed only by a host-call operation.
    HostCapability = 7,
    /// Intrinsic operation token consumed only by a call operation.
    Intrinsic = 8,
    /// Stable target function identifier consumed only by closure creation.
    FunctionId = 9,
    /// Stable lexical binding identifier consumed by binding-store instructions.
    BindingId = 10,
}

impl ConstantKind {
    fn from_byte(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Undefined,
            1 => Self::Null,
            2 => Self::Boolean,
            3 => Self::Number,
            4 => Self::BigInt,
            5 => Self::String,
            6 => Self::Uninitialized,
            7 => Self::HostCapability,
            8 => Self::Intrinsic,
            9 => Self::FunctionId,
            10 => Self::BindingId,
            _ => return None,
        })
    }
}

/// Unary operation used by the initial register encoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum UnaryOperation {
    /// Copy a value between registers.
    Copy = 0,
    /// ECMAScript ToBoolean.
    ToBoolean = 1,
    /// ECMAScript ToNumber.
    ToNumber = 2,
    /// ECMAScript ToString.
    ToString = 3,
    /// ECMAScript typeof.
    TypeOf = 4,
    /// Load a lexical binding, performing its TDZ check.
    LoadBinding = 5,
}

impl UnaryOperation {
    fn from_byte(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Copy,
            1 => Self::ToBoolean,
            2 => Self::ToNumber,
            3 => Self::ToString,
            4 => Self::TypeOf,
            5 => Self::LoadBinding,
            _ => return None,
        })
    }
}

/// Binary operation used by the initial register encoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum BinaryOperation {
    /// ECMAScript addition with coercion.
    Add = 0,
    /// ECMAScript strict equality.
    StrictEqual = 1,
    /// ECMAScript abstract relational less-than.
    LessThan = 2,
    /// ECMAScript greater-than.
    GreaterThan = 3,
}

impl BinaryOperation {
    fn from_byte(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Add,
            1 => Self::StrictEqual,
            2 => Self::LessThan,
            3 => Self::GreaterThan,
            _ => return None,
        })
    }
}

/// Semantic kind of register call or compiler intrinsic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CallKind {
    /// JavaScript call: callee, thisArg, then arguments.
    JavaScript = 0,
    /// JavaScript construction: constructor, newTarget, then arguments.
    Construct = 1,
    /// Capability-mediated host call: capability token, then arguments.
    Host = 2,
    /// Named property read: object and property key.
    GetProperty = 3,
    /// Named property write: object, key and value.
    SetProperty = 4,
    /// Computed property read: object and key.
    GetElement = 5,
    /// Computed property write: object, key and value.
    SetElement = 6,
    /// Closure creation: function identity followed by captured values/cells.
    MakeClosure = 7,
    /// Read a captured lexical cell.
    LoadCapture = 8,
    /// Write a captured lexical cell.
    StoreCapture = 9,
    /// Materialize a shared mutable lexical cell.
    MakeCell = 10,
    /// Store a value through a local lexical binding slot.
    StoreBinding = 11,
}

impl CallKind {
    fn from_byte(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::JavaScript,
            1 => Self::Construct,
            2 => Self::Host,
            3 => Self::GetProperty,
            4 => Self::SetProperty,
            5 => Self::GetElement,
            6 => Self::SetElement,
            7 => Self::MakeClosure,
            8 => Self::LoadCapture,
            9 => Self::StoreCapture,
            10 => Self::MakeCell,
            11 => Self::StoreBinding,
            _ => return None,
        })
    }

    fn accepts_source_count(self, count: usize) -> bool {
        match self {
            Self::JavaScript | Self::Construct => (2..=1025).contains(&count),
            Self::Host | Self::MakeClosure => (1..=1025).contains(&count),
            Self::GetProperty | Self::GetElement => count == 2,
            Self::SetProperty | Self::SetElement => count == 3,
            Self::LoadCapture | Self::MakeCell => count == 1,
            Self::StoreCapture | Self::StoreBinding => count == 2,
        }
    }
}

/// One register instruction before byte-offset layout.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegisterOperation {
    /// Load a constant-pool value into a register.
    Constant {
        /// Destination register.
        destination: u16,
        /// Constant-pool index.
        constant: u32,
        /// Semantic kind of the constant.
        kind: ConstantKind,
    },
    /// Apply a unary semantic operation.
    Unary {
        /// Destination register.
        destination: u16,
        /// Source register.
        source: u16,
        /// Unary operation.
        operation: UnaryOperation,
    },
    /// Apply a binary semantic operation.
    Binary {
        /// Destination register.
        destination: u16,
        /// Left source register.
        left: u16,
        /// Right source register.
        right: u16,
        /// Binary operation.
        operation: BinaryOperation,
    },
    /// Invoke a function or compiler intrinsic.
    Call {
        /// Destination register.
        destination: u16,
        /// Call semantic.
        kind: CallKind,
        /// Source registers in semantic evaluation order.
        sources: Vec<u16>,
    },
    /// Jump to an absolute byte offset.
    Jump {
        /// Target instruction offset.
        target: u32,
    },
    /// Branch to two absolute byte offsets.
    Branch {
        /// Already-converted JavaScript boolean register.
        condition: u16,
        /// True target.
        then_target: u32,
        /// False target.
        else_target: u32,
    },
    /// Return a register value from the function.
    Return {
        /// Return-value register.
        source: u16,
    },
    /// Throw a JavaScript value from the function.
    Throw {
        /// Thrown-value register.
        source: u16,
    },
}

/// Register instruction and its conservative exceptional-effect flag.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterInstruction {
    /// Encoded instruction operation.
    pub operation: RegisterOperation,
    /// Whether executing the operation can throw.
    pub may_throw: bool,
}

impl RegisterInstruction {
    /// Exact byte length for this instruction's current encoding.
    pub fn encoded_len(&self) -> usize {
        match &self.operation {
            RegisterOperation::Constant { .. } => 9,
            RegisterOperation::Unary { .. } => 7,
            RegisterOperation::Binary { .. } => 9,
            RegisterOperation::Call { sources, .. } => {
                7usize.saturating_add(sources.len().saturating_mul(2))
            }
            RegisterOperation::Jump { .. } => 6,
            RegisterOperation::Branch { .. } => 12,
            RegisterOperation::Return { .. } | RegisterOperation::Throw { .. } => 4,
        }
    }
}

/// Encodes a laid-out register instruction stream.
///
/// All branch destinations must already reference absolute instruction byte
/// offsets. The opcode values and operand widths are an M1 implementation
/// detail, not a frozen public ISA.
pub fn encode_register_code(
    instructions: &[RegisterInstruction],
) -> Result<Vec<u8>, Vec<Diagnostic>> {
    if instructions.is_empty() {
        return Err(vec![Diagnostic::new(
            "bytecode.empty",
            "$.instructions",
            "a function must contain at least one instruction",
        )]);
    }
    if instructions.len() > 1_000_000 {
        return Err(vec![Diagnostic::new(
            "resource.instruction_count",
            "$.instructions",
            "function exceeds the OGMB v1 hard instruction-count limit",
        )]);
    }
    for (index, instruction) in instructions.iter().enumerate() {
        if let RegisterOperation::Call { kind, sources, .. } = &instruction.operation {
            if sources.is_empty() || sources.len() > 1025 {
                return Err(vec![Diagnostic::new(
                    "operand.call_arity",
                    format!("$.instructions[{index}]"),
                    "call requires between one and 1025 source registers",
                )]);
            }
            if !kind.accepts_source_count(sources.len()) {
                return Err(vec![Diagnostic::new(
                    "call.kind_arity",
                    format!("$.instructions[{index}].sources"),
                    format!(
                        "call kind {kind:?} does not accept {} source registers",
                        sources.len()
                    ),
                )]);
            }
            if sources.iter().copied().collect::<BTreeSet<_>>().len() != sources.len() {
                return Err(vec![Diagnostic::new(
                    "register.effect_duplicate",
                    format!("$.instructions[{index}].sources"),
                    "a call cannot read the same register more than once",
                )]);
            }
        }
    }
    let total = instructions
        .iter()
        .try_fold(0usize, |sum, item| sum.checked_add(item.encoded_len()));
    let Some(total) = total else {
        return Err(vec![Diagnostic::new(
            "resource.code_size",
            "$.instructions",
            "encoded instruction size overflows the host address space",
        )]);
    };
    if total > MAX_FILE_BYTES || total > u32::MAX as usize {
        return Err(vec![Diagnostic::new(
            "resource.code_size",
            "$.instructions",
            "encoded code exceeds the OGMB v1 size limit",
        )]);
    }

    let mut output = Vec::with_capacity(total);
    for (index, instruction) in instructions.iter().enumerate() {
        let flags = u8::from(instruction.may_throw);
        match &instruction.operation {
            RegisterOperation::Constant {
                destination,
                constant,
                kind,
            } => {
                output.extend_from_slice(&[OP_CONSTANT, flags]);
                push_u16(&mut output, *destination);
                push_u32(&mut output, *constant);
                output.push(*kind as u8);
            }
            RegisterOperation::Unary {
                destination,
                source,
                operation,
            } => {
                output.extend_from_slice(&[OP_UNARY, flags]);
                push_u16(&mut output, *destination);
                push_u16(&mut output, *source);
                output.push(*operation as u8);
            }
            RegisterOperation::Binary {
                destination,
                left,
                right,
                operation,
            } => {
                output.extend_from_slice(&[OP_BINARY, flags]);
                push_u16(&mut output, *destination);
                push_u16(&mut output, *left);
                push_u16(&mut output, *right);
                output.push(*operation as u8);
            }
            RegisterOperation::Call {
                destination,
                kind,
                sources,
            } => {
                if sources.is_empty() || sources.len() > 1025 {
                    return Err(vec![Diagnostic::new(
                        "operand.call_arity",
                        format!("$.instructions[{index}]"),
                        "call requires between one and 1025 source registers",
                    )]);
                }
                if !kind.accepts_source_count(sources.len()) {
                    return Err(vec![Diagnostic::new(
                        "call.kind_arity",
                        format!("$.instructions[{index}].sources"),
                        format!(
                            "call kind {kind:?} does not accept {} source registers",
                            sources.len()
                        ),
                    )]);
                }
                let unique: BTreeSet<u16> = sources.iter().copied().collect();
                if unique.len() != sources.len() {
                    return Err(vec![Diagnostic::new(
                        "register.effect_duplicate",
                        format!("$.instructions[{index}].sources"),
                        "a call cannot read the same register more than once",
                    )]);
                }
                output.extend_from_slice(&[OP_CALL, flags]);
                push_u16(&mut output, *destination);
                output.push(*kind as u8);
                push_u16(&mut output, sources.len() as u16);
                for source in sources {
                    push_u16(&mut output, *source);
                }
            }
            RegisterOperation::Jump { target } => {
                output.extend_from_slice(&[OP_JUMP, flags]);
                push_u32(&mut output, *target);
            }
            RegisterOperation::Branch {
                condition,
                then_target,
                else_target,
            } => {
                output.extend_from_slice(&[OP_BRANCH, flags]);
                push_u16(&mut output, *condition);
                push_u32(&mut output, *then_target);
                push_u32(&mut output, *else_target);
            }
            RegisterOperation::Return { source } => {
                output.extend_from_slice(&[OP_RETURN, flags]);
                push_u16(&mut output, *source);
            }
            RegisterOperation::Throw { source } => {
                output.extend_from_slice(&[OP_THROW, flags | 1]);
                push_u16(&mut output, *source);
            }
        }
    }
    if output.len() != total {
        return Err(vec![Diagnostic::new(
            "bytecode.layout",
            "$.instructions",
            "encoded instruction lengths do not match the layout",
        )]);
    }
    Ok(output)
}

/// Artifact returned only after container and register verification succeeds.
#[derive(Clone, Debug, PartialEq)]
pub struct VerifiedArtifact {
    /// Exact OGMB v1 container bytes.
    pub bytes: Vec<u8>,
    /// Parsed OGMB manifest.
    pub manifest: Value,
    /// Encoded register instruction section.
    pub code: Vec<u8>,
    /// Constant-pool JSON scalars.
    pub constants: Vec<Value>,
    /// Exception-region records.
    pub exceptions: Vec<Value>,
    /// Optional source/debug map.
    pub source_map: Option<Value>,
    /// Normalized facts for the repository bytecode verifier contract.
    pub verification_input: Value,
}

/// Builds the existing OGMB v1 container and binds digests to its code section.
pub fn build_ogmb_v1(
    mut manifest: Value,
    code: &[u8],
    constants: &[Value],
    exceptions: &[Value],
    source_map: Option<&Value>,
) -> Result<Vec<u8>, Vec<Diagnostic>> {
    if code.is_empty() {
        return Err(vec![Diagnostic::new(
            "container.empty_code",
            "$.sections.code",
            "function code must not be empty",
        )]);
    }
    if code.len() > MAX_FILE_BYTES || constants.len() > 262_144 || exceptions.len() > 65_536 {
        return Err(vec![Diagnostic::new(
            "resource.section_limit",
            "$.sections",
            "code, constant pool or exception section exceeds an OGMB v1 hard limit",
        )]);
    }
    if constants.iter().any(|value| !is_json_scalar(value)) {
        return Err(vec![Diagnostic::new(
            "constant_pool.shape",
            "$.sections.constants",
            "the OGMB v1 constant pool accepts JSON scalar values only",
        )]);
    }
    let digest = sha256_hex(code);
    let Some(root) = manifest.as_object_mut() else {
        return Err(vec![Diagnostic::new(
            "manifest.shape",
            "$.manifest",
            "manifest must be a JSON object",
        )]);
    };
    root.insert("code_sha256".to_owned(), Value::String(digest.clone()));
    let Some(summary) = root
        .get_mut("metadata")
        .and_then(Value::as_object_mut)
        .and_then(|metadata| metadata.get_mut("verifier_summary"))
        .and_then(Value::as_object_mut)
    else {
        return Err(vec![Diagnostic::new(
            "metadata.verifier_summary",
            "$.manifest.metadata.verifier_summary",
            "manifest metadata must contain verifier_summary",
        )]);
    };
    summary.insert("code_sha256".to_owned(), Value::String(digest));

    let serialize = |value: &Value, path: &str| {
        serde_json::to_vec(value).map_err(|error| {
            vec![Diagnostic::new(
                "container.serialize",
                path,
                format!("could not serialize JSON section: {error}"),
            )]
        })
    };
    let manifest_bytes = serialize(&manifest, "$.manifest")?;
    let constants_bytes = serialize(&Value::Array(constants.to_vec()), "$.sections.constants")?;
    let exceptions_bytes = serialize(&Value::Array(exceptions.to_vec()), "$.sections.exceptions")?;
    let source_bytes = source_map
        .map(|value| serialize(value, "$.sections.source_map"))
        .transpose()?;

    let mut sections = vec![
        (1u32, 0u32, manifest_bytes),
        (2u32, 0u32, code.to_vec()),
        (3u32, 0u32, constants_bytes),
        (4u32, 0u32, exceptions_bytes),
    ];
    if let Some(bytes) = source_bytes {
        sections.push((5, 1, bytes));
    }
    if sections.len() > MAX_SECTIONS {
        return Err(vec![Diagnostic::new(
            "container.section_count",
            "$.directory",
            "section count exceeds the OGMB v1 hard limit",
        )]);
    }

    let directory_end = HEADER_SIZE + sections.len() * DIRECTORY_ENTRY_SIZE;
    let mut output = vec![0u8; directory_end];
    let mut entries = Vec::with_capacity(sections.len());
    for (section_type, flags, payload) in sections {
        let aligned = align_up(output.len(), 8).ok_or_else(|| {
            vec![Diagnostic::new(
                "container.layout",
                "$.directory",
                "section alignment overflowed",
            )]
        })?;
        output.resize(aligned, 0);
        let offset = output.len();
        output.extend_from_slice(&payload);
        entries.push((
            section_type,
            flags,
            offset as u64,
            payload.len() as u64,
            8u32,
            0u32,
        ));
        if output.len() > MAX_FILE_BYTES {
            return Err(vec![Diagnostic::new(
                "resource.file_size",
                "$",
                "OGMB v1 artifact exceeds the 64 MiB hard limit",
            )]);
        }
    }

    output[0..4].copy_from_slice(b"OGMB");
    write_u16_at(&mut output, 4, 1);
    write_u16_at(&mut output, 6, 0);
    write_u32_at(&mut output, 8, HEADER_SIZE as u32);
    write_u64_at(&mut output, 12, 0); // v1 feature flags
    let file_size = output.len() as u64;
    write_u64_at(&mut output, 20, file_size);
    write_u64_at(&mut output, 28, HEADER_SIZE as u64);
    write_u32_at(&mut output, 36, entries.len() as u32);
    write_u32_at(&mut output, 40, DIRECTORY_ENTRY_SIZE as u32);
    write_u32_at(&mut output, 44, 0);

    for (index, entry) in entries.iter().enumerate() {
        let base = HEADER_SIZE + index * DIRECTORY_ENTRY_SIZE;
        write_u32_at(&mut output, base, entry.0);
        write_u32_at(&mut output, base + 4, entry.1);
        write_u64_at(&mut output, base + 8, entry.2);
        write_u64_at(&mut output, base + 16, entry.3);
        write_u32_at(&mut output, base + 24, entry.4);
        write_u32_at(&mut output, base + 28, entry.5);
    }
    Ok(output)
}

/// Reads, decodes and verifies an M1 register artifact.
///
/// Entry registers are supplied by the compiler because the normalized
/// verification-input contract keeps decoder facts outside the container.
/// They contain the function parameters and imported closure captures.
pub fn verify_register_artifact(
    bytes: &[u8],
    entry_registers: &[u16],
) -> Result<VerifiedArtifact, Vec<Diagnostic>> {
    let (manifest, code, constants, exceptions, source_map) = read_container(bytes)?;
    if manifest.get("encoding_id").and_then(Value::as_str) != Some(REGISTER_ENCODING_ID) {
        return Err(vec![Diagnostic::new(
            "manifest.encoding_id",
            "$.manifest.encoding_id",
            format!("expected registered encoding {REGISTER_ENCODING_ID}"),
        )]);
    }
    let metadata = manifest.get("metadata").ok_or_else(|| {
        vec![Diagnostic::new(
            "metadata.shape",
            "$.manifest.metadata",
            "metadata is required",
        )]
    })?;
    validate_metadata(metadata)?;
    if metadata.get("execution_model").and_then(Value::as_str) != Some("register") {
        return Err(vec![Diagnostic::new(
            "metadata.execution_model",
            "$.manifest.metadata.execution_model",
            "M1 artifacts must use register execution",
        )]);
    }
    let frame_count = read_u32_value(
        metadata.pointer("/frame_layout/register_count"),
        "$.manifest.metadata.frame_layout.register_count",
    )?;
    let parameter_count = read_u32_value(
        metadata.pointer("/frame_layout/parameter_count"),
        "$.manifest.metadata.frame_layout.parameter_count",
    )?;
    let capture_count = metadata
        .pointer("/environment/captures")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if frame_count == 0 || frame_count > 65_536 {
        return Err(vec![Diagnostic::new(
            "frame.register_count",
            "$.manifest.metadata.frame_layout.register_count",
            "register_count must be in the range 1..=65536",
        )]);
    }
    let capture_count = u32::try_from(capture_count).map_err(|_| {
        vec![Diagnostic::new(
            "register.entry_arity",
            "$.manifest.metadata.environment.captures",
            "capture count exceeds the register encoding limit",
        )]
    })?;
    let expected_entry_count = parameter_count.checked_add(capture_count).ok_or_else(|| {
        vec![Diagnostic::new(
            "register.entry_arity",
            "$.manifest.metadata.frame_layout",
            "parameter and capture counts overflow the entry-register layout",
        )]
    })?;
    if expected_entry_count > frame_count {
        return Err(vec![Diagnostic::new(
            "register.entry_frame",
            "$.manifest.metadata.frame_layout",
            "parameter and capture entry registers do not fit the declared frame",
        )]);
    }
    if entry_registers.len() != expected_entry_count as usize {
        return Err(vec![Diagnostic::new(
            "register.entry_arity",
            "$.entry_registers",
            format!(
                "entry registers must contain one initialized register per parameter and capture (expected {expected_entry_count}, got {})",
                entry_registers.len()
            ),
        )]);
    }
    let expected_entry_registers = (0..expected_entry_count)
        .map(|index| index as u16)
        .collect::<Vec<_>>();
    if entry_registers != expected_entry_registers {
        return Err(vec![Diagnostic::new(
            "register.entry_layout",
            "$.entry_registers",
            "M1 entry layout assigns parameters first, followed by captures in metadata order",
        )]);
    }
    let limits = manifest.get("resource_limits").ok_or_else(|| {
        vec![Diagnostic::new(
            "resource.limits_shape",
            "$.manifest.resource_limits",
            "resource limits are required",
        )]
    })?;
    validate_resource_limits(limits)?;
    let max_code = read_u32_value(
        limits.get("max_code_bytes"),
        "$.manifest.resource_limits.max_code_bytes",
    )? as usize;
    let max_instructions = read_u32_value(
        limits.get("max_instructions"),
        "$.manifest.resource_limits.max_instructions",
    )? as usize;
    let max_constants = read_u32_value(
        limits.get("max_constants"),
        "$.manifest.resource_limits.max_constants",
    )? as usize;
    let max_registers = read_u32_value(
        limits.get("max_registers"),
        "$.manifest.resource_limits.max_registers",
    )?;
    let max_locals = read_u32_value(
        limits.get("max_locals"),
        "$.manifest.resource_limits.max_locals",
    )?;
    let local_count = read_u32_value(
        metadata.pointer("/frame_layout/local_count"),
        "$.manifest.metadata.frame_layout.local_count",
    )?;
    if local_count > max_locals {
        return Err(vec![Diagnostic::new(
            "resource.local_limit",
            "$.manifest.metadata.frame_layout.local_count",
            "frame local_count exceeds the manifest max_locals limit",
        )]);
    }
    let max_exceptions = read_u32_value(
        limits.get("max_exception_regions"),
        "$.manifest.resource_limits.max_exception_regions",
    )?;
    if code.len() > max_code || constants.len() > max_constants || frame_count > max_registers {
        return Err(vec![Diagnostic::new(
            "resource.limit",
            "$.manifest.resource_limits",
            "artifact exceeds one or more declared code, constant or register limits",
        )]);
    }
    if !exceptions.is_empty() || exceptions.len() > max_exceptions as usize {
        return Err(vec![Diagnostic::new(
            "exception.local_handlers_unsupported",
            "$.sections.exceptions",
            "the current M1 profile supports propagated throws but does not lower local catch/finally handlers",
        )]);
    }
    let digest = sha256_hex(&code);
    if manifest.get("code_sha256").and_then(Value::as_str) != Some(digest.as_str()) {
        return Err(vec![Diagnostic::new(
            "container.code_digest",
            "$.manifest.code_sha256",
            "manifest code digest does not match the encoded code section",
        )]);
    }
    let (instructions, verification_input, max_register_index) =
        decode_register_code(&code, &digest, entry_registers, max_instructions)?;
    verify_register_program(
        &instructions,
        frame_count,
        entry_registers,
        &constants,
        code.len(),
    )?;
    if let Some(map) = source_map.as_ref() {
        validate_source_map(map, metadata, &instructions, code.len())?;
    }

    let summary = metadata
        .get("verifier_summary")
        .expect("validated metadata");
    let expected = (
        read_u32_value(
            summary.get("instruction_count"),
            "$.manifest.metadata.verifier_summary.instruction_count",
        )?,
        read_u32_value(
            summary.get("max_register_index"),
            "$.manifest.metadata.verifier_summary.max_register_index",
        )?,
        read_u32_value(
            summary.get("max_stack_depth"),
            "$.manifest.metadata.verifier_summary.max_stack_depth",
        )?,
        read_u32_value(
            summary.get("exception_region_count"),
            "$.manifest.metadata.verifier_summary.exception_region_count",
        )?,
        read_u32_value(
            summary.get("constant_count"),
            "$.manifest.metadata.verifier_summary.constant_count",
        )?,
    );
    if expected.0 as usize != instructions.len()
        || expected.1 != max_register_index
        || expected.2 != 0
        || expected.3 != exceptions.len() as u32
        || expected.4 != constants.len() as u32
        || summary.get("schema").and_then(Value::as_str)
            != Some("ogma.bytecode-verification-summary")
        || summary.get("version").and_then(Value::as_str) != Some("1.0.0")
        || summary.get("code_sha256").and_then(Value::as_str) != Some(digest.as_str())
    {
        return Err(vec![Diagnostic::new(
            "verifier_summary.mismatch",
            "$.manifest.metadata.verifier_summary",
            "verifier summary does not match decoded instructions, registers, constants, exceptions or code digest",
        )]);
    }

    Ok(VerifiedArtifact {
        bytes: bytes.to_vec(),
        manifest,
        code,
        constants,
        exceptions,
        source_map,
        verification_input,
    })
}

#[derive(Clone, Debug)]
struct DecodedInstruction {
    offset: u32,
    size: u32,
    opcode: u32,
    operation: &'static str,
    operands: Vec<u32>,
    constant_refs: Vec<u32>,
    constant_kind: Option<ConstantKind>,
    successors: Vec<u32>,
    may_throw: bool,
    reads: Vec<u16>,
    writes: Vec<u16>,
    argument_count: Option<u32>,
}

fn decode_register_code(
    code: &[u8],
    digest: &str,
    entry_registers: &[u16],
    max_instructions: usize,
) -> Result<(Vec<DecodedInstruction>, Value, u32), Vec<Diagnostic>> {
    let mut decoded = Vec::new();
    let mut cursor = 0usize;
    while cursor < code.len() {
        if decoded.len() >= max_instructions {
            return Err(vec![Diagnostic::new(
                "resource.instruction_count",
                "$.instructions",
                "decoded instruction count exceeds the declared limit",
            )]);
        }
        let start = cursor;
        let opcode = read_u8(code, &mut cursor).map_err(|error| vec![error])?;
        let flags = read_u8(code, &mut cursor).map_err(|error| vec![error])?;
        if flags & !1 != 0 {
            return Err(vec![Diagnostic::new(
                "opcode.flags",
                format!("$.instructions[{start}]"),
                "unknown instruction flags are not permitted",
            )]);
        }
        let may_throw = flags & 1 != 0;
        let mut requires_may_throw = false;
        let mut operands = Vec::new();
        let mut constants = Vec::new();
        let mut constant_kind = None;
        let mut successors = Vec::new();
        let mut reads = Vec::new();
        let mut writes = Vec::new();
        let mut argument_count = None;
        let operation = match opcode {
            OP_CONSTANT => {
                let destination = read_u16(code, &mut cursor).map_err(|error| vec![error])?;
                let constant = read_u32(code, &mut cursor).map_err(|error| vec![error])?;
                let kind_tag = read_u8(code, &mut cursor).map_err(|error| vec![error])?;
                let Some(kind) = ConstantKind::from_byte(kind_tag) else {
                    return Err(vec![Diagnostic::new(
                        "constant.kind",
                        format!("$.instructions[{start}]"),
                        format!("unknown constant kind {kind_tag}"),
                    )]);
                };
                constant_kind = Some(kind);
                writes.push(destination);
                constants.push(constant);
                "constant"
            }
            OP_UNARY => {
                let destination = read_u16(code, &mut cursor).map_err(|error| vec![error])?;
                let source = read_u16(code, &mut cursor).map_err(|error| vec![error])?;
                let operation = read_u8(code, &mut cursor).map_err(|error| vec![error])?;
                if UnaryOperation::from_byte(operation).is_none() {
                    return Err(vec![Diagnostic::new(
                        "unary.operation",
                        format!("$.instructions[{start}]"),
                        format!("unknown unary operation {operation}"),
                    )]);
                }
                requires_may_throw = matches!(operation, 2 | 3 | 5);
                reads.push(source);
                writes.push(destination);
                "unary"
            }
            OP_BINARY => {
                let destination = read_u16(code, &mut cursor).map_err(|error| vec![error])?;
                let left = read_u16(code, &mut cursor).map_err(|error| vec![error])?;
                let right = read_u16(code, &mut cursor).map_err(|error| vec![error])?;
                let operation = read_u8(code, &mut cursor).map_err(|error| vec![error])?;
                if BinaryOperation::from_byte(operation).is_none() {
                    return Err(vec![Diagnostic::new(
                        "binary.operation",
                        format!("$.instructions[{start}]"),
                        format!("unknown binary operation {operation}"),
                    )]);
                }
                requires_may_throw = matches!(operation, 0 | 2 | 3);
                reads.extend([left, right]);
                writes.push(destination);
                "binary"
            }
            OP_CALL => {
                let destination = read_u16(code, &mut cursor).map_err(|error| vec![error])?;
                let kind = read_u8(code, &mut cursor).map_err(|error| vec![error])?;
                let Some(call_kind) = CallKind::from_byte(kind) else {
                    return Err(vec![Diagnostic::new(
                        "call.kind",
                        format!("$.instructions[{start}]"),
                        format!("unknown call kind {kind}"),
                    )]);
                };
                // All call semantics except pure closure materialization can
                // complete abruptly in the M1 compatibility profile.
                requires_may_throw = call_kind != CallKind::MakeClosure;
                let count = read_u16(code, &mut cursor).map_err(|error| vec![error])? as usize;
                if count == 0 || count > 1025 {
                    return Err(vec![Diagnostic::new(
                        "operand.call_arity",
                        format!("$.instructions[{start}]"),
                        "call requires between one and 1025 source registers",
                    )]);
                }
                if !call_kind.accepts_source_count(count) {
                    return Err(vec![Diagnostic::new(
                        "call.kind_arity",
                        format!("$.instructions[{start}].sources"),
                        format!("call kind {call_kind:?} does not accept {count} source registers"),
                    )]);
                }
                for _ in 0..count {
                    reads.push(read_u16(code, &mut cursor).map_err(|error| vec![error])?);
                }
                if reads.iter().copied().collect::<BTreeSet<_>>().len() != reads.len() {
                    return Err(vec![Diagnostic::new(
                        "register.effect_duplicate",
                        format!("$.instructions[{start}].register_effects.reads"),
                        "call source registers must be unique",
                    )]);
                }
                argument_count = Some((count - 1) as u32);
                writes.push(destination);
                "call"
            }
            OP_JUMP => {
                let target = read_u32(code, &mut cursor).map_err(|error| vec![error])?;
                operands.push(target);
                successors.push(target);
                "jump"
            }
            OP_BRANCH => {
                let condition = read_u16(code, &mut cursor).map_err(|error| vec![error])?;
                let then_target = read_u32(code, &mut cursor).map_err(|error| vec![error])?;
                let else_target = read_u32(code, &mut cursor).map_err(|error| vec![error])?;
                if then_target == else_target {
                    return Err(vec![Diagnostic::new(
                        "cfg.duplicate_successor",
                        format!("$.instructions[{start}]"),
                        "branch targets must differ; use a jump for equivalent successors",
                    )]);
                }
                reads.push(condition);
                operands.extend([then_target, else_target]);
                successors.extend([then_target, else_target]);
                "branch"
            }
            OP_RETURN => {
                let source = read_u16(code, &mut cursor).map_err(|error| vec![error])?;
                reads.push(source);
                "return"
            }
            OP_THROW => {
                let source = read_u16(code, &mut cursor).map_err(|error| vec![error])?;
                reads.push(source);
                if !may_throw {
                    return Err(vec![Diagnostic::new(
                        "exception.throw_flag",
                        format!("$.instructions[{start}]"),
                        "throw instruction must set may_throw",
                    )]);
                }
                "throw"
            }
            _ => {
                return Err(vec![Diagnostic::new(
                    "opcode.unsupported",
                    format!("$.instructions[{start}]"),
                    format!("unknown register opcode 0x{opcode:02x}"),
                )]);
            }
        };
        if requires_may_throw && !may_throw {
            return Err(vec![Diagnostic::new(
                "opcode.may_throw_underreported",
                format!("$.instructions[{start}].may_throw"),
                "instruction omits a required conservative may_throw effect",
            )]);
        }
        if cursor > code.len() || cursor - start > u32::MAX as usize {
            return Err(vec![Diagnostic::new(
                "instruction.bounds",
                format!("$.instructions[{start}]"),
                "instruction extends beyond code bounds",
            )]);
        }
        decoded.push(DecodedInstruction {
            offset: start as u32,
            size: (cursor - start) as u32,
            opcode: opcode as u32,
            operation,
            operands,
            constant_refs: constants,
            constant_kind,
            successors,
            may_throw,
            reads,
            writes,
            argument_count,
        });
    }
    if decoded.is_empty() {
        return Err(vec![Diagnostic::new(
            "program.instructions",
            "$.instructions",
            "decoded instruction stream must not be empty",
        )]);
    }
    for instruction in &mut decoded {
        let next = instruction.offset + instruction.size;
        if !matches!(
            instruction.operation,
            "jump" | "branch" | "return" | "throw"
        ) && next < code.len() as u32
        {
            instruction.successors = vec![next];
        }
    }

    let mut used = BTreeSet::new();
    used.extend(entry_registers.iter().copied());
    for instruction in &decoded {
        used.extend(instruction.reads.iter().copied());
        used.extend(instruction.writes.iter().copied());
    }
    let maximum = used.iter().next_back().copied().ok_or_else(|| {
        vec![Diagnostic::new(
            "register.frame_empty",
            "$.instructions",
            "register function does not use any virtual register",
        )]
    })? as u32;

    let facts = decoded
        .iter()
        .map(|item| {
            json!({
                "offset": item.offset,
                "size": item.size,
                "opcode": item.opcode,
                "operation": item.operation,
                "operands": item.operands,
                "constant_refs": item.constant_refs,
                "successors": item.successors,
                "may_throw": item.may_throw,
                "may_suspend": false,
                "stack_effect": Value::Null,
                "register_effects": {"reads": item.reads, "writes": item.writes},
                "argument_count": item.argument_count
            })
        })
        .collect::<Vec<_>>();
    let facts = json!({
        "schema": "ogma.bytecode-verification",
        "version": "1.0.0",
        "encoding_id": REGISTER_ENCODING_ID,
        "code_sha256": digest,
        "execution_model": "register",
        "entry": 0,
        "entry_registers": entry_registers,
        "instructions": facts
    });
    Ok((decoded, facts, maximum))
}

fn verify_register_program(
    instructions: &[DecodedInstruction],
    frame_count: u32,
    entry_registers: &[u16],
    constants: &[Value],
    code_size: usize,
) -> Result<(), Vec<Diagnostic>> {
    let constant_count = constants.len();
    let mut errors = Vec::new();
    let mut by_offset = BTreeMap::new();
    let mut previous_end = 0u32;
    for instruction in instructions {
        if instruction.offset != previous_end {
            errors.push(Diagnostic::new(
                "instruction.layout",
                format!("$.instructions[{}].offset", instruction.offset),
                "instructions must form a contiguous partition starting at zero",
            ));
        }
        previous_end = instruction.offset.saturating_add(instruction.size);
        if previous_end as usize > code_size {
            errors.push(Diagnostic::new(
                "instruction.bounds",
                format!("$.instructions[{}]", instruction.offset),
                "instruction exceeds code-section bounds",
            ));
        }
        if by_offset.insert(instruction.offset, instruction).is_some() {
            errors.push(Diagnostic::new(
                "instruction.duplicate_offset",
                format!("$.instructions[{}].offset", instruction.offset),
                "instruction offsets must be unique",
            ));
        }
        if instruction
            .reads
            .iter()
            .chain(instruction.writes.iter())
            .any(|register| *register as u32 >= frame_count)
        {
            errors.push(Diagnostic::new(
                "register.bounds",
                format!("$.instructions[{}].register_effects", instruction.offset),
                "register is outside the declared frame",
            ));
        }
        if instruction
            .reads
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .len()
            != instruction.reads.len()
            || instruction
                .writes
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .len()
                != instruction.writes.len()
            || instruction.writes.len() > 1
        {
            errors.push(Diagnostic::new(
                "register.effect_duplicate",
                format!("$.instructions[{}].register_effects", instruction.offset),
                "register reads must be unique and each instruction writes at most one register",
            ));
        }
        if let (Some(kind), Some(index)) =
            (instruction.constant_kind, instruction.constant_refs.first())
            && let Some(value) = constants.get(*index as usize)
            && !constant_value_matches(kind, value)
        {
            errors.push(Diagnostic::new(
                "constant.kind_mismatch",
                format!("$.instructions[{}].constant_refs", instruction.offset),
                format!("constant-pool value does not match instruction kind {kind:?}"),
            ));
        }
        if instruction
            .constant_refs
            .iter()
            .any(|index| *index as usize >= constant_count)
        {
            errors.push(Diagnostic::new(
                "constant.reference_bounds",
                format!("$.instructions[{}].constant_refs", instruction.offset),
                "constant-pool index is out of bounds",
            ));
        }
        let expected_constants = usize::from(instruction.operation == "constant");
        if instruction.constant_refs.len() != expected_constants {
            errors.push(Diagnostic::new(
                "constant.reference_arity",
                format!("$.instructions[{}].constant_refs", instruction.offset),
                "only constant instructions may reference one constant-pool entry",
            ));
        }
        let expected_effects = match instruction.operation {
            "constant" => Some((0, 1)),
            "unary" => Some((1, 1)),
            "binary" => Some((2, 1)),
            "call" => {
                let count = instruction.argument_count.unwrap_or(u32::MAX);
                if count > 1024 || instruction.reads.len() != count as usize + 1 {
                    errors.push(Diagnostic::new(
                        "operand.call_arity",
                        format!("$.instructions[{}].argument_count", instruction.offset),
                        "call argument count does not match the source-register list",
                    ));
                }
                Some((instruction.reads.len(), 1))
            }
            "jump" => Some((0, 0)),
            "branch" | "return" | "throw" => Some((1, 0)),
            _ => None,
        };
        if expected_effects
            .is_none_or(|expected| expected != (instruction.reads.len(), instruction.writes.len()))
        {
            errors.push(Diagnostic::new(
                "register.effect_mismatch",
                format!("$.instructions[{}].register_effects", instruction.offset),
                "register read/write effects do not match the operation contract",
            ));
        }
        let next = instruction.offset.saturating_add(instruction.size);
        match instruction.operation {
            "jump"
                if instruction.operands.len() != 1
                    || instruction.successors != instruction.operands =>
            {
                errors.push(Diagnostic::new(
                    "cfg.jump_target",
                    format!("$.instructions[{}]", instruction.offset),
                    "jump operand and successor must match",
                ));
            }
            "branch"
                if instruction.operands.len() != 2
                    || instruction.successors != instruction.operands =>
            {
                errors.push(Diagnostic::new(
                    "cfg.branch_targets",
                    format!("$.instructions[{}]", instruction.offset),
                    "branch operands and ordered successors must match",
                ));
            }
            "return" | "throw" if !instruction.successors.is_empty() => {
                errors.push(Diagnostic::new(
                    "cfg.terminal_successor",
                    format!("$.instructions[{}].successors", instruction.offset),
                    "terminal instructions must not have normal successors",
                ));
            }
            "jump" | "branch" | "return" | "throw" => {}
            _ if instruction.successors != [next] || next >= code_size as u32 => {
                errors.push(Diagnostic::new("cfg.fallthrough", format!("$.instructions[{}].successors", instruction.offset), "ordinary instructions must fall through to the immediately following instruction"));
            }
            _ => {}
        }
    }
    if previous_end as usize != code_size {
        errors.push(Diagnostic::new(
            "instruction.coverage",
            "$.instructions",
            "decoded instructions do not cover the code section exactly",
        ));
    }
    if entry_registers
        .iter()
        .any(|register| *register as u32 >= frame_count)
        || entry_registers
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .len()
            != entry_registers.len()
    {
        errors.push(Diagnostic::new(
            "register.entry_bounds",
            "$.entry_registers",
            "entry registers must be unique and fit the declared frame",
        ));
    }
    let boundaries: BTreeSet<u32> = by_offset.keys().copied().collect();
    let mut normal_edges = BTreeMap::<u32, Vec<u32>>::new();
    for instruction in instructions {
        if instruction
            .successors
            .iter()
            .any(|target| !boundaries.contains(target))
        {
            errors.push(Diagnostic::new(
                "cfg.target",
                format!("$.instructions[{}].successors", instruction.offset),
                "control-flow target is not an instruction boundary",
            ));
        }
        if instruction
            .successors
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .len()
            != instruction.successors.len()
        {
            errors.push(Diagnostic::new(
                "cfg.duplicate_successor",
                format!("$.instructions[{}].successors", instruction.offset),
                "successor targets must be unique",
            ));
        }
        if instruction.successors.contains(&0) {
            errors.push(Diagnostic::new(
                "cfg.entry_predecessor",
                "$.entry",
                "function entry cannot have a normal predecessor",
            ));
        }
        normal_edges.insert(instruction.offset, instruction.successors.clone());
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    let mut reachable = BTreeSet::new();
    let mut pending = vec![0u32];
    while let Some(offset) = pending.pop() {
        if !reachable.insert(offset) {
            continue;
        }
        pending.extend(normal_edges.get(&offset).into_iter().flatten().copied());
    }
    if reachable != boundaries {
        let missing = boundaries
            .difference(&reachable)
            .copied()
            .collect::<Vec<_>>();
        return Err(vec![Diagnostic::new(
            "cfg.unreachable",
            "$.instructions",
            format!("unreachable instructions: {missing:?}"),
        )]);
    }

    let initial: BTreeSet<u16> = entry_registers.iter().copied().collect();
    let mut incoming = BTreeMap::<u32, BTreeSet<u16>>::new();
    incoming.insert(0, initial);
    let mut queue = VecDeque::from([0u32]);
    let mut queued = BTreeSet::from([0u32]);
    let mut processed = BTreeMap::<u32, BTreeSet<u16>>::new();
    while let Some(source) = queue.pop_front() {
        queued.remove(&source);
        let Some(mut outgoing) = incoming.get(&source).cloned() else {
            continue;
        };
        outgoing.extend(by_offset[&source].writes.iter().copied());
        if processed.get(&source) == Some(&outgoing) {
            continue;
        }
        processed.insert(source, outgoing.clone());
        for target in normal_edges.get(&source).into_iter().flatten().copied() {
            let merged = match incoming.get(&target) {
                Some(existing) => existing.intersection(&outgoing).copied().collect(),
                None => outgoing.clone(),
            };
            if incoming.get(&target) != Some(&merged) {
                incoming.insert(target, merged);
                if queued.insert(target) {
                    queue.push_back(target);
                }
            }
        }
    }
    for instruction in instructions {
        let initialized = incoming.get(&instruction.offset);
        let missing = instruction
            .reads
            .iter()
            .filter(|register| !initialized.is_some_and(|set| set.contains(register)))
            .copied()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            errors.push(Diagnostic::new(
                "register.uninitialized",
                format!(
                    "$.instructions[{}].register_effects.reads",
                    instruction.offset
                ),
                format!("registers are read before definite initialization: {missing:?}"),
            ));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn read_container(raw: &[u8]) -> Result<ParsedContainer, Vec<Diagnostic>> {
    let mut errors = Vec::new();
    if raw.len() < HEADER_SIZE {
        return Err(vec![Diagnostic::new(
            "container.truncated_header",
            "$",
            "container is shorter than the 48-byte OGMB header",
        )]);
    }
    if raw.len() > MAX_FILE_BYTES {
        return Err(vec![Diagnostic::new(
            "resource.file_size",
            "$",
            "container exceeds the 64 MiB hard limit",
        )]);
    }
    if &raw[0..4] != b"OGMB"
        || read_u16_at(raw, 4) != Some(1)
        || read_u16_at(raw, 6) != Some(0)
        || read_u32_at(raw, 8) != Some(HEADER_SIZE as u32)
        || read_u64_at(raw, 12) != Some(0)
        || read_u64_at(raw, 20) != Some(raw.len() as u64)
        || read_u64_at(raw, 28) != Some(HEADER_SIZE as u64)
        || read_u32_at(raw, 40) != Some(DIRECTORY_ENTRY_SIZE as u32)
        || read_u32_at(raw, 44) != Some(0)
    {
        return Err(vec![Diagnostic::new(
            "container.header",
            "$.header",
            "invalid OGMB v1 header fields",
        )]);
    }
    let count = read_u32_at(raw, 36).unwrap_or(u32::MAX) as usize;
    if count > MAX_SECTIONS || HEADER_SIZE + count * DIRECTORY_ENTRY_SIZE > raw.len() {
        return Err(vec![Diagnostic::new(
            "container.directory",
            "$.directory",
            "section directory exceeds the v1 limit or file bounds",
        )]);
    }
    let directory_end = HEADER_SIZE + count * DIRECTORY_ENTRY_SIZE;
    let mut seen = BTreeSet::new();
    let mut intervals = Vec::<(usize, usize, u32)>::new();
    let mut sections = BTreeMap::<u32, Vec<u8>>::new();
    for index in 0..count {
        let base = HEADER_SIZE + index * DIRECTORY_ENTRY_SIZE;
        let section_type = read_u32_at(raw, base).unwrap_or(u32::MAX);
        let flags = read_u32_at(raw, base + 4).unwrap_or(u32::MAX);
        let offset = read_u64_at(raw, base + 8).unwrap_or(u64::MAX);
        let size = read_u64_at(raw, base + 16).unwrap_or(u64::MAX);
        let alignment = read_u32_at(raw, base + 24).unwrap_or_default();
        let reserved = read_u32_at(raw, base + 28).unwrap_or(u32::MAX);
        let path = format!("$.directory[{index}]");
        if !seen.insert(section_type) {
            errors.push(Diagnostic::new(
                "container.duplicate_section",
                format!("{path}.type"),
                "duplicate section type",
            ));
        }
        if flags & !1 != 0 || reserved != 0 {
            errors.push(Diagnostic::new(
                "container.section_flags",
                &path,
                "unknown section flags or non-zero reserved value",
            ));
        }
        if REQUIRED_SECTIONS.contains(&section_type) && flags & 1 != 0 {
            errors.push(Diagnostic::new(
                "container.required_section_optional",
                &path,
                "required sections cannot be optional",
            ));
        }
        if section_type == 5 && flags & 1 == 0 {
            errors.push(Diagnostic::new(
                "container.source_map_required",
                &path,
                "source_map must be optional in v1",
            ));
        }
        if alignment == 0
            || alignment > MAX_ALIGNMENT
            || !alignment.is_power_of_two()
            || !offset.is_multiple_of(alignment as u64)
        {
            errors.push(Diagnostic::new(
                "container.section_alignment",
                &path,
                "invalid section alignment",
            ));
        }
        let Some(end) = offset.checked_add(size) else {
            errors.push(Diagnostic::new(
                "container.section_bounds",
                &path,
                "section range overflow",
            ));
            continue;
        };
        if offset < directory_end as u64 || end > raw.len() as u64 {
            errors.push(Diagnostic::new(
                "container.section_bounds",
                &path,
                "section overlaps the directory or exceeds file bounds",
            ));
            continue;
        }
        if size == 0 && REQUIRED_SECTIONS.contains(&section_type) {
            errors.push(Diagnostic::new(
                "container.empty_required_section",
                &path,
                "required section is empty",
            ));
        }
        intervals.push((offset as usize, end as usize, section_type));
        if (1..=5).contains(&section_type) {
            sections.insert(section_type, raw[offset as usize..end as usize].to_vec());
        } else if flags & 1 == 0 {
            errors.push(Diagnostic::new(
                "container.unknown_required_section",
                &path,
                "unknown required section type",
            ));
        }
    }
    intervals.sort_by_key(|item| item.0);
    for pair in intervals.windows(2) {
        if pair[1].0 < pair[0].1 {
            errors.push(Diagnostic::new(
                "container.section_overlap",
                "$.directory",
                "section payloads overlap",
            ));
        }
    }
    for required in REQUIRED_SECTIONS {
        if !seen.contains(&required) {
            errors.push(Diagnostic::new(
                "container.missing_section",
                "$.directory",
                format!("missing required section {required}"),
            ));
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    let parse = |kind: u32, name: &str| -> Result<Value, Vec<Diagnostic>> {
        let bytes = sections.get(&kind).ok_or_else(|| {
            vec![Diagnostic::new(
                "container.missing_section",
                "$.directory",
                format!("missing {name} section"),
            )]
        })?;
        parse_json_strict(bytes).map_err(|error| {
            vec![Diagnostic::new(
                "container.section_json",
                format!("$.sections.{name}"),
                format!("invalid strict UTF-8 JSON: {error}"),
            )]
        })
    };
    let manifest = parse(1, "manifest")?;
    let constants_value = parse(3, "constants")?;
    let exceptions_value = parse(4, "exceptions")?;
    let source_map = sections
        .get(&5)
        .map(|bytes| parse_json_strict(bytes))
        .transpose()
        .map_err(|error| {
            vec![Diagnostic::new(
                "container.section_json",
                "$.sections.source_map",
                format!("invalid strict UTF-8 JSON: {error}"),
            )]
        })?;
    let code = sections.get(&2).cloned().ok_or_else(|| {
        vec![Diagnostic::new(
            "container.missing_section",
            "$.directory",
            "missing code section",
        )]
    })?;
    let constants = constants_value.as_array().cloned().ok_or_else(|| {
        vec![Diagnostic::new(
            "constant_pool.shape",
            "$.sections.constants",
            "constant pool must be a JSON array",
        )]
    })?;
    let exceptions = exceptions_value.as_array().cloned().ok_or_else(|| {
        vec![Diagnostic::new(
            "exception.shape",
            "$.sections.exceptions",
            "exception table must be a JSON array",
        )]
    })?;
    if constants.iter().any(|value| !is_json_scalar(value)) {
        return Err(vec![Diagnostic::new(
            "constant_pool.value",
            "$.sections.constants",
            "constant-pool values must be JSON scalars",
        )]);
    }
    if !manifest.is_object()
        || manifest.as_object().is_some_and(|object| {
            !has_exact_keys(
                object,
                &[
                    "format",
                    "version",
                    "encoding_id",
                    "code_sha256",
                    "resource_limits",
                    "metadata",
                ],
            )
        })
    {
        return Err(vec![Diagnostic::new(
            "manifest.fields",
            "$.manifest",
            "manifest fields do not match OGMB v1",
        )]);
    }
    if manifest.get("format").and_then(Value::as_str) != Some("ogma.ogmb")
        || manifest.get("version").and_then(Value::as_str) != Some("1.0")
    {
        return Err(vec![Diagnostic::new(
            "manifest.version",
            "$.manifest.version",
            "expected ogma.ogmb version 1.0",
        )]);
    }
    if source_map.as_ref().is_some_and(|map| !map.is_object()) {
        return Err(vec![Diagnostic::new(
            "source_map.shape",
            "$.source_map",
            "source map must be an object",
        )]);
    }
    if manifest.get("code_sha256").and_then(Value::as_str) != Some(sha256_hex(&code).as_str()) {
        return Err(vec![Diagnostic::new(
            "container.code_digest",
            "$.manifest.code_sha256",
            "manifest code digest does not match code section",
        )]);
    }
    Ok((manifest, code, constants, exceptions, source_map))
}

fn validate_metadata(metadata: &Value) -> Result<(), Vec<Diagnostic>> {
    let object = metadata.as_object().ok_or_else(|| {
        vec![Diagnostic::new(
            "metadata.shape",
            "$.manifest.metadata",
            "metadata must be an object",
        )]
    })?;
    if !has_exact_keys(
        object,
        &[
            "execution_model",
            "function",
            "code_generation",
            "code_cache",
            "frame_layout",
            "environment",
            "suspension_points",
            "inline_cache_slots",
            "verifier_summary",
        ],
    ) {
        return Err(vec![Diagnostic::new(
            "metadata.fields",
            "$.manifest.metadata",
            "metadata does not match the OGMB v1 field set",
        )]);
    }
    if metadata.get("execution_model").and_then(Value::as_str) != Some("register") {
        return Err(vec![Diagnostic::new(
            "metadata.execution_model",
            "$.manifest.metadata.execution_model",
            "this compiler profile requires execution_model=register",
        )]);
    }

    let function = metadata
        .get("function")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            vec![Diagnostic::new(
                "metadata.function",
                "$.manifest.metadata.function",
                "function identity must be an object",
            )]
        })?;
    if !has_exact_keys(function, &["id", "name"])
        || !valid_id(function.get("id").and_then(Value::as_str))
        || function
            .get("name")
            .and_then(Value::as_str)
            .is_none_or(|name| name.is_empty() || name.len() > 512)
    {
        return Err(vec![Diagnostic::new(
            "metadata.function",
            "$.manifest.metadata.function",
            "function identity requires a stable ID and non-empty name",
        )]);
    }

    for (key, identity_fields) in [
        ("code_generation", &["id", "version"][..]),
        ("code_cache", &["id", "version"][..]),
    ] {
        let identity = metadata
            .get(key)
            .and_then(Value::as_object)
            .ok_or_else(|| {
                vec![Diagnostic::new(
                    "metadata.identity",
                    format!("$.manifest.metadata.{key}"),
                    "identity must be an object",
                )]
            })?;
        if !has_exact_keys(identity, identity_fields)
            || !valid_id(identity.get("id").and_then(Value::as_str))
            || identity
                .get("version")
                .and_then(Value::as_str)
                .is_none_or(|version| version.is_empty() || version.len() > 128)
        {
            return Err(vec![Diagnostic::new(
                "metadata.identity",
                format!("$.manifest.metadata.{key}"),
                "identity requires a stable ID and non-empty version",
            )]);
        }
    }

    let frame = metadata
        .get("frame_layout")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            vec![Diagnostic::new(
                "metadata.frame_layout",
                "$.manifest.metadata.frame_layout",
                "frame_layout must be an object",
            )]
        })?;
    if !has_exact_keys(frame, &["parameter_count", "local_count", "register_count"]) {
        return Err(vec![Diagnostic::new(
            "metadata.frame_layout",
            "$.manifest.metadata.frame_layout",
            "frame_layout fields do not match OGMB v1",
        )]);
    }
    let parameter_count = read_u32_value(
        frame.get("parameter_count"),
        "$.manifest.metadata.frame_layout.parameter_count",
    )?;
    let local_count = read_u32_value(
        frame.get("local_count"),
        "$.manifest.metadata.frame_layout.local_count",
    )?;
    let register_count = read_u32_value(
        frame.get("register_count"),
        "$.manifest.metadata.frame_layout.register_count",
    )?;
    if register_count == 0
        || register_count > 65_536
        || parameter_count > register_count
        || local_count > 65_536
    {
        return Err(vec![Diagnostic::new(
            "metadata.frame_bounds",
            "$.manifest.metadata.frame_layout",
            "register_count must be 1..=65536 and parameter/local counts must fit the v1 frame limits",
        )]);
    }

    let environment = metadata
        .get("environment")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            vec![Diagnostic::new(
                "metadata.environment",
                "$.manifest.metadata.environment",
                "environment must be an object",
            )]
        })?;
    if !has_exact_keys(
        environment,
        &["id", "parent", "bindings", "captures", "locations"],
    ) {
        return Err(vec![Diagnostic::new(
            "metadata.environment",
            "$.manifest.metadata.environment",
            "environment fields do not match OGMB v1",
        )]);
    }
    let environment_id = environment
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !valid_id(Some(environment_id)) {
        return Err(vec![Diagnostic::new(
            "metadata.environment_id",
            "$.manifest.metadata.environment.id",
            "environment ID is invalid",
        )]);
    }
    if environment.get("parent").is_some_and(|parent| {
        !parent.is_null()
            && (parent.as_str().is_none_or(|parent| !valid_id(Some(parent)))
                || parent.as_str() == Some(environment_id))
    }) {
        return Err(vec![Diagnostic::new(
            "metadata.environment_parent",
            "$.manifest.metadata.environment.parent",
            "environment parent must be null or a different stable ID",
        )]);
    }

    let bindings = environment
        .get("bindings")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            vec![Diagnostic::new(
                "metadata.bindings_shape",
                "$.manifest.metadata.environment.bindings",
                "bindings must be an array",
            )]
        })?;
    let captures = environment
        .get("captures")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            vec![Diagnostic::new(
                "metadata.captures_shape",
                "$.manifest.metadata.environment.captures",
                "captures must be an array",
            )]
        })?;
    let locations = environment
        .get("locations")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            vec![Diagnostic::new(
                "metadata.locations_shape",
                "$.manifest.metadata.environment.locations",
                "locations must be an array",
            )]
        })?;
    if bindings.len() > 65_536 || captures.len() > 65_536 || locations.len() > 131_072 {
        return Err(vec![Diagnostic::new(
            "resource.binding_count",
            "$.manifest.metadata.environment",
            "binding, capture or location metadata exceeds the hard limit",
        )]);
    }

    let parameter_binding_count = bindings
        .iter()
        .filter(|binding| binding.get("kind").and_then(Value::as_str) == Some("parameter"))
        .count();
    let declared_parameter_count = read_u32_value(
        frame.get("parameter_count"),
        "$.manifest.metadata.frame_layout.parameter_count",
    )? as usize;
    if parameter_binding_count != declared_parameter_count {
        return Err(vec![Diagnostic::new(
            "metadata.parameter_binding_count",
            "$.manifest.metadata.frame_layout.parameter_count",
            format!(
                "parameter_count declares {declared_parameter_count} parameters but environment contains {parameter_binding_count} parameter bindings"
            ),
        )]);
    }

    let mut binding_ids = BTreeSet::<String>::new();
    for (index, binding) in bindings.iter().enumerate() {
        let path = format!("$.manifest.metadata.environment.bindings[{index}]");
        let fields = binding.as_object().ok_or_else(|| {
            vec![Diagnostic::new(
                "metadata.binding_fields",
                &path,
                "binding must be an object",
            )]
        })?;
        if !has_exact_keys(
            fields,
            &[
                "id",
                "name",
                "kind",
                "mutable",
                "semantic_type",
                "representation",
            ],
        ) {
            return Err(vec![Diagnostic::new(
                "metadata.binding_fields",
                &path,
                "binding fields do not match Core IR v1",
            )]);
        }
        let id = binding
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let name = binding
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let kind = binding
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mutable = binding.get("mutable").and_then(Value::as_bool);
        let semantic_type = binding
            .get("semantic_type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let representation = binding
            .get("representation")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let expected_mutability = matches!(kind, "var" | "let" | "parameter" | "catch");
        if !valid_id(Some(id))
            || !binding_ids.insert(id.to_owned())
            || name.is_empty()
            || name.len() > 512
            || !matches!(
                kind,
                "var" | "let" | "const" | "parameter" | "catch" | "import"
            )
            || mutable != Some(expected_mutability)
            || !valid_semantic_representation(semantic_type, representation)
        {
            return Err(vec![Diagnostic::new(
                "metadata.binding",
                &path,
                "binding identity, name, mutability, semantic type or representation is invalid",
            )]);
        }
    }

    let mut capture_ids = BTreeSet::<String>::new();
    let mut imported_bindings = BTreeSet::<String>::new();
    for (index, capture) in captures.iter().enumerate() {
        let path = format!("$.manifest.metadata.environment.captures[{index}]");
        let fields = capture.as_object().ok_or_else(|| {
            vec![Diagnostic::new(
                "metadata.capture_fields",
                &path,
                "capture must be an object",
            )]
        })?;
        if !has_exact_keys(
            fields,
            &[
                "id",
                "source_environment",
                "binding",
                "name",
                "mutable",
                "semantic_type",
                "representation",
                "mode",
            ],
        ) {
            return Err(vec![Diagnostic::new(
                "metadata.capture_fields",
                &path,
                "capture fields do not match Core IR v1",
            )]);
        }
        let id = capture
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let source_environment = capture
            .get("source_environment")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let binding = capture
            .get("binding")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let name = capture
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mutable = capture.get("mutable").and_then(Value::as_bool);
        let semantic_type = capture
            .get("semantic_type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let representation = capture
            .get("representation")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mode = capture
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !valid_id(Some(id))
            || binding_ids.contains(id)
            || !capture_ids.insert(id.to_owned())
            || !valid_id(Some(source_environment))
            || source_environment == environment_id
            || !valid_id(Some(binding))
            || !imported_bindings.insert(binding.to_owned())
            || name.is_empty()
            || name.len() > 512
            || mutable.is_none()
            || (mutable == Some(true) && mode != "cell")
            || (mutable == Some(false) && mode != "value")
            || !matches!(mode, "cell" | "value")
            || !valid_semantic_representation(semantic_type, representation)
        {
            return Err(vec![Diagnostic::new(
                "metadata.capture",
                &path,
                "capture identity, origin, mode, binding or representation is invalid",
            )]);
        }
    }

    if locations.len() != bindings.len() {
        return Err(vec![Diagnostic::new(
            "metadata.binding_location_coverage",
            "$.manifest.metadata.environment.locations",
            "every local lexical binding must have exactly one storage location",
        )]);
    }
    let mut located = BTreeSet::<String>::new();
    for (index, location) in locations.iter().enumerate() {
        let path = format!("$.manifest.metadata.environment.locations[{index}]");
        let fields = location.as_object().ok_or_else(|| {
            vec![Diagnostic::new(
                "metadata.binding_location",
                &path,
                "binding location must be an object",
            )]
        })?;
        if !has_exact_keys(fields, &["binding", "kind", "index"]) {
            return Err(vec![Diagnostic::new(
                "metadata.binding_location",
                &path,
                "binding location fields are invalid",
            )]);
        }
        let binding = location
            .get("binding")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let kind = location
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let slot = read_u32_value(location.get("index"), &format!("{path}.index"))?;
        let maximum = match kind {
            "register" => register_count,
            "local" => local_count,
            "environment" => bindings.len() as u32,
            "capture" => captures.len() as u32,
            _ => 0,
        };
        if !binding_ids.contains(binding) || !located.insert(binding.to_owned()) || slot >= maximum
        {
            return Err(vec![Diagnostic::new(
                "metadata.binding_location_bounds",
                &path,
                "binding location references an unknown binding or out-of-range storage slot",
            )]);
        }
    }
    if located != binding_ids {
        return Err(vec![Diagnostic::new(
            "metadata.binding_location_coverage",
            "$.manifest.metadata.environment.locations",
            "binding storage locations do not cover every local binding exactly once",
        )]);
    }

    let suspension_points = metadata
        .get("suspension_points")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            vec![Diagnostic::new(
                "metadata.suspension_points",
                "$.manifest.metadata.suspension_points",
                "suspension_points must be an array",
            )]
        })?;
    if !suspension_points.is_empty() {
        return Err(vec![Diagnostic::new(
            "metadata.suspension_unsupported",
            "$.manifest.metadata.suspension_points",
            "the synchronous M1 register profile does not emit suspension points",
        )]);
    }
    let inline_cache_slots = metadata
        .get("inline_cache_slots")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            vec![Diagnostic::new(
                "metadata.inline_cache_slots",
                "$.manifest.metadata.inline_cache_slots",
                "inline_cache_slots must be an array",
            )]
        })?;
    if !inline_cache_slots.is_empty() {
        return Err(vec![Diagnostic::new(
            "metadata.inline_cache_unsupported",
            "$.manifest.metadata.inline_cache_slots",
            "the M1 compiler does not emit inline-cache slots",
        )]);
    }

    let summary = metadata
        .get("verifier_summary")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            vec![Diagnostic::new(
                "metadata.verifier_summary",
                "$.manifest.metadata.verifier_summary",
                "verifier summary must be an object",
            )]
        })?;
    if !has_exact_keys(
        summary,
        &[
            "schema",
            "version",
            "code_sha256",
            "instruction_count",
            "max_stack_depth",
            "max_register_index",
            "exception_region_count",
            "constant_count",
        ],
    ) || summary.get("schema").and_then(Value::as_str)
        != Some("ogma.bytecode-verification-summary")
        || summary.get("version").and_then(Value::as_str) != Some("1.0.0")
    {
        return Err(vec![Diagnostic::new(
            "metadata.verifier_summary",
            "$.manifest.metadata.verifier_summary",
            "verifier summary fields or schema version do not match OGMB v1",
        )]);
    }
    for key in [
        "instruction_count",
        "max_stack_depth",
        "max_register_index",
        "exception_region_count",
        "constant_count",
    ] {
        read_u32_value(
            summary.get(key),
            &format!("$.manifest.metadata.verifier_summary.{key}"),
        )?;
    }
    if summary.get("max_stack_depth").and_then(Value::as_u64) != Some(0) {
        return Err(vec![Diagnostic::new(
            "metadata.verifier_summary_model",
            "$.manifest.metadata.verifier_summary.max_stack_depth",
            "register execution model requires max_stack_depth zero",
        )]);
    }
    Ok(())
}

fn valid_semantic_representation(semantic_type: &str, representation: &str) -> bool {
    match semantic_type {
        "dynamic" | "undefined" | "null" => representation == "boxed",
        "boolean" => matches!(representation, "boxed" | "i1"),
        "number" => matches!(representation, "boxed" | "i32" | "u32" | "f64"),
        "string" => matches!(representation, "boxed" | "string_ref"),
        "symbol" => matches!(representation, "boxed" | "symbol_ref"),
        "bigint" => matches!(representation, "boxed" | "bigint_ref"),
        "object" => matches!(representation, "boxed" | "object_ref"),
        "array" => matches!(representation, "boxed" | "array_ref" | "object_ref"),
        "function" => matches!(representation, "boxed" | "function_ref" | "object_ref"),
        "exception" => matches!(representation, "boxed" | "exception_ref"),
        _ => false,
    }
}

fn validate_resource_limits(limits: &Value) -> Result<(), Vec<Diagnostic>> {
    let object = limits.as_object().ok_or_else(|| {
        vec![Diagnostic::new(
            "resource.limits_shape",
            "$.manifest.resource_limits",
            "resource limits must be an object",
        )]
    })?;
    let names = [
        "max_code_bytes",
        "max_instructions",
        "max_constants",
        "max_stack",
        "max_registers",
        "max_locals",
        "max_call_depth",
        "max_exception_regions",
    ];
    if !has_exact_keys(object, &names) {
        return Err(vec![Diagnostic::new(
            "resource.limits_shape",
            "$.manifest.resource_limits",
            "resource limits must define the exact v1 fields",
        )]);
    }
    let max = [
        MAX_FILE_BYTES as u32,
        1_000_000,
        262_144,
        65_536,
        65_536,
        65_536,
        4_096,
        65_536,
    ];
    for (key, maximum) in names.iter().zip(max) {
        let value = read_u32_value(
            limits.get(*key),
            &format!("$.manifest.resource_limits.{key}"),
        )?;
        if value == 0 || value > maximum {
            return Err(vec![Diagnostic::new(
                "resource.limit_range",
                format!("$.manifest.resource_limits.{key}"),
                "resource limit is zero or exceeds the OGMB v1 hard limit",
            )]);
        }
    }
    Ok(())
}

fn validate_source_map(
    map: &Value,
    metadata: &Value,
    instructions: &[DecodedInstruction],
    code_size: usize,
) -> Result<(), Vec<Diagnostic>> {
    let object = map.as_object().ok_or_else(|| {
        vec![Diagnostic::new(
            "source_map.shape",
            "$.source_map",
            "source map must be an object",
        )]
    })?;
    if !has_exact_keys(
        object,
        &[
            "version",
            "files",
            "locations",
            "lexical_scopes",
            "variable_locations",
        ],
    ) || map.get("version").and_then(Value::as_str) != Some("1.0.0")
    {
        return Err(vec![Diagnostic::new(
            "source_map.fields",
            "$.source_map",
            "source map fields or version do not match OGMB v1",
        )]);
    }

    let boundaries: BTreeSet<u32> = instructions.iter().map(|item| item.offset).collect();
    let files = map.get("files").and_then(Value::as_array).ok_or_else(|| {
        vec![Diagnostic::new(
            "source_map.files",
            "$.source_map.files",
            "files must be an array",
        )]
    })?;
    if files.len() > 65_536 {
        return Err(vec![Diagnostic::new(
            "source_map.files",
            "$.source_map.files",
            "file count exceeds the v1 hard limit",
        )]);
    }
    let mut file_ids = BTreeSet::<String>::new();
    for (index, file) in files.iter().enumerate() {
        let path = format!("$.source_map.files[{index}]");
        let fields = file.as_object().ok_or_else(|| {
            vec![Diagnostic::new(
                "source_map.file",
                &path,
                "file entry must be an object",
            )]
        })?;
        if !has_exact_keys(fields, &["id", "path", "language"])
            && !has_exact_keys(fields, &["id", "path", "language", "sha256"])
        {
            return Err(vec![Diagnostic::new(
                "source_map.file_fields",
                &path,
                "file entry must define id, path, language and optional sha256",
            )]);
        }
        let id = file.get("id").and_then(Value::as_str).unwrap_or_default();
        let file_path = file.get("path").and_then(Value::as_str).unwrap_or_default();
        let language = file
            .get("language")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !valid_id(Some(id))
            || !file_ids.insert(id.to_owned())
            || file_path.is_empty()
            || file_path.len() > 4096
            || file_path.starts_with('/')
            || file_path.contains('\\')
            || file_path.contains(':')
            || file_path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || !matches!(language, "javascript" | "typescript" | "json" | "synthetic")
        {
            return Err(vec![Diagnostic::new(
                "source_map.file",
                &path,
                "source file ID, path or language is invalid",
            )]);
        }
        if let Some(sha) = file.get("sha256")
            && !sha.as_str().is_some_and(is_lower_sha256)
        {
            return Err(vec![Diagnostic::new(
                "source_map.file_digest",
                format!("{path}.sha256"),
                "source SHA-256 must be 64 lowercase hexadecimal characters",
            )]);
        }
    }

    let environment = metadata.get("environment").ok_or_else(|| {
        vec![Diagnostic::new(
            "metadata.environment",
            "$.manifest.metadata.environment",
            "environment metadata is required",
        )]
    })?;
    let local_bindings: BTreeSet<String> = environment
        .get("bindings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect();
    let capture_descriptors = environment
        .get("captures")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            vec![Diagnostic::new(
                "metadata.captures_shape",
                "$.manifest.metadata.environment.captures",
                "captures must be an array",
            )]
        })?;
    let mut valid_bindings = local_bindings;
    let mut expected_variable_locations = BTreeMap::<String, (String, u32)>::new();
    for location in environment
        .get("locations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let (Some(binding), Some(kind), Some(index)) = (
            location.get("binding").and_then(Value::as_str),
            location.get("kind").and_then(Value::as_str),
            location
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok()),
        ) {
            expected_variable_locations.insert(binding.to_owned(), (kind.to_owned(), index));
        }
    }
    for (index, capture) in capture_descriptors.iter().enumerate() {
        let id = capture
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !valid_id(Some(id)) || !valid_bindings.insert(id.to_owned()) {
            return Err(vec![Diagnostic::new(
                "source_map.capture_id",
                format!("$.manifest.metadata.environment.captures[{index}].id"),
                "capture ID is invalid or collides with another source binding",
            )]);
        }
        expected_variable_locations.insert(id.to_owned(), ("capture".to_owned(), index as u32));
    }

    let scopes = map
        .get("lexical_scopes")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            vec![Diagnostic::new(
                "source_map.lexical_scopes",
                "$.source_map.lexical_scopes",
                "lexical_scopes must be an array",
            )]
        })?;
    if scopes.len() > 65_536 {
        return Err(vec![Diagnostic::new(
            "source_map.lexical_scopes",
            "$.source_map.lexical_scopes",
            "scope count exceeds the v1 hard limit",
        )]);
    }
    let mut scope_ids = BTreeSet::<String>::new();
    for (index, scope) in scopes.iter().enumerate() {
        let path = format!("$.source_map.lexical_scopes[{index}]");
        let fields = scope.as_object().ok_or_else(|| {
            vec![Diagnostic::new(
                "source_map.scope_fields",
                &path,
                "lexical scope must be an object",
            )]
        })?;
        if !has_exact_keys(
            fields,
            &["id", "parent", "start_offset", "end_offset", "bindings"],
        ) {
            return Err(vec![Diagnostic::new(
                "source_map.scope_fields",
                &path,
                "lexical scope fields do not match v1",
            )]);
        }
        let id = scope.get("id").and_then(Value::as_str).unwrap_or_default();
        let start = read_u32_value(scope.get("start_offset"), &format!("{path}.start_offset"))?;
        let end = read_u32_value(scope.get("end_offset"), &format!("{path}.end_offset"))?;
        let bindings = scope
            .get("bindings")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                vec![Diagnostic::new(
                    "source_map.scope_bindings",
                    format!("{path}.bindings"),
                    "scope bindings must be an array",
                )]
            })?;
        let mut unique = BTreeSet::<String>::new();
        for binding in bindings {
            let binding = binding.as_str().unwrap_or_default();
            if !valid_bindings.contains(binding) || !unique.insert(binding.to_owned()) {
                return Err(vec![Diagnostic::new(
                    "source_map.scope_bindings",
                    format!("{path}.bindings"),
                    "scope binding is unknown or duplicated",
                )]);
            }
        }
        if !valid_id(Some(id))
            || !scope_ids.insert(id.to_owned())
            || start >= end
            || end as usize > code_size
            || !boundaries.contains(&start)
            || (end as usize != code_size && !boundaries.contains(&end))
        {
            return Err(vec![Diagnostic::new(
                "source_map.scope_range",
                &path,
                "scope identity or byte range is invalid",
            )]);
        }
    }
    let mut scope_parents = BTreeMap::<String, Option<String>>::new();
    for (index, scope) in scopes.iter().enumerate() {
        let path = format!("$.source_map.lexical_scopes[{index}].parent");
        let parent = scope.get("parent").ok_or_else(|| {
            vec![Diagnostic::new(
                "source_map.scope_parent",
                &path,
                "parent field is required",
            )]
        })?;
        let parent_id = parent.as_str();
        if !parent.is_null()
            && parent_id.is_none_or(|id| {
                !scope_ids.contains(id)
                    || id == scope.get("id").and_then(Value::as_str).unwrap_or_default()
            })
        {
            return Err(vec![Diagnostic::new(
                "source_map.scope_parent",
                &path,
                "scope parent must be null or another declared scope",
            )]);
        }
        let id = scope.get("id").and_then(Value::as_str).unwrap_or_default();
        scope_parents.insert(id.to_owned(), parent_id.map(str::to_owned));
    }

    // Lexical scope ancestry is a forest. Walk the single-parent graph iteratively
    // so deeply nested untrusted metadata cannot recurse on the native stack.
    let mut scope_state = BTreeMap::<String, u8>::new();
    for start in scope_parents.keys() {
        let mut path = Vec::<String>::new();
        let mut current = Some(start.as_str());
        while let Some(id) = current {
            match scope_state.get(id).copied() {
                Some(1) => {
                    return Err(vec![Diagnostic::new(
                        "source_map.scope_cycle",
                        "$.source_map.lexical_scopes",
                        format!("lexical scope parent chain contains a cycle at {id:?}"),
                    )]);
                }
                Some(2) => break,
                _ => {}
            }
            scope_state.insert(id.to_owned(), 1);
            path.push(id.to_owned());
            current = scope_parents.get(id).and_then(Option::as_deref);
        }
        for id in path {
            scope_state.insert(id, 2);
        }
    }

    let locations = map
        .get("locations")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            vec![Diagnostic::new(
                "source_map.locations",
                "$.source_map.locations",
                "locations must be an array",
            )]
        })?;
    if locations.len() != instructions.len() {
        return Err(vec![Diagnostic::new(
            "source_map.location_coverage",
            "$.source_map.locations",
            "source map must contain exactly one source location for every decoded instruction",
        )]);
    }
    let mut seen_offsets = BTreeSet::<u32>::new();
    for (index, location) in locations.iter().enumerate() {
        let path = format!("$.source_map.locations[{index}]");
        let fields = location.as_object().ok_or_else(|| {
            vec![Diagnostic::new(
                "source_map.location_fields",
                &path,
                "location must be an object",
            )]
        })?;
        if !has_exact_keys(fields, &["offset", "source"]) {
            return Err(vec![Diagnostic::new(
                "source_map.location_fields",
                &path,
                "location must contain offset and source only",
            )]);
        }
        let offset = read_u32_value(location.get("offset"), &format!("{path}.offset"))?;
        if !boundaries.contains(&offset) || !seen_offsets.insert(offset) {
            return Err(vec![Diagnostic::new(
                "source_map.instruction_boundary",
                format!("{path}.offset"),
                "source location offset must be a unique instruction boundary",
            )]);
        }
        let source = location
            .get("source")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                vec![Diagnostic::new(
                    "source_map.source",
                    format!("{path}.source"),
                    "source provenance must be an object",
                )]
            })?;
        let origin = source
            .get("origin")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let is_user = origin == "user";
        let expected_fields = if is_user {
            vec![
                "origin",
                "file",
                "start",
                "end",
                "scope",
                "bindings",
                "suspension",
                "inline_chain",
            ]
        } else if origin == "generated" {
            vec![
                "origin",
                "reason",
                "scope",
                "bindings",
                "suspension",
                "inline_chain",
            ]
        } else {
            return Err(vec![Diagnostic::new(
                "source_map.source",
                format!("{path}.source.origin"),
                "origin must be user or generated",
            )]);
        };
        let keys: Vec<&str> = source.keys().map(String::as_str).collect();
        if keys.len() != expected_fields.len()
            || expected_fields.iter().any(|key| !keys.contains(key))
        {
            return Err(vec![Diagnostic::new(
                "source_map.source_fields",
                format!("{path}.source"),
                "source provenance fields do not match the selected origin",
            )]);
        }
        if is_user {
            let file = source
                .get("file")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !file_ids.contains(file)
                || !valid_position(source.get("start"))
                || !valid_position(source.get("end"))
                || position_precedes(source.get("end"), source.get("start"))
            {
                return Err(vec![Diagnostic::new(
                    "source_map.user_source",
                    format!("{path}.source"),
                    "user provenance must reference a known file and ordered source positions",
                )]);
            }
        } else if source
            .get("reason")
            .and_then(Value::as_str)
            .is_none_or(|reason| reason.is_empty() || reason.len() > 512)
        {
            return Err(vec![Diagnostic::new(
                "source_map.generated_fields",
                format!("{path}.source.reason"),
                "generated source requires a reason of 1..512 characters",
            )]);
        }
        let scope = source.get("scope").ok_or_else(|| {
            vec![Diagnostic::new(
                "source_map.scope_ref",
                format!("{path}.source.scope"),
                "scope field is required",
            )]
        })?;
        if !scope.is_null() && scope.as_str().is_none_or(|id| !scope_ids.contains(id)) {
            return Err(vec![Diagnostic::new(
                "source_map.scope_ref",
                format!("{path}.source.scope"),
                "source references an unknown lexical scope",
            )]);
        }
        if source
            .get("suspension")
            .is_none_or(|value| !value.is_null())
        {
            return Err(vec![Diagnostic::new(
                "source_map.suspension_ref",
                format!("{path}.source.suspension"),
                "synchronous M1 source records must not reference suspension points",
            )]);
        }
        let source_bindings = source
            .get("bindings")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                vec![Diagnostic::new(
                    "source_map.binding_ref",
                    format!("{path}.source.bindings"),
                    "bindings must be an array",
                )]
            })?;
        if source_bindings.len() > 65_536 {
            return Err(vec![Diagnostic::new(
                "source_map.binding_ref",
                format!("{path}.source.bindings"),
                "binding list exceeds the v1 hard limit",
            )]);
        }
        let mut unique_bindings = BTreeSet::<String>::new();
        for binding in source_bindings {
            let id = binding.as_str().unwrap_or_default();
            if !valid_bindings.contains(id) || !unique_bindings.insert(id.to_owned()) {
                return Err(vec![Diagnostic::new(
                    "source_map.binding_ref",
                    format!("{path}.source.bindings"),
                    "source binding ID is unknown or duplicated",
                )]);
            }
        }
        let inline_chain = source
            .get("inline_chain")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                vec![Diagnostic::new(
                    "source_map.inline_chain",
                    format!("{path}.source.inline_chain"),
                    "inline_chain must be an array",
                )]
            })?;
        if inline_chain.len() > 64 {
            return Err(vec![Diagnostic::new(
                "source_map.inline_chain",
                format!("{path}.source.inline_chain"),
                "inline chain exceeds 64 frames",
            )]);
        }
        for (frame_index, frame) in inline_chain.iter().enumerate() {
            let frame_path = format!("{path}.source.inline_chain[{frame_index}]");
            let frame = frame.as_object().ok_or_else(|| {
                vec![Diagnostic::new(
                    "source_map.inline_frame",
                    &frame_path,
                    "inline frame must be an object",
                )]
            })?;
            if !has_exact_keys(frame, &["function", "file", "start", "end"])
                || !valid_id(frame.get("function").and_then(Value::as_str))
                || frame
                    .get("file")
                    .and_then(Value::as_str)
                    .is_none_or(|file| !file_ids.contains(file))
                || !valid_position(frame.get("start"))
                || !valid_position(frame.get("end"))
                || position_precedes(frame.get("end"), frame.get("start"))
            {
                return Err(vec![Diagnostic::new(
                    "source_map.inline_frame",
                    &frame_path,
                    "inline frame identity, file or source range is invalid",
                )]);
            }
        }
    }

    let variables = map
        .get("variable_locations")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            vec![Diagnostic::new(
                "source_map.variable_locations",
                "$.source_map.variable_locations",
                "variable_locations must be an array",
            )]
        })?;
    if variables.len() != expected_variable_locations.len() {
        return Err(vec![Diagnostic::new(
            "source_map.variable_coverage",
            "$.source_map.variable_locations",
            "variable locations must cover every local binding and capture exactly once",
        )]);
    }
    let mut seen_variables = BTreeSet::<String>::new();
    for (index, variable) in variables.iter().enumerate() {
        let path = format!("$.source_map.variable_locations[{index}]");
        let fields = variable.as_object().ok_or_else(|| {
            vec![Diagnostic::new(
                "source_map.variable_fields",
                &path,
                "variable location must be an object",
            )]
        })?;
        if !has_exact_keys(
            fields,
            &["binding", "kind", "index", "start_offset", "end_offset"],
        ) {
            return Err(vec![Diagnostic::new(
                "source_map.variable_fields",
                &path,
                "variable location fields do not match v1",
            )]);
        }
        let binding = variable
            .get("binding")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let kind = variable
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let slot = read_u32_value(variable.get("index"), &format!("{path}.index"))?;
        let start = read_u32_value(
            variable.get("start_offset"),
            &format!("{path}.start_offset"),
        )?;
        let end = read_u32_value(variable.get("end_offset"), &format!("{path}.end_offset"))?;
        let expected = expected_variable_locations.get(binding);
        if !seen_variables.insert(binding.to_owned())
            || expected.is_none_or(|(expected_kind, expected_slot)| {
                expected_kind.as_str() != kind || *expected_slot != slot
            })
            || start >= end
            || end as usize > code_size
            || !boundaries.contains(&start)
            || (end as usize != code_size && !boundaries.contains(&end))
        {
            return Err(vec![Diagnostic::new(
                "source_map.variable_storage",
                &path,
                "variable storage location differs from canonical frame metadata or has an invalid byte range",
            )]);
        }
    }
    Ok(())
}

fn is_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn position_precedes(left: Option<&Value>, right: Option<&Value>) -> bool {
    let left = left.and_then(Value::as_object);
    let right = right.and_then(Value::as_object);
    match (left, right) {
        (Some(left), Some(right)) => {
            let left_line = left.get("line").and_then(Value::as_u64).unwrap_or(0);
            let left_column = left.get("column").and_then(Value::as_u64).unwrap_or(0);
            let right_line = right.get("line").and_then(Value::as_u64).unwrap_or(0);
            let right_column = right.get("column").and_then(Value::as_u64).unwrap_or(0);
            (left_line, left_column) < (right_line, right_column)
        }
        _ => false,
    }
}

/// SHA-256 digest as lowercase hexadecimal without a new dependency.
pub fn sha256_hex(input: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut message = input.to_vec();
    let bit_length = (input.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_length.to_be_bytes());
    let mut state = [
        0x6a09e667u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];
    for block in message.as_chunks::<64>().0.iter() {
        let mut words = [0u32; 64];
        for (index, word) in words.iter_mut().take(16).enumerate() {
            let start = index * 4;
            *word = u32::from_be_bytes([
                block[start],
                block[start + 1],
                block[start + 2],
                block[start + 3],
            ]);
        }
        for index in 16..64 {
            let x = words[index - 15];
            let y = words[index - 2];
            let s0 = x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3);
            let s1 = y.rotate_right(17) ^ y.rotate_right(19) ^ (y >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(choose)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
    state.iter().map(|word| format!("{word:08x}")).collect()
}

fn read_u8(bytes: &[u8], cursor: &mut usize) -> Result<u8, Diagnostic> {
    let value = *bytes.get(*cursor).ok_or_else(|| {
        Diagnostic::new(
            "instruction.truncated",
            format!("$.code[{}]", *cursor),
            "truncated bytecode instruction",
        )
    })?;
    *cursor += 1;
    Ok(value)
}
fn read_u16(bytes: &[u8], cursor: &mut usize) -> Result<u16, Diagnostic> {
    let start = *cursor;
    let end = start.checked_add(2).ok_or_else(|| {
        Diagnostic::new("instruction.bounds", "$.code", "operand offset overflow")
    })?;
    let slice = bytes.get(start..end).ok_or_else(|| {
        Diagnostic::new(
            "instruction.truncated",
            format!("$.code[{start}]"),
            "truncated 16-bit operand",
        )
    })?;
    *cursor = end;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}
fn read_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32, Diagnostic> {
    let start = *cursor;
    let end = start.checked_add(4).ok_or_else(|| {
        Diagnostic::new("instruction.bounds", "$.code", "operand offset overflow")
    })?;
    let slice = bytes.get(start..end).ok_or_else(|| {
        Diagnostic::new(
            "instruction.truncated",
            format!("$.code[{start}]"),
            "truncated 32-bit operand",
        )
    })?;
    *cursor = end;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}
fn push_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}
fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}
fn write_u16_at(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}
fn write_u32_at(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn write_u64_at(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
fn read_u16_at(bytes: &[u8], offset: usize) -> Option<u16> {
    let s = bytes.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}
fn read_u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    let s = bytes.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
fn read_u64_at(bytes: &[u8], offset: usize) -> Option<u64> {
    let s = bytes.get(offset..offset.checked_add(8)?)?;
    Some(u64::from_le_bytes([
        s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
    ]))
}
fn align_up(value: usize, alignment: usize) -> Option<usize> {
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
}

struct StrictJsonValue(Value);

struct StrictJsonSeed {
    depth: usize,
}

impl<'de> serde::de::DeserializeSeed<'de> for StrictJsonSeed {
    type Value = StrictJsonValue;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if self.depth > 32 {
            return Err(de::Error::custom("JSON nesting exceeds hard limit of 32"));
        }
        deserializer.deserialize_any(StrictJsonVisitor { depth: self.depth })
    }
}

impl<'de> Deserialize<'de> for StrictJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        StrictJsonSeed { depth: 0 }.deserialize(deserializer)
    }
}

struct StrictJsonVisitor {
    depth: usize,
}

impl<'de> Visitor<'de> for StrictJsonVisitor {
    type Value = StrictJsonValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Number(Number::from(value))))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Number(Number::from(value))))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Number::from_f64(value)
            .map(|number| StrictJsonValue(Value::Number(number)))
            .ok_or_else(|| E::custom("non-finite JSON numbers are not permitted"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value.len() > MAX_JSON_STRING_BYTES {
            return Err(E::custom("JSON string exceeds the 64 MiB artifact limit"));
        }
        Ok(StrictJsonValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value.len() > MAX_JSON_STRING_BYTES {
            return Err(E::custom("JSON string exceeds the 64 MiB artifact limit"));
        }
        Ok(StrictJsonValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Null))
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        StrictJsonSeed {
            depth: self.depth + 1,
        }
        .deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(StrictJsonSeed {
            depth: self.depth + 1,
        })? {
            if values.len() >= MAX_JSON_ARRAY_ITEMS {
                return Err(de::Error::custom(
                    "JSON array exceeds hard item limit of one million",
                ));
            }
            values.push(value.0);
        }
        Ok(StrictJsonValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom(format!(
                    "duplicate JSON object key: {key}"
                )));
            }
            if values.len() >= 65_536 || key.len() > 256 {
                return Err(de::Error::custom(
                    "JSON object exceeds hard size or key-length limit",
                ));
            }
            let value = object.next_value_seed(StrictJsonSeed {
                depth: self.depth + 1,
            })?;
            values.insert(key, value.0);
        }
        Ok(StrictJsonValue(Value::Object(values)))
    }
}

fn parse_json_strict(bytes: &[u8]) -> Result<Value, serde_json::Error> {
    serde_json::from_slice::<StrictJsonValue>(bytes).map(|value| value.0)
}

fn is_json_scalar(value: &Value) -> bool {
    value.is_null() || value.is_boolean() || value.is_string() || value.is_number()
}

fn constant_value_matches(kind: ConstantKind, value: &Value) -> bool {
    match kind {
        ConstantKind::Undefined | ConstantKind::Null | ConstantKind::Uninitialized => {
            value.is_null()
        }
        ConstantKind::Boolean => value.is_boolean(),
        ConstantKind::Number => {
            let exact_integer = value
                .as_i64()
                .is_some_and(|integer| (integer as f64) as i128 == integer as i128)
                || value
                    .as_u64()
                    .is_some_and(|integer| (integer as f64) as u128 == integer as u128);
            exact_integer
                || (value.as_i64().is_none()
                    && value.as_u64().is_none()
                    && value.as_f64().is_some_and(f64::is_finite))
                || value
                    .as_str()
                    .is_some_and(|value| matches!(value, "NaN" | "Infinity" | "-Infinity" | "-0"))
        }
        ConstantKind::BigInt => value.as_str().is_some_and(is_canonical_bigint),
        ConstantKind::String => value.is_string(),
        ConstantKind::HostCapability
        | ConstantKind::Intrinsic
        | ConstantKind::FunctionId
        | ConstantKind::BindingId => value.as_str().is_some_and(|value| valid_id(Some(value))),
    }
}

fn is_canonical_bigint(value: &str) -> bool {
    if value == "0" {
        return true;
    }
    let digits = value.strip_prefix('-').unwrap_or(value);
    !digits.is_empty()
        && digits.as_bytes()[0].is_ascii_digit()
        && digits.as_bytes()[0] != b'0'
        && digits.as_bytes().iter().all(u8::is_ascii_digit)
}
fn valid_id(value: Option<&str>) -> bool {
    value.is_some_and(|id| {
        let bytes = id.as_bytes();
        !bytes.is_empty()
            && bytes.len() <= 128
            && (bytes[0].is_ascii_alphabetic() || bytes[0] == b'_')
            && bytes.iter().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'.' | b':' | b'-')
            })
    })
}
fn has_exact_keys(object: &Map<String, Value>, keys: &[&str]) -> bool {
    object.len() == keys.len() && keys.iter().all(|key| object.contains_key(*key))
}
fn read_u32_value(value: Option<&Value>, path: &str) -> Result<u32, Vec<Diagnostic>> {
    value
        .and_then(Value::as_u64)
        .and_then(|number| u32::try_from(number).ok())
        .ok_or_else(|| {
            vec![Diagnostic::new(
                "metadata.integer",
                path,
                "expected an unsigned 32-bit integer",
            )]
        })
}
fn valid_position(value: Option<&Value>) -> bool {
    value.and_then(Value::as_object).is_some_and(|position| {
        has_exact_keys(position, &["line", "column"])
            && position
                .get("line")
                .and_then(Value::as_u64)
                .is_some_and(|line| line > 0 && line <= u32::MAX as u64)
            && position
                .get("column")
                .and_then(Value::as_u64)
                .is_some_and(|column| column <= u32::MAX as u64)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(instruction_count: u32, constants: u32) -> Value {
        json!({
            "format":"ogma.ogmb", "version":"1.0", "encoding_id":REGISTER_ENCODING_ID,
            "code_sha256":"0".repeat(64),
            "resource_limits":{
                "max_code_bytes":65536,"max_instructions":1000,"max_constants":1000,
                "max_stack":4096,"max_registers":4096,"max_locals":4096,
                "max_call_depth":256,"max_exception_regions":1000
            },
            "metadata":{
                "execution_model":"register",
                "function":{"id":"test_fn","name":"test"},
                "code_generation":{"id":"ogma_register_codegen","version":"1.0.0"},
                "code_cache":{"id":"cache_test_fn","version":"1.0.0"},
                "frame_layout":{"parameter_count":0,"local_count":0,"register_count":1},
                "environment":{"id":"env_test_fn","parent":null,"bindings":[],"captures":[],"locations":[]},
                "suspension_points":[],"inline_cache_slots":[],
                "verifier_summary":{
                    "schema":"ogma.bytecode-verification-summary","version":"1.0.0",
                    "code_sha256":"0".repeat(64),"instruction_count":instruction_count,
                    "max_stack_depth":0,"max_register_index":0,
                    "exception_region_count":0,"constant_count":constants
                }
            }
        })
    }

    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn register_container_is_deterministic_and_verified_before_return() {
        let instructions = vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::Number,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Return { source: 0 },
                may_throw: false,
            },
        ];
        let code = encode_register_code(&instructions).expect("encode");
        let constants = vec![json!(42)];
        let left = build_ogmb_v1(manifest(2, 1), &code, &constants, &[], None).expect("container");
        let right = build_ogmb_v1(manifest(2, 1), &code, &constants, &[], None).expect("container");
        assert_eq!(left, right);
        let artifact = verify_register_artifact(&left, &[]).expect("verified artifact");
        assert_eq!(artifact.code, code);
        assert_eq!(artifact.constants, constants);
        assert_eq!(
            artifact.verification_input["instructions"][0]["register_effects"]["writes"],
            json!([0])
        );
    }

    #[test]
    fn rejects_manifest_with_unknown_metadata_fields() {
        let instructions = vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::Number,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Return { source: 0 },
                may_throw: false,
            },
        ];
        let code = encode_register_code(&instructions).expect("encode");
        let mut manifest = manifest(2, 1);
        manifest["metadata"]["unversioned_extension"] = json!({"enabled":true});
        let bytes = build_ogmb_v1(manifest, &code, &[json!(1)], &[], None).expect("build");
        let errors = verify_register_artifact(&bytes, &[])
            .expect_err("metadata must match the closed v1 field set");
        assert!(errors.iter().any(|item| item.code == "metadata.fields"));
    }

    #[test]
    fn requires_entry_registers_for_every_declared_parameter() {
        let instructions = vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::Number,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Return { source: 0 },
                may_throw: false,
            },
        ];
        let code = encode_register_code(&instructions).expect("encode");
        let mut artifact_manifest = manifest(2, 1);
        artifact_manifest["metadata"]["frame_layout"]["parameter_count"] = json!(1);
        artifact_manifest["metadata"]["environment"]["bindings"] = json!([{
            "id":"parameter_0","name":"value","kind":"parameter","mutable":true,
            "semantic_type":"number","representation":"boxed"
        }]);
        artifact_manifest["metadata"]["environment"]["locations"] = json!([{
            "binding":"parameter_0","kind":"register","index":0
        }]);
        let bytes = build_ogmb_v1(artifact_manifest, &code, &[json!(1)], &[], None).expect("build");
        let errors = verify_register_artifact(&bytes, &[])
            .expect_err("parameter entry register is mandatory");
        assert!(
            errors
                .iter()
                .any(|item| item.code == "register.entry_arity")
        );
    }

    #[test]
    fn rejects_manifest_that_claims_a_different_execution_model() {
        let instructions = vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::Number,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Return { source: 0 },
                may_throw: false,
            },
        ];
        let code = encode_register_code(&instructions).expect("encode");
        let mut wrong_model = manifest(2, 1);
        wrong_model["metadata"]["execution_model"] = json!("stack");
        let bytes = build_ogmb_v1(wrong_model, &code, &[json!(1)], &[], None).expect("build");
        let errors =
            verify_register_artifact(&bytes, &[]).expect_err("stack manifest must be rejected");
        assert!(
            errors
                .iter()
                .any(|item| item.code == "metadata.execution_model")
        );
    }

    #[test]
    fn rejects_manifest_with_unknown_registered_decoder() {
        let instructions = vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::Number,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Return { source: 0 },
                may_throw: false,
            },
        ];
        let code = encode_register_code(&instructions).expect("encode");
        let mut unknown = manifest(2, 1);
        unknown["encoding_id"] = json!("ogma.unknown.m9.v1");
        let bytes = build_ogmb_v1(unknown, &code, &[json!(1)], &[], None).expect("build");
        let errors =
            verify_register_artifact(&bytes, &[]).expect_err("unknown encoding must fail closed");
        assert!(
            errors
                .iter()
                .any(|item| item.code == "manifest.encoding_id")
        );
    }

    #[test]
    fn rejects_instruction_stream_with_unknown_opcode() {
        let code = vec![0xff, 0x00];
        let bytes = build_ogmb_v1(manifest(1, 0), &code, &[], &[], None).expect("build");
        let errors =
            verify_register_artifact(&bytes, &[]).expect_err("unknown opcode must fail closed");
        assert!(errors.iter().any(|item| item.code == "opcode.unsupported"));
    }

    #[test]
    fn rejects_instruction_stream_with_out_of_range_constant_reference() {
        let instructions = vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 99,
                    kind: ConstantKind::Number,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Return { source: 0 },
                may_throw: false,
            },
        ];
        let code = encode_register_code(&instructions).expect("encode");
        let bytes = build_ogmb_v1(manifest(2, 1), &code, &[json!(1)], &[], None).expect("build");
        let errors =
            verify_register_artifact(&bytes, &[]).expect_err("bad constant reference must fail");
        assert!(
            errors
                .iter()
                .any(|item| item.code == "constant.reference_bounds")
        );
    }

    #[test]
    fn decodes_local_binding_store_as_an_explicit_call_kind() {
        let instructions = vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::BindingId,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 1,
                    constant: 1,
                    kind: ConstantKind::Number,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Call {
                    destination: 2,
                    kind: CallKind::StoreBinding,
                    sources: vec![0, 1],
                },
                may_throw: true,
            },
            RegisterInstruction {
                operation: RegisterOperation::Return { source: 2 },
                may_throw: false,
            },
        ];
        let code = encode_register_code(&instructions).expect("encode local binding store");
        let constants = vec![json!("local_count"), json!(42)];
        let mut metadata = manifest(4, 2);
        metadata["metadata"]["frame_layout"]["register_count"] = json!(3);
        metadata["metadata"]["verifier_summary"]["max_register_index"] = json!(2);
        let bytes = build_ogmb_v1(metadata, &code, &constants, &[], None).expect("build");
        let artifact = verify_register_artifact(&bytes, &[])
            .expect("local binding store is structurally valid");
        assert_eq!(
            artifact.verification_input["instructions"][2]["operation"],
            "call"
        );
        assert_eq!(
            artifact.verification_input["instructions"][2]["argument_count"],
            json!(1)
        );
        assert_eq!(
            artifact.verification_input["instructions"][2]["register_effects"]["reads"],
            json!([0, 1])
        );
    }

    #[test]
    fn rejects_underreported_throw_effects() {
        let instructions = vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::Number,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Unary {
                    destination: 1,
                    source: 0,
                    operation: UnaryOperation::ToNumber,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Return { source: 1 },
                may_throw: false,
            },
        ];
        let code = encode_register_code(&instructions).expect("encode");
        let mut metadata = manifest(3, 1);
        metadata["metadata"]["frame_layout"]["register_count"] = json!(2);
        metadata["metadata"]["verifier_summary"]["max_register_index"] = json!(1);
        let bytes = build_ogmb_v1(metadata, &code, &[json!(42)], &[], None).expect("build");
        let errors = verify_register_artifact(&bytes, &[])
            .expect_err("potentially throwing conversion must retain may_throw");
        assert!(
            errors
                .iter()
                .any(|item| item.code == "opcode.may_throw_underreported")
        );
    }

    #[test]
    fn enforces_call_kind_specific_source_arity_in_encoder_and_decoder() {
        let invalid = [
            (CallKind::JavaScript, 1usize),
            (CallKind::Construct, 1),
            (CallKind::GetProperty, 1),
            (CallKind::SetProperty, 2),
            (CallKind::GetElement, 1),
            (CallKind::SetElement, 2),
            (CallKind::LoadCapture, 2),
            (CallKind::StoreCapture, 1),
            (CallKind::MakeCell, 2),
            (CallKind::StoreBinding, 1),
        ];
        for (kind, count) in invalid {
            let sources = (0..count).map(|index| index as u16).collect();
            let errors = encode_register_code(&[RegisterInstruction {
                operation: RegisterOperation::Call {
                    destination: 0,
                    kind,
                    sources,
                },
                may_throw: kind != CallKind::MakeClosure,
            }])
            .expect_err("kind-specific source arity must be rejected");
            assert!(errors.iter().any(|item| item.code == "call.kind_arity"));
        }

        let mut code = vec![OP_CALL, 1];
        push_u16(&mut code, 0);
        code.push(CallKind::GetProperty as u8);
        push_u16(&mut code, 1);
        push_u16(&mut code, 0);
        let bytes = build_ogmb_v1(manifest(1, 0), &code, &[], &[], None)
            .expect("build malformed container");
        let errors = verify_register_artifact(&bytes, &[])
            .expect_err("decoder must reject an invalid operation shape");
        assert!(errors.iter().any(|item| item.code == "call.kind_arity"));
    }

    #[test]
    fn rejects_integer_number_constants_not_exactly_representable_as_binary64() {
        let instructions = vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::Number,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Return { source: 0 },
                may_throw: false,
            },
        ];
        let code = encode_register_code(&instructions).expect("encode");
        let bytes = build_ogmb_v1(
            manifest(2, 1),
            &code,
            &[json!(9_007_199_254_740_993u64)],
            &[],
            None,
        )
        .expect("build");
        let errors = verify_register_artifact(&bytes, &[])
            .expect_err("inexact integer Number payload must fail");
        assert!(
            errors
                .iter()
                .any(|item| item.code == "constant.kind_mismatch")
        );
    }

    #[test]
    fn rejects_cyclic_source_map_lexical_scope_hierarchies() {
        let instructions = vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::Number,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Return { source: 0 },
                may_throw: false,
            },
        ];
        let code = encode_register_code(&instructions).expect("encode");
        let source = |scope: &str| {
            json!({
                "origin":"generated",
                "reason":"lexical scope cycle regression",
                "scope":scope,
                "bindings":[],
                "suspension":null,
                "inline_chain":[]
            })
        };
        let source_map = json!({
            "version":"1.0.0",
            "files":[],
            "locations":[
                {"offset":0,"source":source("scope_a")},
                {"offset":9,"source":source("scope_b")}
            ],
            "lexical_scopes":[
                {"id":"scope_a","parent":"scope_b","start_offset":0,"end_offset":13,"bindings":[]},
                {"id":"scope_b","parent":"scope_a","start_offset":0,"end_offset":13,"bindings":[]}
            ],
            "variable_locations":[]
        });
        let bytes = build_ogmb_v1(manifest(2, 1), &code, &[json!(1)], &[], Some(&source_map))
            .expect("build malformed source map");
        let errors = verify_register_artifact(&bytes, &[])
            .expect_err("cyclic lexical scope ancestry must fail closed");
        assert!(
            errors
                .iter()
                .any(|item| item.code == "source_map.scope_cycle")
        );
    }

    #[test]
    fn rejects_noncanonical_bigint_and_identifier_constants() {
        for (kind, value) in [
            (ConstantKind::BigInt, json!("01")),
            (ConstantKind::BigInt, json!("-0")),
            (ConstantKind::FunctionId, json!("not an id")),
            (ConstantKind::BindingId, json!("bad id")),
        ] {
            let instructions = vec![
                RegisterInstruction {
                    operation: RegisterOperation::Constant {
                        destination: 0,
                        constant: 0,
                        kind,
                    },
                    may_throw: false,
                },
                RegisterInstruction {
                    operation: RegisterOperation::Return { source: 0 },
                    may_throw: false,
                },
            ];
            let code = encode_register_code(&instructions).expect("encode");
            let bytes = build_ogmb_v1(manifest(2, 1), &code, &[value], &[], None)
                .expect("build malformed constant");
            let errors = verify_register_artifact(&bytes, &[])
                .expect_err("invalid constant payload must be rejected");
            assert!(
                errors
                    .iter()
                    .any(|item| item.code == "constant.kind_mismatch")
            );
        }
    }

    #[test]
    fn rejects_noncanonical_entry_register_mapping() {
        let instructions = vec![RegisterInstruction {
            operation: RegisterOperation::Return { source: 1 },
            may_throw: false,
        }];
        let code = encode_register_code(&instructions).expect("encode");
        let mut artifact_manifest = manifest(1, 0);
        artifact_manifest["metadata"]["frame_layout"]["parameter_count"] = json!(1);
        artifact_manifest["metadata"]["frame_layout"]["register_count"] = json!(2);
        artifact_manifest["metadata"]["verifier_summary"]["max_register_index"] = json!(1);
        artifact_manifest["metadata"]["environment"]["bindings"] = json!([{
            "id":"parameter_0","name":"value","kind":"parameter","mutable":true,
            "semantic_type":"number","representation":"boxed"
        }]);
        artifact_manifest["metadata"]["environment"]["locations"] = json!([{
            "binding":"parameter_0","kind":"register","index":0
        }]);
        let bytes = build_ogmb_v1(artifact_manifest, &code, &[], &[], None)
            .expect("build malformed entry layout");
        let errors = verify_register_artifact(&bytes, &[1])
            .expect_err("entry registers must follow canonical M1 prefix layout");
        assert!(
            errors
                .iter()
                .any(|item| item.code == "register.entry_layout")
        );
    }

    #[test]
    fn rejects_local_frame_size_above_declared_resource_limit() {
        let instructions = vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::Number,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Return { source: 0 },
                may_throw: false,
            },
        ];
        let code = encode_register_code(&instructions).expect("encode");
        let mut artifact_manifest = manifest(2, 1);
        artifact_manifest["metadata"]["frame_layout"]["local_count"] = json!(2);
        artifact_manifest["resource_limits"]["max_locals"] = json!(1);
        let bytes = build_ogmb_v1(artifact_manifest, &code, &[json!(1)], &[], None).expect("build");
        let errors = verify_register_artifact(&bytes, &[])
            .expect_err("frame local_count must not exceed max_locals");
        assert!(
            errors
                .iter()
                .any(|item| item.code == "resource.local_limit")
        );
    }

    #[test]
    fn rejects_register_operand_outside_the_declared_frame() {
        let instructions = vec![RegisterInstruction {
            operation: RegisterOperation::Return { source: 1 },
            may_throw: false,
        }];
        let code = encode_register_code(&instructions).expect("encode");
        let bytes = build_ogmb_v1(manifest(1, 0), &code, &[], &[], None).expect("build");
        let errors = verify_register_artifact(&bytes, &[])
            .expect_err("register index must fit the function frame");
        assert!(errors.iter().any(|item| item.code == "register.bounds"));
    }

    #[test]
    fn rejects_read_before_definite_initialization() {
        let instructions = vec![RegisterInstruction {
            operation: RegisterOperation::Return { source: 0 },
            may_throw: false,
        }];
        let code = encode_register_code(&instructions).expect("encode");
        let bytes = build_ogmb_v1(manifest(1, 0), &code, &[], &[], None).expect("container");
        let errors =
            verify_register_artifact(&bytes, &[]).expect_err("must reject uninitialized register");
        assert!(
            errors
                .iter()
                .any(|error| error.code == "register.uninitialized")
        );
    }

    #[test]
    fn header_fields_match_the_documented_48_byte_layout() {
        let instructions = vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::Number,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Return { source: 0 },
                may_throw: false,
            },
        ];
        let code = encode_register_code(&instructions).expect("encode");
        let bytes = build_ogmb_v1(manifest(2, 1), &code, &[json!(42)], &[], None).expect("build");
        assert_eq!(read_u64_at(&bytes, 12), Some(0));
        assert_eq!(read_u64_at(&bytes, 20), Some(bytes.len() as u64));
        assert_eq!(read_u64_at(&bytes, 28), Some(HEADER_SIZE as u64));
        assert_eq!(read_u32_at(&bytes, 36), Some(4));
        assert_eq!(read_u32_at(&bytes, 40), Some(DIRECTORY_ENTRY_SIZE as u32));
        assert_eq!(read_u32_at(&bytes, 44), Some(0));
        assert!(read_container(&bytes).is_ok());
    }

    #[test]
    fn strict_json_rejects_duplicate_keys_and_excessive_nesting() {
        assert!(parse_json_strict(br#"{"format":"ogma.ogmb","format":"other"}"#).is_err());
        let nested = format!("{}0{}", "[".repeat(34), "]".repeat(34));
        assert!(parse_json_strict(nested.as_bytes()).is_err());
    }

    #[test]
    fn rejects_tampered_code_after_container_build() {
        let instructions = vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::Number,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Return { source: 0 },
                may_throw: false,
            },
        ];
        let code = encode_register_code(&instructions).expect("encode");
        let mut bytes =
            build_ogmb_v1(manifest(2, 1), &code, &[json!(42)], &[], None).expect("build container");
        let code_section = read_container(&bytes).expect("read valid container").1;
        let needle = code_section.as_slice();
        let position = bytes
            .windows(needle.len())
            .position(|window| window == needle)
            .expect("code payload is present");
        bytes[position] ^= 0x01;
        let errors = verify_register_artifact(&bytes, &[])
            .expect_err("tampered code must not pass the manifest digest check");
        assert!(
            errors
                .iter()
                .any(|error| error.code == "container.code_digest")
        );
    }

    #[test]
    fn rejects_control_flow_targets_that_are_not_instruction_boundaries() {
        let instructions = vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::Boolean,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Branch {
                    condition: 0,
                    then_target: 9,
                    else_target: 99,
                },
                may_throw: false,
            },
        ];
        let code = encode_register_code(&instructions).expect("encoder preserves explicit targets");
        let bytes = build_ogmb_v1(manifest(2, 1), &code, &[json!(true)], &[], None)
            .expect("build container");
        let errors = verify_register_artifact(&bytes, &[])
            .expect_err("branch targets must resolve to instruction boundaries");
        assert!(errors.iter().any(|error| error.code == "cfg.target"));
    }

    #[test]
    fn rejects_oversized_call_before_computing_layout() {
        let instructions = vec![RegisterInstruction {
            operation: RegisterOperation::Call {
                destination: 0,
                kind: CallKind::JavaScript,
                sources: vec![1; 1026],
            },
            may_throw: true,
        }];
        let errors = encode_register_code(&instructions)
            .expect_err("oversized call must be rejected before layout");
        assert!(errors.iter().any(|item| item.code == "operand.call_arity"));
    }

    #[test]
    fn rejects_constant_kind_that_does_not_match_its_pool_value() {
        let instructions = vec![
            RegisterInstruction {
                operation: RegisterOperation::Constant {
                    destination: 0,
                    constant: 0,
                    kind: ConstantKind::Number,
                },
                may_throw: false,
            },
            RegisterInstruction {
                operation: RegisterOperation::Return { source: 0 },
                may_throw: false,
            },
        ];
        let code = encode_register_code(&instructions).expect("encode");
        let bytes = build_ogmb_v1(manifest(2, 1), &code, &[json!("not a number")], &[], None)
            .expect("build");
        let errors = verify_register_artifact(&bytes, &[])
            .expect_err("constant kind and pool value must agree");
        assert!(
            errors
                .iter()
                .any(|item| item.code == "constant.kind_mismatch")
        );
    }

    #[test]
    fn rejects_non_scalar_constant_pool_values() {
        let error = build_ogmb_v1(
            manifest(1, 1),
            &[OP_RETURN, 0, 0, 0],
            &[json!({"nested":true})],
            &[],
            None,
        )
        .expect_err("constant pool only supports JSON scalars");
        assert!(error.iter().any(|item| item.code == "constant_pool.shape"));
    }
}
