//! JavaScript source frontend boundary.
//!
//! OGM-41 supplies the JavaScript parser, source identity/position mapping and
//! deterministic diagnostics. Semantic resolution and Core IR lowering stay
//! in their own frontend/compiler boundaries. This crate never depends on the
//! Ogma Runtime.
//!
//! The Oxc AST borrows from the caller-provided compile-unit arena. A parsed
//! tree cannot outlive either the source file or its allocator.

#![deny(missing_docs)]

use std::{fmt, sync::Arc};

use oxc_allocator::Allocator;
use oxc_ast::ast::Program;
use oxc_diagnostics::{OxcDiagnostic, Severity as OxcSeverity};
use oxc_parser::{ParseOptions as OxcParseOptions, Parser};
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;

/// Stable identity for one source file within a compilation/debugging session.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SourceId(Arc<str>);

impl SourceId {
    /// Creates a source identity from a stable caller-provided identifier.
    pub fn new(value: impl Into<Arc<str>>) -> Self {
        Self(value.into())
    }

    /// Returns the stable identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A one-based source line and zero-based UTF-16 code-unit column.
///
/// Byte offsets are stored separately in SourceSpan. UTF-16 columns match
/// the coordinate convention used by JavaScript tooling and LSP clients.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourcePosition {
    /// One-based source line.
    pub line: u32,
    /// Zero-based UTF-16 code-unit column.
    pub column_utf16: u32,
}

/// A source range retaining byte offsets and editor-facing coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceSpan {
    /// Inclusive UTF-8 byte offset.
    pub start_byte: u32,
    /// Exclusive UTF-8 byte offset.
    pub end_byte: u32,
    /// Start position (one-based line, zero-based UTF-16 column).
    pub start: SourcePosition,
    /// End position (one-based line, zero-based UTF-16 column).
    pub end: SourcePosition,
}

impl SourceSpan {
    /// Returns the span length in UTF-8 bytes.
    pub fn byte_len(self) -> u32 {
        self.end_byte.saturating_sub(self.start_byte)
    }

    /// Returns true when this span is empty.
    pub fn is_empty(self) -> bool {
        self.start_byte == self.end_byte
    }
}

/// Owned source text plus its stable identity and line index.
///
/// Construct one SourceFile per immutable source revision and keep it alive
/// for the entire lifetime of the AST and any diagnostics derived from it.
#[derive(Clone, Debug)]
pub struct SourceFile {
    id: SourceId,
    uri: Arc<str>,
    revision: Option<Arc<str>>,
    text: Arc<str>,
    content_sha256: String,
    line_starts: Vec<u32>,
}

impl SourceFile {
    /// Creates a source file and validates that its byte offsets fit Oxc's
    /// 32-bit span representation.
    pub fn new(
        id: impl Into<Arc<str>>,
        uri: impl Into<Arc<str>>,
        revision: Option<Arc<str>>,
        text: impl Into<Arc<str>>,
    ) -> Result<Self, SourceFileError> {
        let text = text.into();
        if text.len() > u32::MAX as usize {
            return Err(SourceFileError::TooLarge {
                byte_len: text.len() as u64,
            });
        }
        let line_starts = build_line_starts(&text);
        let content_sha256 = sha256_hex(text.as_bytes());
        Ok(Self {
            id: SourceId::new(id),
            uri: uri.into(),
            revision,
            text,
            content_sha256,
            line_starts,
        })
    }

    /// Returns the stable source identity.
    pub fn id(&self) -> &SourceId {
        &self.id
    }

    /// Returns the source URI/path used for diagnostics and debugging.
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Returns an optional caller-supplied source revision.
    pub fn revision(&self) -> Option<&str> {
        self.revision.as_deref()
    }

    /// Returns the immutable UTF-8 source text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the SHA-256 digest of the exact source bytes.
    pub fn content_sha256(&self) -> &str {
        &self.content_sha256
    }

    /// Converts a byte offset to a one-based line and zero-based UTF-16 column.
    ///
    /// Offsets outside the source or inside a UTF-8 code point are clamped to
    /// the nearest preceding valid character boundary.
    pub fn position(&self, byte_offset: usize) -> SourcePosition {
        let offset = floor_char_boundary(&self.text, byte_offset.min(self.text.len()));
        let line_index = match self.line_starts.binary_search(&(offset as u32)) {
            Ok(index) => index,
            Err(index) => index.saturating_sub(1),
        };
        let line_start = self.line_starts[line_index] as usize;
        let column_utf16 = self.text[line_start..offset].encode_utf16().count() as u32;
        SourcePosition {
            line: line_index as u32 + 1,
            column_utf16,
        }
    }

    /// Creates a clamped source span from UTF-8 byte offsets.
    pub fn span(&self, start_byte: usize, end_byte: usize) -> SourceSpan {
        let start = floor_char_boundary(&self.text, start_byte.min(self.text.len()));
        let mut end = floor_char_boundary(&self.text, end_byte.min(self.text.len()));
        if end < start {
            end = start;
        }
        SourceSpan {
            start_byte: start as u32,
            end_byte: end as u32,
            start: self.position(start),
            end: self.position(end),
        }
    }
}

/// Failure to construct source metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceFileError {
    /// Source exceeds the 32-bit byte-span limit used by the parser.
    TooLarge {
        /// Source length in bytes.
        byte_len: u64,
    },
}

impl fmt::Display for SourceFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { byte_len } => write!(
                formatter,
                "source has {byte_len} bytes; the frontend span limit is {} bytes",
                u32::MAX
            ),
        }
    }
}

impl std::error::Error for SourceFileError {}

/// ECMAScript parse goal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseGoal {
    /// A Script grammar goal.
    Script,
    /// A Module grammar goal (implicitly strict).
    Module,
}

/// Strictness requested for Script input.
///
/// Module input is always strict regardless of this setting. A Script-level
/// use-strict directive also activates strict semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseMode {
    /// Ordinary Script semantics, subject to source directives.
    Sloppy,
    /// Strict Script semantics, equivalent to a leading strict directive.
    Strict,
}

/// The frontend phase that produced a diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticPhase {
    /// Lexer or grammar parsing failure.
    Parse,
    /// ECMAScript early syntax/semantic error.
    Early,
}

/// Severity of a frontend diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    /// Non-fatal warning/advice.
    Warning,
    /// Compilation-blocking error.
    Error,
}

/// One labeled span associated with a diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticLabel {
    /// Source location of the label.
    pub span: SourceSpan,
    /// Optional human-readable label message.
    pub message: Option<String>,
    /// Whether this is the primary label.
    pub primary: bool,
}

/// Structured source diagnostic independent of Oxc's renderer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseDiagnostic {
    /// Phase that produced the diagnostic.
    pub phase: DiagnosticPhase,
    /// Optional stable upstream diagnostic code.
    pub code: Option<String>,
    /// Diagnostic severity.
    pub severity: DiagnosticSeverity,
    /// Human-readable diagnostic message.
    pub message: String,
    /// Source labels, including the primary range when supplied by Oxc.
    pub labels: Vec<DiagnosticLabel>,
    /// Optional corrective guidance.
    pub help: Option<String>,
    /// Optional additional context.
    pub note: Option<String>,
}

/// Parse failure. No partial/recovered AST is exposed through this type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseFailure {
    /// Phase in which the input was rejected.
    pub phase: DiagnosticPhase,
    /// Structured diagnostics for the failure.
    pub diagnostics: Vec<ParseDiagnostic>,
}

/// Successfully parsed JavaScript program.
///
/// The AST borrows the source and compile-unit arena supplied to parse.
/// Drop this value before resetting or dropping that arena.
#[derive(Debug)]
pub struct ParsedProgram<'a> {
    /// Oxc JavaScript AST with original-source byte spans.
    pub program: Program<'a>,
    /// Source metadata that owns the text referenced by the AST.
    pub source: &'a SourceFile,
    /// Original grammar goal selected for this source.
    pub goal: ParseGoal,
    /// Effective strictness after applying the parse mode and source directives.
    ///
    /// Modules are always strict. Function-local directive prologues remain
    /// represented in the AST for the semantic-analysis stage to inspect.
    pub is_strict: bool,
    /// Non-fatal parser/semantic diagnostics, if any.
    pub diagnostics: Vec<ParseDiagnostic>,
}

/// Options for parsing the M1 JavaScript frontend subset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseRequestOptions {
    /// Script or Module grammar goal.
    pub goal: ParseGoal,
    /// Strict/sloppy mode for Script input.
    pub mode: ParseMode,
}

impl Default for ParseRequestOptions {
    fn default() -> Self {
        Self {
            goal: ParseGoal::Script,
            mode: ParseMode::Sloppy,
        }
    }
}

/// Parses JavaScript and runs Oxc's syntax/early-error semantic pass.
///
/// Every parser or early-error diagnostic with error severity rejects the
/// input. Recoverable Oxc parser output is never exposed as a valid program.
pub fn parse<'a>(
    allocator: &'a Allocator,
    source: &'a SourceFile,
    options: ParseRequestOptions,
) -> Result<ParsedProgram<'a>, ParseFailure> {
    let source_type = match options.goal {
        ParseGoal::Script => SourceType::script(),
        ParseGoal::Module => SourceType::mjs(),
    };
    let oxc_options = OxcParseOptions {
        preserve_parens: false,
        ..OxcParseOptions::default()
    };

    let parsed = Parser::new(allocator, source.text(), source_type)
        .with_options(oxc_options)
        .parse();
    let program = parsed.program;
    let parser_diagnostics = convert_diagnostics(
        parsed.diagnostics.into_vec(),
        DiagnosticPhase::Parse,
        source,
        OffsetMapping::Identity,
    );
    if has_errors(&parser_diagnostics) {
        return Err(ParseFailure {
            phase: DiagnosticPhase::Parse,
            diagnostics: parser_diagnostics,
        });
    }

    let mut diagnostics = parser_diagnostics;
    if options.goal == ParseGoal::Script && options.mode == ParseMode::Strict {
        // A strict directive is inserted into a temporary source instead of
        // reclassifying the source as a Module. This keeps Script-only grammar
        // (notably top-level await) distinct from Module grammar.
        let (strict_text, mapping) = strict_script_source(source, program.hashbang.as_ref());
        let strict_parse = Parser::new(allocator, &strict_text, SourceType::script())
            .with_options(oxc_options)
            .parse();
        let strict_program = strict_parse.program;
        let strict_parser_diagnostics = convert_diagnostics(
            strict_parse.diagnostics.into_vec(),
            DiagnosticPhase::Early,
            source,
            mapping,
        );
        if has_errors(&strict_parser_diagnostics) {
            return Err(ParseFailure {
                phase: DiagnosticPhase::Early,
                diagnostics: merge_diagnostics(diagnostics, strict_parser_diagnostics),
            });
        }
        diagnostics.extend(strict_parser_diagnostics);

        let semantic_result = SemanticBuilder::new_compiler().build(&strict_program);
        let semantic_diagnostics = convert_diagnostics(
            semantic_result.diagnostics.into_vec(),
            DiagnosticPhase::Early,
            source,
            mapping,
        );
        if has_errors(&semantic_diagnostics) {
            return Err(ParseFailure {
                phase: DiagnosticPhase::Early,
                diagnostics: merge_diagnostics(diagnostics, semantic_diagnostics),
            });
        }
        diagnostics.extend(semantic_diagnostics);
    } else {
        let semantic_result = SemanticBuilder::new_compiler().build(&program);
        let semantic_diagnostics = convert_diagnostics(
            semantic_result.diagnostics.into_vec(),
            DiagnosticPhase::Early,
            source,
            OffsetMapping::Identity,
        );
        if has_errors(&semantic_diagnostics) {
            return Err(ParseFailure {
                phase: DiagnosticPhase::Early,
                diagnostics: merge_diagnostics(diagnostics, semantic_diagnostics),
            });
        }
        diagnostics.extend(semantic_diagnostics);
    }

    let source_has_strict_directive = program
        .directives
        .iter()
        .any(|directive| directive.directive.as_str() == "use strict");
    Ok(ParsedProgram {
        program,
        source,
        goal: options.goal,
        is_strict: options.goal == ParseGoal::Module
            || options.mode == ParseMode::Strict
            || source_has_strict_directive,
        diagnostics,
    })
}

/// Returns a deterministic SHA-256 fingerprint of AST structure.
///
/// Parenthesized grouping, comments, whitespace and semicolon placement do not
/// appear in the canonical structure. Source byte offsets are normalized away.
pub fn normalized_ast_sha256(program: &Program<'_>) -> String {
    let debug_ast = format!("{:?}{:?}", program.directives, program.body);
    let normalized = normalize_span_offsets(&debug_ast);
    sha256_hex(normalized.as_bytes())
}

/// Computes a lowercase SHA-256 digest without adding a crypto dependency.
pub fn sha256_hex(bytes: &[u8]) -> String {
    const INITIAL: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
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

    let mut state = INITIAL;
    let bit_len = (bytes.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity(bytes.len().saturating_add(72));
    padded.extend_from_slice(bytes);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut words = [0_u32; 64];
        for (index, word) in words.iter_mut().take(16).enumerate() {
            let offset = index * 4;
            *word = u32::from_be_bytes([
                chunk[offset],
                chunk[offset + 1],
                chunk[offset + 2],
                chunk[offset + 3],
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
            let sum1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ (!e & g);
            let temp1 = h
                .wrapping_add(sum1)
                .wrapping_add(choose)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let sum0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = sum0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        let working = [a, b, c, d, e, f, g, h];
        for (slot, value) in state.iter_mut().zip(working) {
            *slot = slot.wrapping_add(value);
        }
    }

    let mut output = String::with_capacity(64);
    for value in state {
        use fmt::Write as _;
        let _ = write!(output, "{value:08x}");
    }
    output
}

#[derive(Clone, Copy)]
enum OffsetMapping {
    Identity,
    Inserted { at: usize, len: usize },
}

impl OffsetMapping {
    fn to_original(self, offset: usize) -> usize {
        match self {
            Self::Identity => offset,
            Self::Inserted { at, .. } if offset < at => offset,
            Self::Inserted { at, len } if offset < at.saturating_add(len) => at,
            Self::Inserted { len, .. } => offset.saturating_sub(len),
        }
    }
}

fn strict_script_source(
    source: &SourceFile,
    hashbang: Option<&oxc_ast::ast::Hashbang<'_>>,
) -> (String, OffsetMapping) {
    let insertion_at = hashbang
        .map(|value| value.span.end as usize)
        .unwrap_or(0)
        .min(source.text().len());
    let prefix = if insertion_at == 0 {
        "\"use strict\";\n"
    } else {
        "\n\"use strict\";\n"
    };
    let mut strict_text = String::with_capacity(source.text().len() + prefix.len());
    strict_text.push_str(&source.text()[..insertion_at]);
    strict_text.push_str(prefix);
    strict_text.push_str(&source.text()[insertion_at..]);
    (
        strict_text,
        OffsetMapping::Inserted {
            at: insertion_at,
            len: prefix.len(),
        },
    )
}

fn merge_diagnostics(
    mut first: Vec<ParseDiagnostic>,
    second: Vec<ParseDiagnostic>,
) -> Vec<ParseDiagnostic> {
    first.extend(second);
    first
}

fn has_errors(diagnostics: &[ParseDiagnostic]) -> bool {
    diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
}

fn convert_diagnostics(
    diagnostics: Vec<OxcDiagnostic>,
    phase: DiagnosticPhase,
    source: &SourceFile,
    mapping: OffsetMapping,
) -> Vec<ParseDiagnostic> {
    diagnostics
        .into_iter()
        .map(|diagnostic| {
            let severity = match diagnostic.severity {
                OxcSeverity::Error => DiagnosticSeverity::Error,
                OxcSeverity::Advice | OxcSeverity::Warning => DiagnosticSeverity::Warning,
            };
            let labels = diagnostic
                .labels
                .iter()
                .map(|label| {
                    let start = mapping.to_original(label.offset() as usize);
                    let end = mapping.to_original(
                        (label.offset() as usize).saturating_add(label.len() as usize),
                    );
                    DiagnosticLabel {
                        span: source.span(start, end),
                        message: label.label().map(str::to_owned),
                        primary: label.primary(),
                    }
                })
                .collect();
            let code = diagnostic.code.to_string();
            ParseDiagnostic {
                phase,
                code: (!code.is_empty()).then_some(code),
                severity,
                message: diagnostic.message.to_string(),
                labels,
                help: diagnostic.help.as_deref().map(str::to_owned),
                note: diagnostic.note.as_deref().map(str::to_owned),
            }
        })
        .collect()
}

fn build_line_starts(text: &str) -> Vec<u32> {
    let bytes = text.as_bytes();
    let mut starts = vec![0];
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' => {
                if bytes.get(index + 1) == Some(&b'\n') {
                    index += 2;
                } else {
                    index += 1;
                }
                starts.push(index as u32);
            }
            b'\n' => {
                index += 1;
                starts.push(index as u32);
            }
            _ => {
                let character = text[index..].chars().next().expect("valid UTF-8 boundary");
                let width = character.len_utf8();
                if character == '\u{2028}' || character == '\u{2029}' {
                    index += width;
                    starts.push(index as u32);
                } else {
                    index += width;
                }
            }
        }
    }
    starts
}

fn floor_char_boundary(text: &str, mut offset: usize) -> usize {
    while !text.is_char_boundary(offset) {
        offset = offset.saturating_sub(1);
    }
    offset
}

fn normalize_span_offsets(debug_ast: &str) -> String {
    let mut result = String::with_capacity(debug_ast.len());
    let mut cursor = 0;
    while let Some(relative) = debug_ast[cursor..].find("Span {") {
        let start = cursor + relative;
        result.push_str(&debug_ast[cursor..start]);
        let Some(relative_end) = debug_ast[start..].find('}') else {
            result.push_str(&debug_ast[start..]);
            return result;
        };
        let end = start + relative_end + 1;
        let mut span_debug = debug_ast[start..end].to_owned();
        normalize_numeric_field(&mut span_debug, "start: ");
        normalize_numeric_field(&mut span_debug, "end: ");
        result.push_str(&span_debug);
        cursor = end;
    }
    result.push_str(&debug_ast[cursor..]);
    result
}

fn normalize_numeric_field(value: &mut String, field: &str) {
    let Some(field_start) = value.find(field) else {
        return;
    };
    let number_start = field_start + field.len();
    let number_end = value[number_start..]
        .find(|character: char| !character.is_ascii_digit())
        .map(|offset| number_start + offset)
        .unwrap_or(value.len());
    if number_end > number_start {
        value.replace_range(number_start..number_end, "0");
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DiagnosticPhase, ParseGoal, ParseMode, ParseRequestOptions, SourceFile,
        normalized_ast_sha256, parse, sha256_hex,
    };
    use oxc_allocator::Allocator;

    fn source(text: &str) -> SourceFile {
        SourceFile::new("test:unit", "memory:///unit.js", None, text).expect("source is bounded")
    }

    fn parse_text(
        text: &str,
        goal: ParseGoal,
        mode: ParseMode,
    ) -> Result<String, super::ParseFailure> {
        let allocator = Allocator::default();
        let source = source(text);
        parse(&allocator, &source, ParseRequestOptions { goal, mode })
            .map(|parsed| normalized_ast_sha256(&parsed.program))
    }

    #[test]
    fn sha256_matches_standard_vectors() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn maps_utf8_byte_offsets_to_utf16_editor_columns_and_crlf_lines() {
        let source = source("a😀b\r\nz\u{2028}q");
        assert_eq!(
            source.position(5),
            super::SourcePosition {
                line: 1,
                column_utf16: 3,
            }
        );
        assert_eq!(
            source.position(8),
            super::SourcePosition {
                line: 2,
                column_utf16: 0,
            }
        );
        assert_eq!(
            source.position(12),
            super::SourcePosition {
                line: 3,
                column_utf16: 0,
            }
        );
    }

    #[test]
    fn parses_javascript_and_preserves_source_identity() {
        let allocator = Allocator::default();
        let source = source("const answer = 40 + 2;");
        let parsed = parse(&allocator, &source, ParseRequestOptions::default())
            .expect("ordinary JavaScript should parse");
        assert_eq!(parsed.source.id().as_str(), "test:unit");
        assert_eq!(parsed.source.uri(), "memory:///unit.js");
        assert_eq!(parsed.program.body.len(), 1);
        assert_eq!(
            parsed.source.content_sha256(),
            sha256_hex(source.text().as_bytes())
        );
    }

    #[test]
    fn rejects_recoverable_syntax_errors_instead_of_exposing_partial_ast() {
        let allocator = Allocator::default();
        let source = source("const = ;\nconst afterFirstError = 1;");
        let failure = parse(&allocator, &source, ParseRequestOptions::default())
            .expect_err("invalid source must not expose Oxc's recovered AST");
        assert_eq!(failure.phase, DiagnosticPhase::Parse);
        assert!(!failure.diagnostics.is_empty());
        assert!(
            failure
                .diagnostics
                .iter()
                .any(|diagnostic| !diagnostic.labels.is_empty())
        );
    }

    #[test]
    fn strict_script_and_module_early_errors_are_distinguished_from_sloppy_script() {
        let text = "function duplicate(parameter, parameter) { return parameter; }";
        assert!(parse_text(text, ParseGoal::Script, ParseMode::Sloppy).is_ok());
        let strict = parse_text(text, ParseGoal::Script, ParseMode::Strict)
            .expect_err("duplicate parameters are forbidden in strict code");
        assert_eq!(strict.phase, DiagnosticPhase::Early);
        let module = parse_text(text, ParseGoal::Module, ParseMode::Sloppy)
            .expect_err("module code is always strict");
        assert_eq!(module.phase, DiagnosticPhase::Early);
    }

    #[test]
    fn strict_script_insertion_preserves_hashbang_and_reports_original_offsets() {
        let source = source("#!/usr/bin/env node\nfunction duplicate(a, a) {}");
        let allocator = Allocator::default();
        let failure = parse(
            &allocator,
            &source,
            ParseRequestOptions {
                goal: ParseGoal::Script,
                mode: ParseMode::Strict,
            },
        )
        .expect_err("duplicate parameters are an early strict error");
        assert_eq!(failure.phase, DiagnosticPhase::Early);
        assert!(
            failure
                .diagnostics
                .iter()
                .flat_map(|diagnostic| diagnostic.labels.iter())
                .all(|label| label.span.end_byte as usize <= source.text().len())
        );
    }

    #[test]
    fn source_directive_activates_strict_semantics_even_in_sloppy_mode() {
        let allocator = Allocator::default();
        let source = source("\"use strict\"; const value = 1;");
        let parsed = parse(
            &allocator,
            &source,
            ParseRequestOptions {
                goal: ParseGoal::Script,
                mode: ParseMode::Sloppy,
            },
        )
        .expect("valid strict script should parse");
        assert_eq!(parsed.goal, ParseGoal::Script);
        assert!(parsed.is_strict);

        let failure = parse_text(
            "\"use strict\"; function duplicate(a, a) {}",
            ParseGoal::Script,
            ParseMode::Sloppy,
        )
        .expect_err("the source directive is normative");
        assert_eq!(failure.phase, DiagnosticPhase::Early);
    }

    #[test]
    fn modules_are_strict_even_when_sloppy_mode_is_requested() {
        let allocator = Allocator::default();
        let source = source("export const value = 1;");
        let parsed = parse(
            &allocator,
            &source,
            ParseRequestOptions {
                goal: ParseGoal::Module,
                mode: ParseMode::Sloppy,
            },
        )
        .expect("valid module should parse");
        assert_eq!(parsed.goal, ParseGoal::Module);
        assert!(parsed.is_strict);
    }

    #[test]
    fn parser_normalization_ignores_parentheses_whitespace_and_semicolons() {
        let first = parse_text("[ 1 ]", ParseGoal::Script, ParseMode::Sloppy)
            .expect("first variant parses");
        let second = parse_text("[1];", ParseGoal::Script, ParseMode::Sloppy)
            .expect("second variant parses");
        assert_eq!(first, second);
    }

    #[test]
    fn script_with_statement_is_sloppy_only() {
        assert!(
            parse_text(
                "with ({ value: 1 }) { value; }",
                ParseGoal::Script,
                ParseMode::Sloppy
            )
            .is_ok()
        );
        let strict = parse_text(
            "with ({ value: 1 }) { value; }",
            ParseGoal::Script,
            ParseMode::Strict,
        )
        .expect_err("with statements are rejected in strict scripts");
        assert_eq!(strict.phase, DiagnosticPhase::Early);
    }
}
