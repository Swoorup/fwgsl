//! Token-stream based formatter for shadml.
//!
//! Operates on the raw lexed tokens (not the layout-resolved stream), so that
//! comments and original structure are preserved. Re-emits tokens with
//! canonical whitespace and indentation.

use shadml_parser::lex;
use shadml_parser::lexer::{is_negative_literal_start, Token};
use shadml_syntax::SyntaxKind;

pub mod config;

pub use config::{load_formatter_config, AttributeStyle, FormatConfig};

/// Format a shadml source string with the given configuration.
///
/// Processing pipeline (in order):
///   1. Token-stream pass       — normalise intra-line spacing, emit tokens
///   2. Binding-group collapse  — fold flat `@group` lines into indented blocks
///   3. Record-field alignment  — align `:` colons across consecutive fields
///   4. If-then-else restructure — canonicalise multi-line `if` expressions
///   5. Line breaking           — break over-long lines at safe syntactic points
///
/// Restructuring runs *before* line breaking so that the canonical `if`-form
/// is established first; subsequent line-breaking then ensures each output
/// line stays within `max_width`.  If the order were reversed, line-breaking
/// decisions (e.g. breaking at `=`) would be partially undone by the
/// restructuring pass, and the test suite would need adjusting.
pub fn format(source: &str, config: &FormatConfig) -> String {
    let tokens = lex(source);
    let mut engine = FormatEngine::new(source, &tokens);
    engine.run();
    collapse_binding_group_blocks(&mut engine.output, config);
    align_record_fields(&mut engine.output, config);
    restructure_if_then_else(&mut engine.output, config);
    if config.enforce_max_width {
        break_long_lines(&mut engine.output, config);
    }
    engine.output
}

/// Format a shadml source string with default configuration.
pub fn format_default(source: &str) -> String {
    format(source, &FormatConfig::default())
}

// ---------------------------------------------------------------------------
// Format engine
// ---------------------------------------------------------------------------

struct FormatEngine<'a> {
    source: &'a str,
    tokens: &'a [Token],
    pos: usize,
    output: String,
    /// Current column in the output (0-based).
    col: usize,
    /// True if we're at the start of a line (only whitespace so far).
    at_line_start: bool,
    /// Number of consecutive blank lines emitted.
    blank_lines: usize,
    /// Nesting depth of type-parameter angle brackets (`Vec<..>`).
    type_angle_depth: usize,
    /// The last mid-line whitespace that was skipped (for alignment preservation).
    last_skipped_ws: Option<&'a str>,
    /// Kind of the last non-trivia token emitted (cached for O(1) lookup).
    last_kind: Option<SyntaxKind>,
    /// The end byte offset of the last non-trivia token (cached for adjacency check).
    last_span_end: u32,
    /// Whether we are in `@ident` attribute-name context.
    at_attr: bool,
    /// Whether the last non-trivia token puts us in a unary prefix context.
    unary_context: bool,
    /// Whether the current output line contains an `@`.
    line_has_at: bool,
    /// The index of the last non-trivia token in the input slice.
    last_idx: usize,
    /// Whether the current output line is indented (starts with whitespace).
    line_indented: bool,
    /// Whether the current output line already has at least one `=` token.
    /// Subsequent `=` on the same line (e.g. in `where` / `let` clauses) are
    /// treated as alignment targets even on non-indented lines.
    line_has_equals: bool,
}

impl<'a> FormatEngine<'a> {
    fn new(source: &'a str, tokens: &'a [Token]) -> Self {
        FormatEngine {
            source,
            tokens,
            pos: 0,
            output: String::with_capacity(source.len()),
            col: 0,
            at_line_start: true,
            blank_lines: 0,
            type_angle_depth: 0,
            last_skipped_ws: None,
            last_kind: None,
            last_span_end: 0,
            at_attr: false,
            unary_context: true, // start-of-file = unary context
            line_has_at: false,
            last_idx: 0,
            line_indented: false,
            line_has_equals: false,
        }
    }

    fn text(&self, tok: &Token) -> &'a str {
        tok.span.source_text(self.source)
    }

    #[allow(dead_code)]
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    #[allow(dead_code)]
    fn peek_kind(&self) -> SyntaxKind {
        self.peek().map_or(SyntaxKind::Eof, |t| t.kind)
    }

    fn advance(&mut self) -> &Token {
        let tok = &self.tokens[self.pos];
        self.pos += 1;
        tok
    }

    fn emit_str(&mut self, s: &str) {
        for ch in s.chars() {
            if ch == '\n' {
                self.col = 0;
                self.at_line_start = true;
                self.line_has_at = false;
                self.line_indented = false;
            } else {
                if self.at_line_start && (ch == ' ' || ch == '\t') {
                    self.line_indented = true;
                }
                self.col += 1;
                self.at_line_start = false;
                if ch == '@' {
                    self.line_has_at = true;
                }
            }
        }
        self.output.push_str(s);
    }

    fn emit_newline(&mut self) {
        self.output.push('\n');
        self.col = 0;
        self.at_line_start = true;
        self.line_has_at = false;
        self.line_indented = false;
        self.line_has_equals = false;
    }

    fn emit_space(&mut self) {
        self.output.push(' ');
        self.col += 1;
        self.at_line_start = false;
    }

    #[allow(dead_code)]
    fn emit_spaces(&mut self, n: usize) {
        for _ in 0..n {
            self.output.push(' ');
        }
        self.col += n;
        if n > 0 {
            self.at_line_start = false;
        }
    }

    /// Compute the column of a byte offset in the source.
    #[allow(dead_code)]
    fn source_column(&self, offset: u32) -> usize {
        let off = offset as usize;
        let line_start = self.source[..off].rfind('\n').map_or(0, |i| i + 1);
        off - line_start
    }

    fn run(&mut self) {
        // The formatter preserves the original indentation and structure,
        // but normalizes whitespace within lines:
        // - Single space between tokens on the same line
        // - Spaces around binary operators
        // - No trailing whitespace
        // - Normalize blank lines between top-level declarations to exactly 1
        // - Preserve comment placement

        while self.pos < self.tokens.len() {
            let tok = &self.tokens[self.pos];
            match tok.kind {
                SyntaxKind::Newline => {
                    self.advance();
                    self.trim_trailing_whitespace();
                    self.emit_newline();
                    self.blank_lines += 1;
                    self.last_skipped_ws = None;
                }
                SyntaxKind::Whitespace => {
                    // Whitespace at line start = indentation. Preserve it.
                    // Whitespace mid-line = normalize to single space (handled per-token below).
                    let text = self.text(tok);
                    if self.at_line_start {
                        // Count blank lines: if we just saw newlines, check for top-level gaps
                        if self.blank_lines > 1 {
                            // Normalize multiple blank lines to exactly one
                            // (we already emitted one newline; the extra ones were counted)
                            // Remove extra blank lines from output
                            self.collapse_blank_lines();
                        }
                        self.blank_lines = 0;
                        // Preserve original indentation
                        self.advance();
                        self.emit_str(text);
                    } else {
                        // Mid-line whitespace — skip it; we insert canonical spacing
                        // between tokens in the main loop.
                        // Store the original whitespace for alignment preservation.
                        let ws_text = self.text(tok);
                        self.last_skipped_ws = Some(ws_text);
                        self.advance();
                    }
                }
                SyntaxKind::LineComment => {
                    if self.blank_lines > 1 {
                        self.collapse_blank_lines();
                    }
                    self.blank_lines = 0;
                    if !self.at_line_start && self.col > 0 {
                        // Trailing comment — ensure single space before it
                        self.ensure_single_space();
                    }
                    let text = self.text(tok);
                    self.advance();
                    self.emit_str(text);
                }
                SyntaxKind::BlockComment => {
                    if self.blank_lines > 1 {
                        self.collapse_blank_lines();
                    }
                    self.blank_lines = 0;
                    if !self.at_line_start {
                        self.ensure_single_space();
                    }
                    let text = self.text(tok);
                    self.advance();
                    self.emit_str(text);
                }
                SyntaxKind::Eof => break,
                _ => {
                    if self.blank_lines > 1 {
                        self.collapse_blank_lines();
                    }
                    self.blank_lines = 0;

                    // For non-trivia tokens, ensure proper spacing
                    if !self.at_line_start {
                        self.emit_inter_token_space(tok);
                    }
                    self.last_skipped_ws = None;

                    // Track type-parameter angle brackets (after spacing decision)
                    if tok.kind == SyntaxKind::Less
                        && self.last_kind == Some(SyntaxKind::UpperIdent)
                    {
                        self.type_angle_depth += 1;
                    } else if tok.kind == SyntaxKind::Greater && self.type_angle_depth > 0 {
                        self.type_angle_depth -= 1;
                    }

                    // Update cached state for the next spacing decision.
                    self.last_kind = Some(tok.kind);
                    self.last_span_end = tok.span.end;
                    self.last_idx = self.pos;

                    // Track attribute context: @ ident -> at_attr = true.
                    // Reset at_attr when we reach a non-ident, non-upper-ident token.
                    if tok.kind == SyntaxKind::At {
                        self.at_attr = true;
                    } else if tok.kind == SyntaxKind::Ident
                        || tok.kind == SyntaxKind::UpperIdent
                        || tok.kind == SyntaxKind::KwBuiltin
                    {
                        // keep at_attr state (it was set by preceding @ or already false)
                    } else {
                        self.at_attr = false;
                    }

                    // Track unary context for the next token.
                    // Don't overwrite when the current token is itself a prefix operator
                    // (so that e.g. `|| !b` keeps unary_context=true from `||`).
                    if !matches!(
                        tok.kind,
                        SyntaxKind::Minus | SyntaxKind::Bang | SyntaxKind::Tilde
                    ) {
                        self.unary_context = matches!(
                            tok.kind,
                            SyntaxKind::LParen
                                | SyntaxKind::LBracket
                                | SyntaxKind::Equals
                                | SyntaxKind::Comma
                                | SyntaxKind::Arrow
                                | SyntaxKind::Pipe
                                | SyntaxKind::OrOr
                                | SyntaxKind::AndAnd
                                | SyntaxKind::KwLet
                                | SyntaxKind::KwIn
                                | SyntaxKind::KwThen
                                | SyntaxKind::KwElse
                                | SyntaxKind::KwIf
                                | SyntaxKind::KwMatch
                                | SyntaxKind::KwCase
                                | SyntaxKind::KwOf
                        );
                    }

                    let text = self.text(tok);
                    self.advance();
                    self.emit_str(text);

                    // Track `=` tokens for multi-equals alignment on the current line.
                    if tok.kind == SyntaxKind::Equals {
                        self.line_has_equals = true;
                    }
                }
            }
        }

        // Final cleanup
        self.trim_trailing_whitespace();
        // Ensure file ends with exactly one newline
        if !self.output.ends_with('\n') {
            self.output.push('\n');
        }
        // Remove trailing blank lines (keep exactly one \n at end)
        while self.output.ends_with("\n\n") {
            self.output.pop();
        }
    }

    /// Emit appropriate spacing between the previous token and the next one.
    fn emit_inter_token_space(&mut self, next: &Token) {
        let kind = next.kind;
        let prev = self.last_kind;

        // No space before certain punctuation
        if matches!(
            kind,
            SyntaxKind::RParen | SyntaxKind::RBracket | SyntaxKind::Comma | SyntaxKind::Semicolon
        ) {
            return;
        }

        // No space after opening brackets (already handled since we're looking at 'next')
        if prev == Some(SyntaxKind::LParen) || prev == Some(SyntaxKind::LBracket) {
            return;
        }

        // No space between `@` and attribute name
        if prev == Some(SyntaxKind::At) {
            return;
        }

        // No space before/after `.` (field access, composition)
        if kind == SyntaxKind::Dot || prev == Some(SyntaxKind::Dot) {
            return;
        }

        // No space after `\` (lambda)
        if prev == Some(SyntaxKind::Backslash) {
            return;
        }

        // No space between attribute/function name and `(` — e.g. `@builtin(...)`,
        // `@workgroup_size(...)`, `@location(0)`, `@interpolate(flat)`
        if kind == SyntaxKind::LParen
            && matches!(
                prev,
                Some(SyntaxKind::Ident | SyntaxKind::UpperIdent | SyntaxKind::KwBuiltin)
            )
            && self.at_attr
        {
            return;
        }

        // No space between `storage` keyword and `(` for `storage(read_write)`
        if kind == SyntaxKind::LParen && prev == Some(SyntaxKind::KwStorage) {
            return;
        }

        // No space before `[` when preceded by an identifier or `)` (array indexing)
        // but only when the tokens were adjacent in the original source.
        if kind == SyntaxKind::LBracket
            && self.last_skipped_ws.is_none()
            && matches!(
                prev,
                Some(
                    SyntaxKind::Ident
                        | SyntaxKind::UpperIdent
                        | SyntaxKind::RParen
                        | SyntaxKind::RBracket
                )
            )
        {
            return;
        }

        // No space after unary `-`, `!`, or `~` when they appear as prefix operators.
        // Detect prefix context: the operator follows `(`, `=`, `let`, `in`,
        // `then`, `else`, `->`, a comma, or is at statement start.
        if (prev == Some(SyntaxKind::Minus)
            || prev == Some(SyntaxKind::Bang)
            || prev == Some(SyntaxKind::Tilde))
            && self.unary_context
        {
            return;
        }

        // Keep negative literals as single "words" when there's a space before `-` but no space after it.
        // e.g., `vec2 -0.5` instead of `vec2 - 0.5`.
        if self.last_idx > 0 && is_negative_literal_start(self.tokens, self.last_idx) {
            return;
        }

        // No space between two adjacent `>` tokens that form `>>` (shift right).
        if kind == SyntaxKind::Greater
            && prev == Some(SyntaxKind::Greater)
            && self.last_span_end == next.span.start
        {
            return;
        }

        // Type parameter angle brackets: no space around `<`, `>`, or after `,`
        if kind == SyntaxKind::Less && prev == Some(SyntaxKind::UpperIdent) {
            return; // no space before `<` in `Vec<`
        }
        if kind == SyntaxKind::Greater && self.type_angle_depth > 0 {
            return; // no space before `>` in `...>`
        }
        if prev == Some(SyntaxKind::Less) && self.type_angle_depth > 0 {
            return; // no space after `<` in `<4, ...`
        }

        // Preserve original multi-space padding before alignment-sensitive tokens.
        // This keeps author-intentional column alignment in let/where `=`, record `:`,
        // match `->`, const `:`, and binding `:` / `@group`.
        if let Some(ws) = self.last_skipped_ws {
            if ws.len() > 1 {
                let is_alignment_target = match kind {
                    // `=` alignment preserved on indented lines (let/where bindings,
                    // record construction fields) OR when there's already a `=` on
                    // the line (secondary `=` in `where` / `let` clauses).
                    SyntaxKind::Equals => self.line_indented || self.line_has_equals,
                    // `:`, `->`, `@` alignment preserved everywhere (const, binding, match arms).
                    SyntaxKind::Colon | SyntaxKind::Arrow | SyntaxKind::At => true,
                    // Ident after `)` — preserve attribute-to-field-name padding in records
                    SyntaxKind::Ident | SyntaxKind::UpperIdent
                        if prev == Some(SyntaxKind::RParen) && self.line_has_at =>
                    {
                        true
                    }
                    _ => false,
                };
                if is_alignment_target {
                    self.emit_str(ws);
                    return;
                }
            }
        }

        // Single space for everything else
        self.ensure_single_space();
    }

    fn ensure_single_space(&mut self) {
        if !self.output.ends_with(' ') && !self.output.ends_with('\n') {
            self.emit_space();
        }
    }

    /// Remove trailing spaces from the last line in the output.
    fn trim_trailing_whitespace(&mut self) {
        while self.output.ends_with(' ') || self.output.ends_with('\t') {
            self.output.pop();
        }
    }

    /// Collapse multiple blank lines to at most one.
    fn collapse_blank_lines(&mut self) {
        // Remove extra trailing newlines, keep at most 2 (which = 1 blank line)
        while self.output.ends_with("\n\n\n") {
            // Find the position to truncate
            let len = self.output.len();
            self.output.truncate(len - 1);
        }
    }
}

// ---------------------------------------------------------------------------
// Post-processing helpers
// ---------------------------------------------------------------------------

/// Apply a line-based transformation to the formatted output.  Wraps the
/// common "split → process → join" pattern that every post-processing pass
/// uses, removing boilerplate.
fn with_lines(output: &mut String, f: impl FnOnce(Vec<&str>) -> Vec<String>) {
    let lines: Vec<&str> = output.split('\n').collect();
    *output = f(lines).join("\n");
}

// ---------------------------------------------------------------------------
// Post-processing: align `:` in record field blocks
// ---------------------------------------------------------------------------

/// Parsed structure of a single record field line.
struct FieldLineInfo {
    /// Byte offset where leading whitespace ends.
    indent: usize,
    /// The raw text of all attributes (including trailing spaces).
    attrs_text: String,
    /// The raw field name.
    name: String,
    /// Everything from the colon to the end of the line.
    suffix: String,
}

/// Align attributes, field names, and colons in consecutive record field lines
/// that share the same indentation.
///
/// When `config.record_attribute_style` is `Auto`, any field whose aligned
/// version would exceed `max_width` is emitted in "Style B" (attributes on
/// their own line, field name indented underneath).
fn align_record_fields(output: &mut String, config: &FormatConfig) {
    with_lines(output, |lines| {
        let mut result: Vec<String> = Vec::with_capacity(lines.len());
        let mut i = 0;

        while i < lines.len() {
            if let Some(info) = parse_field_line(lines[i]) {
                let mut group: Vec<(usize, FieldLineInfo)> = vec![(i, info)];
                i += 1;
                while i < lines.len() {
                    if let Some(info2) = parse_field_line(lines[i]) {
                        if info2.indent == group[0].1.indent {
                            group.push((i, info2));
                            i += 1;
                            continue;
                        }
                    }
                    break;
                }

                if group.len() > 1 {
                    let infos: Vec<&FieldLineInfo> = group.iter().map(|(_, info)| info).collect();
                    let has_attrs = infos.iter().any(|i| !i.attrs_text.trim().is_empty());
                    let max_attr_len = infos
                        .iter()
                        .map(|i| i.attrs_text.trim_end().len())
                        .max()
                        .unwrap();
                    let max_name_len = infos.iter().map(|i| i.name.len()).max().unwrap();

                    for (idx, info) in &group {
                        let line = lines[*idx];
                        let use_own_line = should_field_use_own_line(
                            info,
                            max_attr_len,
                            max_name_len,
                            line,
                            config,
                        );

                        // If the line was already manually aligned (multi-space gap
                        // between name and colon), preserve the original.
                        if !use_own_line && is_manually_aligned_field(line, info) {
                            result.push(line.to_string());
                            continue;
                        }

                        if use_own_line {
                            // Style B: attributes on their own line, then indented field name
                            let indent_str = &line[..info.indent];
                            let inner_indent =
                                format!("{}{}", indent_str, " ".repeat(config.indent_width));
                            if !info.attrs_text.trim().is_empty() {
                                for attr in info
                                    .attrs_text
                                    .trim_end()
                                    .split('@')
                                    .filter(|s| !s.trim().is_empty())
                                {
                                    result.push(format!("{}@{}", indent_str, attr.trim()));
                                }
                            }
                            let name_padding = max_name_len - info.name.len();
                            let padded_name = format!("{}{}", info.name, " ".repeat(name_padding));
                            result
                                .push(format!("{}{} : {}", inner_indent, padded_name, info.suffix));
                        } else {
                            // Style A: inline with aligned columns
                            let attr_text = info.attrs_text.trim_end();
                            let attr_padding = if has_attrs {
                                max_attr_len - attr_text.len() + 1
                            } else {
                                0
                            };
                            let name_padding = max_name_len - info.name.len() + 1;

                            let mut aligned =
                                String::with_capacity(line.len() + max_attr_len + max_name_len + 4);
                            aligned.push_str(&line[..info.indent]);
                            aligned.push_str(attr_text);
                            aligned.push_str(" ".repeat(attr_padding).as_str());
                            aligned.push_str(&info.name);
                            aligned.push_str(" ".repeat(name_padding).as_str());
                            aligned.push_str(": ");
                            aligned.push_str(&info.suffix);
                            result.push(aligned);
                        }
                    }
                } else {
                    for (idx, _) in &group {
                        result.push(lines[*idx].to_string());
                    }
                }
            } else {
                result.push(lines[i].to_string());
                i += 1;
            }
        }

        result
    });
}

/// Decide whether a field should use Style B (attributes on own line).
fn should_field_use_own_line(
    info: &FieldLineInfo,
    max_attr_len: usize,
    max_name_len: usize,
    _original_line: &str,
    config: &FormatConfig,
) -> bool {
    let attr_part = if info.attrs_text.trim().is_empty() {
        max_attr_len + 1
    } else {
        info.attrs_text.trim_end().len() + (max_attr_len - info.attrs_text.trim_end().len() + 1)
    };
    let name_part = info.name.len() + (max_name_len - info.name.len() + 1);
    let aligned_len = info.indent + attr_part + name_part + 3 + info.suffix.len();
    should_fallback_to_own_line(
        &info.attrs_text,
        aligned_len,
        config.record_attribute_style,
        config.attribute_threshold,
        config.enforce_max_width,
        config.max_width,
    )
}

/// Check whether any single attribute in `attrs_text` exceeds the threshold.
fn has_long_attribute(attrs_text: &str, threshold: usize) -> bool {
    attrs_text.split('@').filter(|s| !s.is_empty()).any(|attr| {
        let len = attr.trim_end().len() + 1; // +1 for the leading '@'
        len > threshold
    })
}

/// Shared logic for deciding whether to fall back to Style B (own-line)
/// in `Auto` mode.  Used by both field alignment and binding alignment.
fn should_fallback_to_own_line(
    attrs_text: &str,
    aligned_len: usize,
    style: AttributeStyle,
    attribute_threshold: usize,
    enforce_max_width: bool,
    max_width: usize,
) -> bool {
    match style {
        AttributeStyle::OwnLine => true,
        AttributeStyle::Inline => false,
        AttributeStyle::Auto => {
            if has_long_attribute(attrs_text, attribute_threshold) {
                return true;
            }
            if !enforce_max_width {
                return false;
            }
            aligned_len > max_width
        }
    }
}

/// Check whether a field line already has manual multi-space alignment.
///
/// Returns `true` when there are two or more spaces between either:
/// - the last attribute and the field name, or
/// - the field name and the colon,
/// indicating the author intentionally aligned the line.
fn is_manually_aligned_field(line: &str, info: &FieldLineInfo) -> bool {
    let name_start = match line[info.indent..].find(&info.name) {
        Some(p) => info.indent + p,
        None => return false,
    };
    let name_end = name_start + info.name.len();

    // Check gap between attribute and name
    let attr_to_name_gap =
        name_start.saturating_sub(info.indent + info.attrs_text.trim_end().len());
    if attr_to_name_gap > 1 {
        return true;
    }

    // Check gap between name and colon
    if let Some(colon_pos) = line[name_end..].find(':') {
        let colon_abs = name_end + colon_pos;
        let gap = line[name_end..colon_abs].len();
        if gap > 1 {
            return true;
        }
    }

    false
}

/// Parse a line as a record field declaration.
///
/// Returns `Some(FieldLineInfo)` when the line matches:
/// `<indent>[<@attr(...)> ]*<ident> : <rest>`
///
/// Lines whose parsed attributes contain `@binding(` are rejected so that
/// render-block binding declarations are not mistaken for record fields.
fn parse_field_line(line: &str) -> Option<FieldLineInfo> {
    let bytes = line.as_bytes();
    let indent = bytes
        .iter()
        .take_while(|&&b| b == b' ' || b == b'\t')
        .count();
    if indent == 0 || indent >= bytes.len() {
        return None;
    }

    let mut pos = indent;
    let attr_start = pos;

    // Parse optional attributes
    while bytes.get(pos) == Some(&b'@') {
        pos += 1;
        let ident_start = pos;
        while pos < bytes.len() && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_') {
            pos += 1;
        }
        if pos == ident_start {
            return None;
        }
        if pos < bytes.len() && bytes[pos] == b'(' {
            let close = line[pos..].find(')')?;
            pos += close + 1;
        }
        while pos < bytes.len() && (bytes[pos] == b' ' || bytes[pos] == b'\t') {
            pos += 1;
        }
    }

    let attr_end = pos;
    let attrs_text = line[attr_start..attr_end].to_string();

    // Reject binding declarations (e.g. `@group(0) @binding(0) uniform name : Type`)
    if attrs_text.contains("@binding(") {
        return None;
    }

    // Must start with an identifier character
    if pos >= bytes.len() || !(bytes[pos].is_ascii_alphabetic() || bytes[pos] == b'_') {
        return None;
    }

    let name_start = pos;
    while pos < bytes.len() && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_') {
        pos += 1;
    }
    if pos == name_start {
        return None;
    }
    let name = line[name_start..pos].to_string();

    // Must be followed by whitespace then `: `
    if pos >= bytes.len() || bytes[pos] != b' ' {
        return None;
    }
    while pos < bytes.len() && bytes[pos] == b' ' {
        pos += 1;
    }
    if pos >= bytes.len() || bytes[pos] != b':' {
        return None;
    }
    if pos + 1 >= bytes.len() || bytes[pos + 1] != b' ' {
        return None;
    }
    pos += 2; // skip ": "

    let suffix = line[pos..].to_string();

    Some(FieldLineInfo {
        indent,
        attrs_text,
        name,
        suffix,
    })
}

// ---------------------------------------------------------------------------
// Post-processing: restructure if-then-else expressions
// ---------------------------------------------------------------------------

/// Byte lengths of the keywords we restructure (excluding trailing space).
const IF_LEN: usize = 2;
const THEN_LEN: usize = 4;
const ELSE_LEN: usize = 4;

/// Restructure multi-line `if … then … else …` expressions to put `then` and
/// `else` on their own lines with consistent indentation.
///
/// Only applies to expressions that already span multiple physical lines or
/// that exceed `max_width`.  Expressions that fit on a single line are left
/// unchanged so that compact `if … then … else …` one-liners are preserved.
fn restructure_if_then_else(output: &mut String, config: &FormatConfig) {
    with_lines(output, |lines| {
        let mut result: Vec<String> = Vec::with_capacity(lines.len() * 3);
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i];
            let trimmed = line.trim_start();

            // Never touch empty / comment lines.
            if trimmed.is_empty() || trimmed.starts_with("--") {
                result.push(line.to_string());
                i += 1;
                continue;
            }

            // Collect a logical expression group: one line that contains an `if`,
            // possibly followed by continuation lines (lines indented *more* than
            // the first line, ending at a blank / comment / keyword-started line).
            let indent_len = leading_indent_len(line);

            // Only consider multi-line candidates.  A single-line if-expression
            // that fits within max_width is already fine.
            let mut full = String::from(trimmed);
            let mut consumed = 1;

            if i + 1 < lines.len() {
                let mut j = i + 1;
                while j < lines.len() {
                    let next = lines[j];
                    let next_trimmed = next.trim_start();
                    // A blank or comment-only line between the expression and its
                    // continuation signals a logical break; stop collecting.
                    if next_trimmed.is_empty() || next_trimmed.starts_with("--") {
                        break;
                    }
                    // A continuation line has more indent than the first line, but
                    // not so much more that it's clearly a deeper nesting level.
                    // We cap the indent delta to 3× indent_width so that top-level
                    // lines don't accidentally swallow an entire indented block.
                    let next_indent = leading_indent_len(next);
                    let max_cont_indent = indent_len + config.indent_width * 3;
                    if next_indent <= indent_len || next_indent > max_cont_indent {
                        break;
                    }
                    // Lines containing `=` are standalone `where` / `let` bindings,
                    // not expression continuations.  Stop collecting when we hit one.
                    // We check for ` = ` (space-equals-space) or line starting with `= `,
                    // which are assignment-like; we skip `<=`, `>=`, `==`, `!=`, `->`.
                    if has_standalone_equals(next_trimmed) {
                        break;
                    }
                    full.push(' ');
                    full.push_str(next_trimmed);
                    consumed += 1;
                    j += 1;
                }
            }

            if consumed == 1 {
                // Single physical line — leave it alone.
                result.push(line.to_string());
                i += 1;
                continue;
            }

            // If any of the continuation lines already start with `then` or
            // `else`, the expression is already well-structured — do not
            // restructure it.  We only restructure cases where `then` / `else`
            // sit inline with other tokens (often the result of a clumsy manual
            // line break).
            let already_structured = (i + 1..i + consumed).any(|j| {
                let start = lines[j].trim_start();
                start.starts_with("then ")
                    || start.starts_with("then\n")
                    || start == "then"
                    || start.starts_with("else ")
                    || start.starts_with("else\n")
                    || start == "else"
            });
            if already_structured {
                // Re-emit the original lines unchanged.
                for j in i..i + consumed {
                    result.push(lines[j].to_string());
                }
                i += consumed;
                continue;
            }

            // We have a multi-line candidate.  Try to restructure `if-then-else`.
            let restructured = restructure_if_expr(&full, line, indent_len, config);
            // If `restructure_if_expr` returned the unchanged first line, the
            // expression either didn't contain if-then-else or it was compact
            // enough.  In that case re-emit all the original lines.
            if restructured == line {
                for j in i..i + consumed {
                    result.push(lines[j].to_string());
                }
            } else {
                for rline in restructured.lines() {
                    result.push(rline.to_string());
                }
            }
            i += consumed;
        }

        result
    });
}

/// Given a (possibly multi-line) expression text and the first line's
/// information, attempt to restructure `if-then-else` into canonical form.
///
/// Returns the restructured string (which may be the same as the original
/// line if no `if-then-else` pattern was found).
fn restructure_if_expr(
    expr: &str,
    original_line: &str,
    indent_len: usize,
    config: &FormatConfig,
) -> String {
    // Find whole-word positions of `if`, `then`, `else` in the flattened
    // expression.
    let if_pos = find_keyword_pos(expr, "if", 0);
    let then_pos = if_pos.and_then(|ip| find_keyword_pos(expr, "then", ip + IF_LEN));
    let else_pos = then_pos.and_then(|tp| find_keyword_pos(expr, "else", tp + THEN_LEN));

    let (if_p, then_p, else_p) = match (if_pos, then_pos, else_pos) {
        (Some(a), Some(b), Some(c)) => (a, b, c),
        _ => {
            // Not a complete if-then-else — keep the original first line.
            return original_line.to_string();
        }
    };

    let base_indent = &original_line[..indent_len];
    let then_else_indent = format!("{}{}", base_indent, " ".repeat(config.indent_width));

    let prefix = expr[..if_p].trim_end().to_string();
    let condition = expr[if_p + IF_LEN + 1..then_p].trim();
    let then_body = expr[then_p + THEN_LEN + 1..else_p].trim();
    let else_body = expr[else_p + ELSE_LEN + 1..].trim();

    let mut out = String::new();

    // Line 1: <prefix> if <condition>
    if !prefix.is_empty() {
        out.push_str(&format!("{}{} if {}", base_indent, prefix, condition));
    } else {
        out.push_str(&format!("{}if {}", base_indent, condition));
    }

    // Line 2: <then-indent> then <then_body>
    out.push('\n');
    out.push_str(&format!("{}then {}", then_else_indent, then_body));

    // Line 3: <else-indent> else <else_body>
    out.push('\n');
    out.push_str(&format!("{}else {}", then_else_indent, else_body));

    out
}

/// Find a whole-word keyword (`kw`) in `s` starting from position `start`.
/// The keyword must be surrounded by non-identifier characters (or string
/// boundaries).  Skips string literals to avoid false matches inside `"…"`.
fn find_keyword_pos(s: &str, kw: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let kw_bytes = kw.as_bytes();
    let kw_len = kw_bytes.len();
    let mut i = start;

    while i + kw_len <= bytes.len() {
        // Skip string literals
        if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            continue;
        }

        if &bytes[i..i + kw_len] == kw_bytes {
            // Word boundary before
            let before = i == 0 || {
                let b = bytes[i - 1];
                !b.is_ascii_alphanumeric() && b != b'_' && b != b'\''
            };
            // Word boundary after
            let after = i + kw_len >= bytes.len() || {
                let b = bytes[i + kw_len];
                b == b' ' || b == b'\n' || b == b'\t' || b == b'('
            };
            if before && after {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// Post-processing: break long lines
// ---------------------------------------------------------------------------

/// Break lines that exceed `max_width` at safe syntactic boundaries.
///
/// This is a best-effort heuristic pass.  It operates on the already-
/// formatted text and attempts to insert line breaks after:
///
/// 1. `-> ` in type signatures,
/// 2. `= ` in bindings (when preceded by a name at line start),
/// 3. Binary operators surrounded by spaces,
/// 4. `, ` in parameter / argument lists.
///
/// Continuation lines are indented by one extra level.
fn break_long_lines(output: &mut String, config: &FormatConfig) {
    with_lines(output, |lines| {
        let mut result: Vec<String> = Vec::with_capacity(lines.len() * 2);

        for line in lines {
            // Skip comment lines — breaking inside a comment would turn the
            // continuation into code, which breaks compilation.
            let trimmed = line.trim_start();
            if trimmed.starts_with("--")
                || line.len() <= config.max_width
                || parse_field_line(line).is_some()
                || parse_binding_line(line).is_some()
                || parse_child_binding_line(line).is_some()
            {
                result.push(line.to_string());
                continue;
            }

            let mut remaining = line.to_string();
            let base_indent_len = leading_indent_len(line);
            let preferred_cont_indent = compute_continuation_indent(line, base_indent_len, config);

            while remaining.len() > config.max_width {
                let indent_len = leading_indent_len(&remaining);
                let indent = &remaining[..indent_len];
                // For the first break on the original line, align with the
                // expression start; for nested breaks add one more level.
                let continuation_indent = if remaining.as_str() == line {
                    preferred_cont_indent.clone()
                } else {
                    format!("{}{}", indent, " ".repeat(config.indent_width))
                };

                if let Some(break_at) = find_break_point(&remaining, config.max_width, indent_len) {
                    let before = remaining[..break_at].trim_end().to_string();
                    let after = remaining[break_at..].trim_start().to_string();
                    result.push(before);
                    remaining = format!("{}{}", continuation_indent, after);
                } else {
                    // No safe break point found — keep the line as-is
                    break;
                }
            }
            result.push(remaining);
        }

        result
    });
}

/// Return the byte length of the leading whitespace on `line`.
fn leading_indent_len(line: &str) -> usize {
    line.bytes()
        .take_while(|&b| b == b' ' || b == b'\t')
        .count()
}

/// Returns true when `s` contains a standalone `=` assignment (the line is a
/// `where` / `let` binding, not an expression continuation).  Excludes
/// comparison operators (`<=`, `>=`, `==`, `!=`) and arrows (`->`).
///
/// NOTE: This is a best-effort byte-level check; it does not handle `=` inside
/// parenthesised or bracketed expressions (e.g. `{a = b}`).  In practice such
/// lines are rare as standalone continuations, so false positives are unlikely.
fn has_standalone_equals(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if bytes[i] == b'=' {
            let before = i > 0 && bytes[i - 1] == b' ';
            let after = i + 1 < bytes.len() && bytes[i + 1] != b'=';
            if before && after {
                return true;
            }
            // `=` at start of line followed by space/end (e.g. `= x`)
            if i == 0 && (i + 1 >= bytes.len() || bytes[i + 1] == b' ') {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Compute the indentation string for a continuation line.
///
/// When the original line is a binding (`name = expr`) or type signature
/// (`name : Type`), the continuation is aligned with the expression that
/// follows `= ` or `: `.  For all other lines the continuation is indented by
/// one extra `indent_width` beyond the current leading whitespace.
fn compute_continuation_indent(line: &str, indent_len: usize, config: &FormatConfig) -> String {
    let bytes = line.as_bytes();
    let mut i = indent_len;
    while i + 2 <= bytes.len() {
        if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if &bytes[i..i + 2] == b"= " {
            // Not part of ==, <=, >=
            if bytes.get(i + 2) != Some(&b'=')
                && bytes.get(i.wrapping_sub(1)) != Some(&b'<')
                && bytes.get(i.wrapping_sub(1)) != Some(&b'>')
            {
                return " ".repeat(i + 2);
            }
        }
        if &bytes[i..i + 2] == b": " {
            return " ".repeat(i + 2);
        }
        i += 1;
    }

    let indent = &line[..indent_len];
    format!("{}{}", indent, " ".repeat(config.indent_width))
}

/// Find the best break point in `line` before `max_width`.
///
/// `indent_len` is the length of the leading indentation; break points
/// before the first non-whitespace character are ignored.
fn find_break_point(line: &str, max_width: usize, indent_len: usize) -> Option<usize> {
    // We search for break points in the range [indent_len, max_width].
    // The break point is the byte offset *after* the break character(s).
    let search_end = max_width.min(line.len());

    // Priority 1: break after `-> ` in type signatures (outside parens).
    if let Some(pos) = find_rightmost_arrow(line, indent_len, search_end) {
        return Some(pos + 3);
    }

    // Priority 2: break after `= ` in bindings (must be preceded by a name).
    if let Some(pos) = find_binding_eq_break(line, indent_len, search_end) {
        return Some(pos + 2);
    }

    // Priority 3: break after binary operators surrounded by spaces.
    if let Some(pos) = find_rightmost_binary_op(line, indent_len, search_end) {
        return Some(pos + 1);
    }

    // Priority 4: break after `, ` in lists.
    if let Some(pos) = find_rightmost_comma(line, indent_len, search_end) {
        return Some(pos + 2);
    }

    // Priority 5: break at space-separated application boundaries (depth 0 only).
    if let Some(pos) = find_rightmost_application_break(line, indent_len, search_end) {
        return Some(pos);
    }

    None
}

/// Find the rightmost `, ` in `line` within `[start, end]`, skipping string
/// literals.  Returns the start byte offset of the match.
fn find_rightmost_comma(line: &str, start: usize, end: usize) -> Option<usize> {
    let mut last = None;
    let bytes = line.as_bytes();
    let mut i = start;
    while i + 2 <= end {
        if bytes[i] == b'"' {
            // Skip string literal
            i += 1;
            while i < end && bytes[i] != b'"' {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if &bytes[i..i + 2] == b", " {
            last = Some(i);
            i += 2;
            continue;
        }
        i += 1;
    }
    last
}

/// Find a break point after `= ` that looks like a binding (preceded by a
/// name at the start of the line or after indentation).
fn find_binding_eq_break(line: &str, indent_len: usize, end: usize) -> Option<usize> {
    let mut last = None;
    let bytes = line.as_bytes();
    let mut i = indent_len;
    while i + 2 <= end {
        if bytes[i] == b'"' {
            // Skip string literal
            i += 1;
            while i < end && bytes[i] != b'"' {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if &bytes[i..i + 2] == b"= " {
            // Heuristic: the `=` must be preceded by an identifier character
            // and not be part of `==` or `>=` or `<=`.
            if i > indent_len
                && (bytes[i - 1].is_ascii_alphanumeric()
                    || bytes[i - 1] == b'_'
                    || bytes[i - 1] == b')'
                    || bytes[i - 1] == b']')
            {
                if bytes.get(i + 2).map_or(true, |&b| b != b'=') {
                    last = Some(i);
                }
            }
            i += 2;
            continue;
        }
        i += 1;
    }
    last
}

/// Find the rightmost binary operator surrounded by spaces, preferring
/// operators at shallower nesting depth.
fn find_rightmost_binary_op(line: &str, start: usize, end: usize) -> Option<usize> {
    // List of binary operators we consider safe to break after.
    // Multi-character operators like `&&`, `||`, `<<`, `>>` are handled
    // by their first character, but we ensure they're surrounded by spaces.
    const OPS: &[char] = &['+', '-', '*', '/', '&', '|', '<', '>'];

    // Track the best (rightmost) operator at each nesting depth.
    // We return the rightmost at the shallowest depth.
    let mut by_depth: Vec<Option<usize>> = Vec::new();
    let mut paren_depth: usize = 0;
    let bytes = line.as_bytes();
    let mut i = start;
    while i < end {
        let b = bytes[i];
        match b {
            b'(' => {
                paren_depth += 1;
                i += 1;
            }
            b')' => {
                paren_depth = paren_depth.saturating_sub(1);
                i += 1;
            }
            b'"' => {
                // Skip string literal
                i += 1;
                while i < end && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
            }
            _ => {
                let ch = b as char;
                if OPS.contains(&ch)
                    && bytes.get(i.wrapping_sub(1)).map_or(false, |&b| b == b' ')
                    && bytes.get(i + 1).map_or(false, |&b| b == b' ')
                {
                    // Reject `-` that looks like unary (preceded by `(` or `,` or `=`)
                    if ch == '-' {
                        if let Some(&prev) = bytes.get(i.wrapping_sub(2)) {
                            if matches!(prev, b'(' | b',' | b'=' | b'[' | b'-' | b'+') {
                                i += 1;
                                continue;
                            }
                        }
                    }
                    // Reject `>` that is part of `->`
                    if ch == '>' && bytes.get(i.wrapping_sub(1)) == Some(&b'-') {
                        i += 1;
                        continue;
                    }
                    // Reject `<` that is part of type params `Vec<`
                    if ch == '<' {
                        i += 1;
                        continue;
                    }
                    if by_depth.len() <= paren_depth {
                        by_depth.resize(paren_depth + 1, None);
                    }
                    by_depth[paren_depth] = Some(i);
                }
                i += 1;
            }
        }
    }
    by_depth.iter().find_map(|&opt| opt)
}

/// Find the rightmost `-> ` outside of parenthesized groups and strings.
///
/// Tracks nesting depth and prefers breaks at the shallowest depth.  This
/// prevents breaking inside `(A -> B) -> C` at the inner arrow.
fn find_rightmost_arrow(line: &str, start: usize, end: usize) -> Option<usize> {
    let mut by_depth: Vec<Option<usize>> = Vec::new();
    let mut paren_depth: usize = 0;
    let bytes = line.as_bytes();
    let mut i = start;
    while i + 3 <= end {
        let b = bytes[i];
        match b {
            b'(' => {
                paren_depth += 1;
                i += 1;
            }
            b')' => {
                paren_depth = paren_depth.saturating_sub(1);
                i += 1;
            }
            b'"' => {
                // Skip string literal
                i += 1;
                while i < end && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
            }
            _ => {
                if &bytes[i..(i + 3).min(end)] == b"-> " {
                    if by_depth.len() <= paren_depth {
                        by_depth.resize(paren_depth + 1, None);
                    }
                    by_depth[paren_depth] = Some(i);
                    i += 3;
                } else {
                    i += 1;
                }
            }
        }
    }
    by_depth.iter().find_map(|&opt| opt)
}

/// Find a break point after a space between two word-like tokens (ident or
/// numeric).  This handles long function application chains that have no
/// infix operators, arrows, or commas.
fn find_rightmost_application_break(line: &str, start: usize, end: usize) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut paren_depth: usize = 0;
    let bytes = line.as_bytes();
    let mut i = start;
    while i < end {
        let b = bytes[i];
        match b {
            b'(' => {
                paren_depth += 1;
                i += 1;
            }
            b')' => {
                paren_depth = paren_depth.saturating_sub(1);
                i += 1;
            }
            b'"' => {
                i += 1;
                while i < end && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
            }
            b' ' => {
                let prev_word = i > start && bytes[i - 1].is_ascii_alphanumeric();
                let next_word = i + 1 < end && bytes[i + 1].is_ascii_alphanumeric();
                if paren_depth == 0 && prev_word && next_word {
                    best = Some(i + 1);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Post-processing: collapse flat bindings into group blocks
// ---------------------------------------------------------------------------

/// Parsed structure of a single binding line (after the `@group` header).
struct BindingLineInfo<'a> {
    /// The raw text of the line including leading indent.
    raw_line: &'a str,
    /// Byte offset where leading whitespace ends.
    indent: usize,
    /// `@binding(N)` text.
    binding: &'a str,
    /// Attribute text including trailing space, or empty.
    attrs: &'a str,
    /// `uniform` or `storage(...)` text.
    keyword: &'a str,
    /// The binding name.
    name: &'a str,
    /// Everything from the colon to the end (`: Type`).
    suffix: &'a str,
}

/// Parse the portion of a binding line that follows `@group(N)`.
///
/// Input: `@binding(N) [@attr(...)] uniform/storage name : Type`
/// Returns `Some(BindingLineInfo)` on success.
fn parse_binding_rest(raw_line: &str, indent: usize) -> Option<BindingLineInfo<'_>> {
    let trimmed = raw_line[indent..].trim_start();
    if !trimmed.starts_with("@binding(") {
        return None;
    }

    let (binding, mut pos) = parse_binding_prefix(trimmed)?;
    let attrs = parse_binding_attrs(trimmed, &mut pos);
    let keyword = parse_keyword(trimmed, &mut pos)?;
    let (name, suffix) = parse_name_and_suffix(trimmed, &mut pos)?;

    Some(BindingLineInfo {
        raw_line,
        indent,
        binding,
        attrs,
        keyword,
        name,
        suffix,
    })
}

/// Parse `@binding(N)` from the start of `trimmed`.
fn parse_binding_prefix(trimmed: &str) -> Option<(&str, usize)> {
    let bclose = trimmed[9..].find(')')?;
    let binding = &trimmed[..9 + bclose + 1];
    Some((binding, binding.len()))
}

/// Parse optional `@attr(...)` entries after `@binding(N)`.
fn parse_binding_attrs<'a>(trimmed: &'a str, pos: &mut usize) -> &'a str {
    let attr_start = *pos;
    while let Some(rest) = trimmed.get(*pos..) {
        let rest_trimmed = rest.trim_start();
        *pos += rest.len() - rest_trimmed.len();
        if !rest_trimmed.starts_with('@') {
            break;
        }
        let attr_len = skip_attr(rest_trimmed);
        if attr_len == 0 {
            break;
        }
        *pos += attr_len;
    }
    trimmed[attr_start..*pos].trim_end()
}

/// Return the byte length of one `@ident` or `@ident(args)` token, or 0.
fn skip_attr(s: &str) -> usize {
    let mut p = 1;
    while p < s.len() && (s.as_bytes()[p].is_ascii_alphanumeric() || s.as_bytes()[p] == b'_') {
        p += 1;
    }
    if p == 1 {
        return 0;
    }
    if p < s.len() && s.as_bytes()[p] == b'(' {
        let Some(close) = s[p..].find(')') else {
            return 0;
        };
        p += close + 1;
    }
    p
}

/// Split an attribute string like `@foo @bar(args)` into individual tokens.
fn split_attrs(s: &str) -> Vec<&str> {
    let mut attrs = Vec::new();
    let mut pos = 0;
    while pos < s.len() {
        let rest = &s[pos..];
        let rest_trimmed = rest.trim_start();
        pos += rest.len() - rest_trimmed.len();
        if rest_trimmed.is_empty() {
            break;
        }
        let len = skip_attr(rest_trimmed);
        if len == 0 {
            break;
        }
        attrs.push(&rest_trimmed[..len]);
        pos += len;
    }
    attrs
}

/// Parse an optional keyword (`uniform` or `storage(...)`) from a binding.
fn parse_keyword<'a>(trimmed: &'a str, pos: &mut usize) -> Option<&'a str> {
    let after_attrs = trimmed.get(*pos..)?.trim_start();
    *pos += trimmed.get(*pos..)?.len() - after_attrs.len();

    let keyword = if after_attrs.starts_with("uniform") {
        *pos += "uniform".len();
        &trimmed[*pos - "uniform".len()..*pos]
    } else if after_attrs.starts_with("storage(") {
        let close = after_attrs.find(')')?;
        *pos += close + 1;
        &trimmed[*pos - (close + 1)..*pos]
    } else if after_attrs.starts_with("storage") {
        *pos += "storage".len();
        &trimmed[*pos - "storage".len()..*pos]
    } else {
        ""
    };

    Some(keyword)
}

/// Parse the binding name and `: Type` suffix.
fn parse_name_and_suffix<'a>(trimmed: &'a str, pos: &mut usize) -> Option<(&'a str, &'a str)> {
    let after_kw = trimmed.get(*pos..)?.trim_start();
    *pos += trimmed.get(*pos..)?.len() - after_kw.len();

    let name_end = after_kw.find([' ', ':', '\t']).unwrap_or(after_kw.len());
    let name = &trimmed[*pos..*pos + name_end];
    *pos += name_end;

    let after_name = trimmed.get(*pos..)?.trim_start();
    *pos += trimmed.get(*pos..)?.len() - after_name.len();

    if !after_name.starts_with(':') {
        return None;
    }
    let suffix = &trimmed[*pos..];

    Some((name, suffix))
}

/// Detect consecutive binding declaration lines that share the same `@group(N)` and
/// collapse them into the group block sugar:
///
/// ```text
/// @group(0)
///   @binding(0) uniform frame  : FrameData
///   @binding(1) uniform params : Params
/// ```
///
/// Lines that are the only binding in their group are left as flat declarations.
/// Already-indented binding lines (inside `when` blocks) are handled by checking
/// the leading indentation of the original `@group` line.
///
/// Also re-aligns existing group blocks where `@group(N)` is already on its own
/// line followed by indented `@binding(N) ...` children.
fn collapse_binding_group_blocks(output: &mut String, config: &FormatConfig) {
    with_lines(output, |lines| {
        let mut result: Vec<String> = Vec::with_capacity(lines.len());
        let mut i = 0;

        while i < lines.len() {
            // Case 1: flat binding lines — collapse consecutive same-group lines
            if let Some((indent, group_val, binding_rest)) = parse_binding_line(lines[i]) {
                let mut group = vec![(indent, group_val, binding_rest, i)];
                let mut j = i + 1;
                while j < lines.len() {
                    if let Some((ind2, gv2, br2)) = parse_binding_line(lines[j]) {
                        if ind2 == indent && gv2 == group_val {
                            group.push((ind2, gv2, br2, j));
                            j += 1;
                            continue;
                        }
                    }
                    break;
                }

                if group.len() > 1 {
                    let indent_str = &lines[group[0].3][..indent];
                    result.push(format!("{}@group({})", indent_str, group_val));
                    let child_indent = format!("{}{}", indent_str, " ".repeat(config.indent_width));

                    let mut infos: Vec<BindingLineInfo> = Vec::new();
                    for &(_, _, rest, _) in &group {
                        if let Some(info) = parse_binding_rest(rest, indent) {
                            infos.push(info);
                        }
                    }

                    emit_aligned_bindings(&infos, &child_indent, config, &mut result);
                    i = j;
                } else {
                    result.push(lines[i].to_string());
                    i += 1;
                }
            }
            // Case 2: existing group block header — `@group(N)` on its own line
            else if let Some((header_indent, _group_val)) = parse_group_header_line(lines[i]) {
                let expected_child_indent = header_indent + config.indent_width;
                result.push(lines[i].to_string());
                i += 1;

                let mut children: Vec<BindingLineInfo> = Vec::new();
                while i < lines.len() {
                    if let Some(info) = parse_child_binding_line(lines[i]) {
                        if info.indent >= expected_child_indent {
                            children.push(info);
                            i += 1;
                            continue;
                        }
                    }
                    break;
                }

                if children.len() > 1 {
                    let child_indent_str = format!(
                        "{}{}",
                        " ".repeat(header_indent),
                        " ".repeat(config.indent_width)
                    );
                    emit_aligned_bindings(&children, &child_indent_str, config, &mut result);
                } else {
                    for child in &children {
                        result.push(child.raw_line.to_string());
                    }
                }
            } else {
                result.push(lines[i].to_string());
                i += 1;
            }
        }

        result
    });
}

/// Emit aligned binding lines.
///
/// Aligns `@binding(N)`, attributes, keyword, name, and `: Type` across the
/// group.  When a line would be too long or an attribute exceeds the
/// threshold, the whole group falls back to Style B (each attribute and the
/// name on separate indented lines) provided every line still fits inside
/// `max_width`.  Otherwise it falls back to unaligned inline emission.
fn emit_aligned_bindings(
    infos: &[BindingLineInfo],
    child_indent: &str,
    config: &FormatConfig,
    result: &mut Vec<String>,
) {
    if infos.is_empty() {
        return;
    }

    let has_attrs = infos.iter().any(|i| !i.attrs.trim().is_empty());
    let max_binding = infos.iter().map(|i| i.binding.len()).max().unwrap();
    let max_attrs = infos.iter().map(|i| i.attrs.trim().len()).max().unwrap();
    let max_keyword = infos.iter().map(|i| i.keyword.len()).max().unwrap();
    let max_name = infos.iter().map(|i| i.name.len()).max().unwrap();

    // Check whether the aligned Style A layout would exceed max_width.
    let style_a_fits = if config.enforce_max_width {
        infos.iter().all(|info| {
            let aligned_len = if has_attrs {
                child_indent.len()
                    + max_binding
                    + 1
                    + max_attrs
                    + 1
                    + max_keyword
                    + 1
                    + max_name
                    + 1
                    + info.suffix.len()
            } else {
                let max_prefix_len = max_binding + 1 + max_keyword;
                child_indent.len() + max_prefix_len + 1 + max_name + 1 + info.suffix.len()
            };
            aligned_len <= config.max_width
        })
    } else {
        true
    };

    // Decide whether to use Style B for the whole group.
    let use_style_b = match config.binding_attribute_style {
        AttributeStyle::OwnLine => has_attrs,
        AttributeStyle::Inline => false,
        AttributeStyle::Auto => {
            if !has_attrs {
                false
            } else if !style_a_fits {
                true
            } else {
                infos
                    .iter()
                    .any(|info| has_long_attribute(info.attrs.trim(), config.attribute_threshold))
            }
        }
    };

    if use_style_b {
        let style_b_viable = infos.iter().all(|info| {
            let name_line_len = child_indent.len()
                + config.indent_width
                + (if info.keyword.is_empty() {
                    0
                } else {
                    info.keyword.len() + 1
                })
                + info.name.len()
                + 1
                + info.suffix.len();
            if name_line_len > config.max_width {
                return false;
            }
            for attr in split_attrs(info.attrs.trim()) {
                let attr_line_len = child_indent.len() + config.indent_width + attr.len();
                if attr_line_len > config.max_width {
                    return false;
                }
            }
            true
        });

        let force_style_b =
            config.binding_attribute_style == AttributeStyle::OwnLine || !config.enforce_max_width;

        if style_b_viable || force_style_b {
            let attr_indent = format!("{}{}", child_indent, " ".repeat(config.indent_width));
            for info in infos {
                result.push(format!("{}{}", child_indent, info.binding));
                for attr in split_attrs(info.attrs.trim()) {
                    result.push(format!("{}{}", attr_indent, attr));
                }
                let mut line = String::new();
                line.push_str(&attr_indent);
                if !info.keyword.is_empty() {
                    line.push_str(info.keyword);
                    line.push(' ');
                }
                line.push_str(info.name);
                line.push(' ');
                line.push_str(info.suffix);
                result.push(line);
            }
            return;
        }
    }

    if !style_a_fits {
        // Fall back to unaligned emission: each binding gets standard indentation
        // with a single space between each token (no inter-binding column alignment).
        for info in infos {
            let attrs_trim = info.attrs.trim();
            let mut line = String::new();
            line.push_str(child_indent);
            line.push_str(info.binding);
            if !attrs_trim.is_empty() {
                line.push(' ');
                line.push_str(attrs_trim);
            }
            if !info.keyword.is_empty() {
                line.push(' ');
                line.push_str(info.keyword);
            }
            line.push(' ');
            line.push_str(info.name);
            line.push(' ');
            line.push_str(info.suffix);
            result.push(line);
        }
        return;
    }

    for info in infos {
        if has_attrs {
            // Style A with attribute alignment
            let attrs_trim = info.attrs.trim();
            let binding_pad = max_binding - info.binding.len();
            let attr_pad = max_attrs - attrs_trim.len();
            let keyword_pad = max_keyword - info.keyword.len();
            let name_pad = max_name - info.name.len();

            let mut line = String::with_capacity(
                child_indent.len()
                    + info.binding.len()
                    + attrs_trim.len()
                    + info.keyword.len()
                    + info.name.len()
                    + info.suffix.len()
                    + 8,
            );
            line.push_str(child_indent);
            line.push_str(info.binding);
            line.push_str(" ".repeat(binding_pad + 1).as_str());
            line.push_str(attrs_trim);
            line.push_str(" ".repeat(attr_pad + 1).as_str());
            line.push_str(info.keyword);
            line.push_str(" ".repeat(keyword_pad + 1).as_str());
            line.push_str(info.name);
            line.push_str(" ".repeat(name_pad + 1).as_str());
            line.push_str(info.suffix);
            result.push(line);
        } else {
            // No attributes in the group — use the simpler prefix/name alignment
            // (preserves the exact output of the original formatter)
            let prefix = format!("{} {}", info.binding, info.keyword);
            let max_prefix_len = max_binding + 1 + max_keyword;
            let prefix_pad = max_prefix_len - prefix.len() + 1;
            let name_pad = max_name - info.name.len() + 1;
            let mut line = String::new();
            line.push_str(child_indent);
            line.push_str(&prefix);
            line.push_str(&" ".repeat(prefix_pad));
            line.push_str(info.name);
            line.push_str(&" ".repeat(name_pad));
            line.push_str(info.suffix);
            result.push(line);
        }
    }
}

/// Parse a standalone group header line: `<indent>@group(N)` with nothing after it.
/// Returns `(indent_len, group_value_str)`.
fn parse_group_header_line(line: &str) -> Option<(usize, &str)> {
    let bytes = line.as_bytes();
    let indent = bytes
        .iter()
        .take_while(|&&b| b == b' ' || b == b'\t')
        .count();
    let trimmed = line[indent..].trim_end();

    if !trimmed.starts_with("@group(") {
        return None;
    }
    let after_open = &trimmed[7..];
    let close = after_open.find(')')?;
    let group_val = &after_open[..close];
    let remainder = after_open[close + 1..].trim();
    if !remainder.is_empty() {
        return None;
    }
    Some((indent, group_val))
}

/// Parse an indented child binding line.
/// Returns `Some(BindingLineInfo)` when the line matches a binding declaration.
fn parse_child_binding_line(line: &str) -> Option<BindingLineInfo<'_>> {
    let bytes = line.as_bytes();
    let indent = bytes
        .iter()
        .take_while(|&&b| b == b' ' || b == b'\t')
        .count();
    if indent == 0 {
        return None;
    }
    parse_binding_rest(line, indent)
}

/// Try to parse a line as a flat binding declaration.
/// Returns `(indent_len, group_value_str, rest_after_group)`.
/// Matches: `<indent>@group(N) @binding(N) [@attr...] uniform/storage ...`
fn parse_binding_line(line: &str) -> Option<(usize, &str, &str)> {
    let bytes = line.as_bytes();
    let indent = bytes
        .iter()
        .take_while(|&&b| b == b' ' || b == b'\t')
        .count();
    let trimmed = &line[indent..];

    if !trimmed.starts_with("@group(") {
        return None;
    }
    let after_group_open = &trimmed[7..];
    let close = after_group_open.find(')')?;
    let group_val = &after_group_open[..close];
    let after_group = &after_group_open[close + 1..];

    let after_ws = after_group.trim_start();
    if !after_ws.starts_with("@binding(") {
        return None;
    }

    // Validate the rest is a proper binding by attempting to parse it
    let rest_indent = line.len() - after_ws.len();
    parse_binding_rest(line, rest_indent)?;

    Some((indent, group_val, after_ws))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_preserves_simple_program() {
        let source = "double x = x * 2\n";
        let result = format_default(source);
        assert_eq!(result, "double x = x * 2\n");
    }

    #[test]
    fn format_normalizes_trailing_whitespace() {
        let source = "f x = x   \n";
        let result = format_default(source);
        assert_eq!(result, "f x = x\n");
    }

    #[test]
    fn format_ensures_final_newline() {
        let source = "f x = x";
        let result = format_default(source);
        assert_eq!(result, "f x = x\n");
    }

    #[test]
    fn format_collapses_multiple_blank_lines() {
        let source = "f x = x\n\n\n\ng y = y\n";
        let result = format_default(source);
        assert_eq!(result, "f x = x\n\ng y = y\n");
    }

    #[test]
    fn format_preserves_comments() {
        let source = "-- this is a comment\nf x = x\n";
        let result = format_default(source);
        assert_eq!(result, "-- this is a comment\nf x = x\n");
    }

    #[test]
    fn format_preserves_indentation() {
        let source = "main x =\n  let y = x\n  in y + 1\n";
        let result = format_default(source);
        assert_eq!(result, "main x =\n  let y = x\n  in y + 1\n");
    }

    #[test]
    fn format_normalizes_extra_spaces() {
        let source = "f   x   =   x  +  1\n";
        let result = format_default(source);
        assert_eq!(result, "f x = x + 1\n");
    }

    #[test]
    fn format_no_space_inside_parens() {
        let source = "f ( x ) = ( x + 1 )\n";
        let result = format_default(source);
        assert_eq!(result, "f (x) = (x + 1)\n");
    }

    #[test]
    fn format_preserves_data_decl() {
        let source = "data Color = Red | Green | Blue\n";
        let result = format_default(source);
        assert_eq!(result, "data Color = Red | Green | Blue\n");
    }

    #[test]
    fn format_preserves_type_sig() {
        let source = "add : I32 -> I32 -> I32\n";
        let result = format_default(source);
        assert_eq!(result, "add : I32 -> I32 -> I32\n");
    }

    #[test]
    fn format_no_space_around_dot() {
        let source = "f x = x . y\n";
        let result = format_default(source);
        assert_eq!(result, "f x = x.y\n");
    }

    #[test]
    fn format_field_access_in_expr() {
        let source = "f p = p.x + p.y\n";
        let result = format_default(source);
        assert_eq!(result, "f p = p.x + p.y\n");
    }

    #[test]
    fn format_attribute_no_space() {
        let source = "@ compute\n";
        let result = format_default(source);
        assert_eq!(result, "@compute\n");
    }

    #[test]
    fn format_const_attribute() {
        let source = "@ const\npi : F32\npi = 3.14159\n";
        let result = format_default(source);
        assert_eq!(result, "@const\npi : F32\npi = 3.14159\n");
    }

    #[test]
    fn format_idempotent() {
        let source = "-- Example\nf x = x + 1\n\ng : I32 -> I32\ng y = y * 2\n";
        let first = format_default(source);
        let second = format_default(&first);
        assert_eq!(first, second, "formatter is not idempotent");
    }

    #[test]
    fn format_list_literal() {
        let source = "v = [ 1.0 , 2.0 , 3.0 ]\n";
        let result = format_default(source);
        assert_eq!(result, "v = [1.0, 2.0, 3.0]\n");
    }

    #[test]
    fn format_record() {
        let source = "p = Particle { x = 1.0 , y = 2.0 }\n";
        let result = format_default(source);
        assert_eq!(result, "p = Particle { x = 1.0, y = 2.0 }\n");
    }

    #[test]
    fn format_hello_example() {
        let source = "-- Minimal end-to-end example.\n-- Demonstrates arithmetic, function calls, and let-bindings.\n\ndouble x = x * 2\n\nmain x =\n  let y = double x\n  in y + 1\n";
        let result = format_default(source);
        assert_eq!(result, source);
    }

    #[test]
    fn format_aligns_record_field_colons() {
        let source =
            "data Particle = Particle {\n  x : F32,\n  y : F32,\n  vx : F32,\n  vy : F32,\n}\n";
        let expected =
            "data Particle = Particle {\n  x  : F32,\n  y  : F32,\n  vx : F32,\n  vy : F32,\n}\n";
        let result = format_default(source);
        assert_eq!(result, expected);
    }

    #[test]
    fn format_field_alignment_idempotent() {
        let source =
            "data Particle = Particle {\n  x  : F32,\n  y  : F32,\n  vx : F32,\n  vy : F32,\n}\n";
        let result = format_default(source);
        let result2 = format_default(&result);
        assert_eq!(result, result2, "field alignment is not idempotent");
    }

    #[test]
    fn format_record_update_idempotent() {
        let source = "nudgeX dx p = p { x = p.x + dx }\n";
        let result = format_default(source);
        assert_eq!(result, source);
    }

    #[test]
    fn format_type_params_no_spaces() {
        let source = "f : Vec < 4 , F32 > -> F32\n";
        let result = format_default(source);
        assert_eq!(result, "f : Vec<4, F32> -> F32\n");
    }

    #[test]
    fn format_nested_type_params() {
        let source = "f : Array < Vec < 3 , F32 > , 64 > -> F32\n";
        let result = format_default(source);
        assert_eq!(result, "f : Array<Vec<3, F32>, 64> -> F32\n");
    }

    #[test]
    fn format_type_params_idempotent() {
        let source = "f : Vec<4, F32> -> F32\n";
        let result = format_default(source);
        let result2 = format_default(&result);
        assert_eq!(result, result2, "type params formatting is not idempotent");
    }

    #[test]
    fn format_attribute_args_no_space() {
        let source = "@builtin(position) foo : Vec<4, F32>\n";
        let result = format_default(source);
        assert_eq!(result, "@builtin(position) foo : Vec<4, F32>\n");
    }

    #[test]
    fn format_workgroup_size_no_space() {
        let source = "@compute @workgroup_size(64, 1, 1)\n";
        let result = format_default(source);
        assert_eq!(result, "@compute @workgroup_size(64, 1, 1)\n");
    }

    #[test]
    fn format_array_index_no_space() {
        let source = "f x = buf[idx]\n";
        let result = format_default(source);
        assert_eq!(result, "f x = buf[idx]\n");
    }

    #[test]
    fn format_vec_literal_argument_preserves_space() {
        let source = "f x [1.0, 2.0] [3.0, 4.0]\n";
        let result = format_default(source);
        assert_eq!(result, "f x [1.0, 2.0] [3.0, 4.0]\n");
    }

    #[test]
    fn format_array_index_still_no_space() {
        let source = "buf[i]\n";
        let result = format_default(source);
        assert_eq!(result, "buf[i]\n");
    }

    #[test]
    fn format_vec_literal_after_paren_preserves_space() {
        let source = "f (g x) [1.0, 2.0]\n";
        let result = format_default(source);
        assert_eq!(result, "f (g x) [1.0, 2.0]\n");
    }

    #[test]
    fn format_unary_negation_no_space() {
        let source = "f x = (-x)\n";
        let result = format_default(source);
        assert_eq!(result, "f x = (-x)\n");
    }

    #[test]
    fn format_unary_not_no_space() {
        let source = "f x = !x\n";
        let result = format_default(source);
        assert_eq!(result, "f x = !x\n");
    }

    #[test]
    fn format_binary_minus_has_space() {
        let source = "f x = x - 1\n";
        let result = format_default(source);
        assert_eq!(result, "f x = x - 1\n");
    }

    #[test]
    fn format_lambda_no_space_after_backslash() {
        let source = "f = (\\x -> x + 1)\n";
        let result = format_default(source);
        assert_eq!(result, "f = (\\x -> x + 1)\n");
    }

    #[test]
    fn format_negative_literal() {
        let source = "f = Fp64 (-a.high) (-a.low)\n";
        let result = format_default(source);
        assert_eq!(result, "f = Fp64 (-a.high) (-a.low)\n");
    }

    #[test]
    fn format_match_negation() {
        let source = "  | 4 | 8 -> -1.0\n";
        let result = format_default(source);
        assert_eq!(result, "  | 4 | 8 -> -1.0\n");
    }

    #[test]
    fn format_multiple_attributes() {
        let source = "  @location(4) @interpolate(flat) cap_type : U32\n";
        let result = format_default(source);
        assert_eq!(result, "  @location(4) @interpolate(flat) cap_type : U32\n");
    }

    #[test]
    fn format_preserves_let_binding_alignment() {
        let source = "  let x    = 1\n      yLong = 2\n";
        let result = format_default(source);
        assert_eq!(result, "  let x    = 1\n      yLong = 2\n");
    }

    #[test]
    fn format_preserves_const_colon_alignment() {
        let source = "const FOO      : I32 = 1\nconst BAR_LONG : I32 = 2\n";
        let result = format_default(source);
        assert_eq!(
            result,
            "const FOO      : I32 = 1\nconst BAR_LONG : I32 = 2\n"
        );
    }

    #[test]
    fn format_preserves_match_arrow_alignment() {
        let source = "  | Red   -> 1\n  | Green -> 2\n";
        let result = format_default(source);
        assert_eq!(result, "  | Red   -> 1\n  | Green -> 2\n");
    }

    #[test]
    fn format_collapses_binding_group_block() {
        let source = "@group(0) @binding(0) uniform frame : FrameData\n@group(0) @binding(1) uniform capFlags : CapFlags\n";
        let result = format_default(source);
        assert_eq!(
            result,
            "@group(0)\n  @binding(0) uniform frame    : FrameData\n  @binding(1) uniform capFlags : CapFlags\n"
        );
    }

    #[test]
    fn format_single_binding_stays_flat() {
        let source = "@group(2) @binding(0) uniform params : Params\n";
        let result = format_default(source);
        assert_eq!(result, "@group(2) @binding(0) uniform params : Params\n");
    }

    #[test]
    fn format_mixed_groups_collapsed_separately() {
        let source = "@group(0) @binding(0) uniform frame : FrameData\n@group(0) @binding(1) uniform capFlags : CapFlags\n\n@group(1) @binding(0) storage prims : Array<Prim>\n";
        let result = format_default(source);
        assert_eq!(
            result,
            "@group(0)\n  @binding(0) uniform frame    : FrameData\n  @binding(1) uniform capFlags : CapFlags\n\n@group(1) @binding(0) storage prims : Array<Prim>\n"
        );
    }

    #[test]
    fn format_group_block_idempotent() {
        let source = "@group(0)\n  @binding(0) uniform frame    : FrameData\n  @binding(1) uniform capFlags : CapFlags\n";
        let result = format_default(source);
        let result2 = format_default(&result);
        assert_eq!(result, result2, "group block formatting is not idempotent");
    }

    #[test]
    fn format_preserves_group_block_storage_rw() {
        let source = "@group(0) @binding(0) uniform frame : FrameData\n@group(0) @binding(1) storage(read_write) output : Array<F32, 64>\n";
        let result = format_default(source);
        assert_eq!(
            result,
            "@group(0)\n  @binding(0) uniform             frame  : FrameData\n  @binding(1) storage(read_write) output : Array<F32, 64>\n"
        );
    }

    #[test]
    fn format_preserves_record_field_attr_padding() {
        let source = "  @builtin(position)              clip_pos : Vec<4, F32>,\n  @location(0)                    dist     : F32,\n";
        let result = format_default(source);
        assert_eq!(result, "  @builtin(position)              clip_pos : Vec<4, F32>,\n  @location(0)                    dist     : F32,\n");
    }

    #[test]
    fn format_normalizes_toplevel_equals() {
        // Top-level function definitions should NOT preserve alignment padding
        let source = "f   x   =   x + 1\n";
        let result = format_default(source);
        assert_eq!(result, "f x = x + 1\n");
    }

    #[test]
    fn format_unary_not_after_or() {
        let source = "  x = !a || !b\n";
        let result = format_default(source);
        assert_eq!(result, "  x = !a || !b\n");
    }

    #[test]
    fn format_alignment_idempotent_let_block() {
        let source = "  let x    = 1\n      yLong = 2\n";
        let first = format_default(source);
        let second = format_default(&first);
        assert_eq!(first, second, "let binding alignment is not idempotent");
    }

    #[test]
    fn format_bitwise_not_no_space() {
        let source = "f x = ~x\n";
        let result = format_default(source);
        assert_eq!(result, "f x = ~x\n");
    }

    #[test]
    fn format_bitwise_not_in_parens() {
        let source = "f x m = (x & (~m))\n";
        let result = format_default(source);
        assert_eq!(result, "f x m = (x & (~m))\n");
    }

    #[test]
    fn format_shift_right_no_space() {
        let source = "f x y = x >> y\n";
        let result = format_default(source);
        assert_eq!(result, "f x y = x >> y\n");
    }

    #[test]
    fn format_shift_right_in_parens() {
        let source = "f v o m = (v >> o) & m\n";
        let result = format_default(source);
        assert_eq!(result, "f v o m = (v >> o) & m\n");
    }

    #[test]
    fn format_breaks_long_type_signature() {
        let mut config = FormatConfig::default();
        config.max_width = 60;
        let source =
            "sceneDist : Shape -> Shape -> Shape -> Shape -> Shape -> Vec2f -> F32 -> F32\n";
        let result = format(source, &config);
        assert!(
            result.lines().all(|l| l.len() <= 60),
            "line exceeded max_width: {:?}",
            result
        );
        assert!(
            result.contains("->\n"),
            "should break at arrow: {:?}",
            result
        );
    }

    #[test]
    fn format_breaks_long_binding() {
        let mut config = FormatConfig::default();
        config.max_width = 60;
        let source = "light1Fall = if light1Ld > 0.6 then 0.0 else ((0.6 - light1Ld) / 0.6) * ((0.6 - light1Ld) / 0.6)\n";
        let result = format(source, &config);
        assert!(
            result.lines().all(|l| l.len() <= 60),
            "line exceeded max_width: {:?}",
            result
        );
        assert!(
            result.contains(">\n") || result.contains("*\n"),
            "should break at operator: {:?}",
            result
        );
    }

    #[test]
    fn format_breaks_long_binary_chain() {
        let mut config = FormatConfig::default();
        config.max_width = 50;
        let source =
            "litCol = bgCol + light1 + light2 + light3 + ambient + emission + specular + diffuse\n";
        let result = format(source, &config);
        assert!(
            result.lines().all(|l| l.len() <= 50),
            "line exceeded max_width: {:?}",
            result
        );
        assert!(
            result.contains("+\n"),
            "should break at operator: {:?}",
            result
        );
    }

    #[test]
    fn format_record_field_attribute_alignment() {
        let source = "data VertexOutput = VertexOutput {\n  @builtin(position) clip_pos : Vec<4, F32>,\n  @location(0) uv : Vec<2, F32>,\n}\n";
        let result = format_default(source);
        // Attributes and names should be aligned; colons aligned.
        assert!(result.contains("@builtin(position) clip_pos : Vec<4, F32>,"));
        assert!(result.contains("@location(0)       uv       : Vec<2, F32>,"));
    }

    #[test]
    fn format_record_field_fallback_to_own_line() {
        let source = "data T = T {\n  @textureSampleType(filterable = false) tex : Texture2d F32,\n  @location(0) uv : Vec<2, F32>,\n}\n";
        let result = format_default(source);
        assert!(
            result.lines().all(|l| l.len() <= 100),
            "line exceeded max_width: {:?}",
            result
        );
        assert!(
            result.contains("@textureSampleType(filterable = false)\n"),
            "should fallback to own line: {:?}",
            result
        );
    }

    #[test]
    fn format_binding_with_attribute_alignment() {
        let source = "@group(0) @binding(0) uniform frame : FrameData\n@group(0) @binding(1) @textureSampleType(filterable = false) textTexture : Texture2d F32\n";
        let result = format_default(source);
        assert!(
            result.lines().all(|l| l.len() <= 100),
            "line exceeded max_width: {:?}",
            result
        );
        assert!(
            result.contains("@group(0)\n"),
            "should collapse group: {:?}",
            result
        );
    }

    #[test]
    fn format_binding_fallback_to_own_line() {
        let source = "@group(0) @binding(0) uniform frame : FrameData\n@group(0) @binding(1) @textureSampleType(filterable = false) textTexture : Texture2d F32\n";
        let result = format_default(source);
        assert!(
            result.lines().all(|l| l.len() <= 100),
            "line exceeded max_width: {:?}",
            result
        );
        assert!(
            result.contains("@textureSampleType(filterable = false)\n"),
            "should fallback to own line: {:?}",
            result
        );
    }

    #[test]
    fn format_binding_style_b_when_max_width_exceeded() {
        let mut config = FormatConfig::default();
        config.max_width = 80;
        config.enforce_max_width = true;
        let source = "@group(1) @binding(0) mainTexture : Texture2d F32\n@group(1) @binding(1) @samplerState(filter = \"linear\", address_mode_u = \"repeat\") mainSampler : Sampler\n";
        let result = format(source, &config);
        assert!(
            result.lines().all(|l| l.len() <= 80),
            "line exceeded max_width: {:?}",
            result
        );
        assert!(
            result.contains("@samplerState(filter = \"linear\", address_mode_u = \"repeat\")\n"),
            "should place attribute on own line: {:?}",
            result
        );
        assert!(
            result.contains("@group(1)\n"),
            "should collapse group: {:?}",
            result
        );
    }

    #[test]
    fn format_config_override_width() {
        let mut config = FormatConfig::default();
        config.max_width = 40;
        config.enforce_max_width = true;
        let source = "veryLongFunctionName : I32 -> I32 -> I32 -> I32\n";
        let result = format(source, &config);
        assert!(
            result.lines().all(|l| l.len() <= 40),
            "line exceeded custom max_width: {:?}",
            result
        );
    }

    #[test]
    fn format_does_not_break_inside_string_literal() {
        // Even when a line exceeds max_width, the formatter must never break
        // inside a string literal.  In this example the only break points are
        // inside the string (", ->, etc.), so the line is kept intact.
        let mut config = FormatConfig::default();
        config.max_width = 50;
        let source = "f = someFunc \"a very long string, with commas, and arrows -> \" 1.0\n";
        let result = format(source, &config);
        assert!(
            result.contains("\"a very long string, with commas, and arrows -> \""),
            "should not break inside string literal: {:?}",
            result
        );
    }

    #[test]
    fn format_does_not_break_record_field_line() {
        let mut config = FormatConfig::default();
        config.max_width = 40;
        let source = "data T = T {\n  veryLongFieldName : VeryLongTypeName,\n}\n";
        let result = format(source, &config);
        assert!(
            result.contains("  veryLongFieldName : VeryLongTypeName,"),
            "should not break record field: {:?}",
            result
        );
    }

    #[test]
    fn format_does_not_break_flat_binding_line() {
        let mut config = FormatConfig::default();
        config.max_width = 40;
        let source = "@group(0) @binding(0) uniform veryLongName : VeryLongType\n";
        let result = format(source, &config);
        assert!(
            result.contains("@group(0) @binding(0) uniform veryLongName : VeryLongType"),
            "should not break flat binding line: {:?}",
            result
        );
    }

    #[test]
    fn format_does_not_break_group_block_child_line() {
        let mut config = FormatConfig::default();
        config.max_width = 40;
        let source = "@group(0)\n  @binding(0) uniform veryLongName : VeryLongType\n";
        let result = format(source, &config);
        assert!(
            result.contains("  @binding(0) uniform veryLongName : VeryLongType"),
            "should not break child binding: {:?}",
            result
        );
    }

    #[test]
    fn format_field_with_binding_in_suffix_is_aligned() {
        // A line with `@binding(` in a comment/suffix should still be treated
        // as a record field and aligned with its siblings.
        let source = "data T = T {\n  @location(0) foo : F32 -- @binding(0) comment,\n  @builtin(position) bar : Vec<4, F32>,\n}\n";
        let result = format_default(source);
        assert!(
            result.contains("  @location(0)       foo : F32 -- @binding(0) comment,"),
            "foo should be aligned: {:?}",
            result
        );
        assert!(
            result.contains("  @builtin(position) bar : Vec<4, F32>,"),
            "bar should be aligned: {:?}",
            result
        );
    }

    #[test]
    fn format_binding_continuation_aligns_with_expression() {
        let mut config = FormatConfig::default();
        config.max_width = 50;
        let source = "clampedZ = if abs newPos.z > 0.5 then sign newPos.z * 0.5 else newPos.z\n";
        let result = format(source, &config);
        // All continuation lines must be <= max_width.
        assert!(
            result.lines().all(|l| l.len() <= 50),
            "line exceeded max_width: {:?}",
            result
        );
        // The first continuation (after a binary-op break on the original line)
        // should align with the expression start — column 11, after "clampedZ = ".
        assert!(
            result.contains("\n           0.5 then sign newPos.z *"),
            "should align continuation with expression after '= ': {:?}",
            result
        );
    }

    #[test]
    fn format_type_sig_continuation_aligns_with_expression() {
        let mut config = FormatConfig::default();
        config.max_width = 50;
        let source =
            "sceneDist : Shape -> Shape -> Shape -> Shape -> Shape -> Vec2f -> F32 -> F32\n";
        let result = format(source, &config);
        assert!(
            result.lines().all(|l| l.len() <= 50),
            "line exceeded max_width: {:?}",
            result
        );
        // Continuation should align with the type expression start — column 12,
        // after "sceneDist : ".
        assert!(
            result.contains("\n            Shape -> Vec2f -> F32 -> F32"),
            "should align continuation with expression after ': ': {:?}",
            result
        );
    }
}

#[cfg(test)]
mod negative_literal_tests {
    use super::*;

    #[test]
    fn format_subtraction_vs_application() {
        assert_eq!(format_default("f = x-1\n"), "f = x - 1\n");
        assert_eq!(format_default("f = x - 1\n"), "f = x - 1\n");
        assert_eq!(format_default("f = x- 1\n"), "f = x - 1\n");
        assert_eq!(format_default("f = x -1\n"), "f = x -1\n"); // Application
        assert_eq!(format_default("f = vec2 -0.5\n"), "f = vec2 -0.5\n");
    }
}

#[cfg(test)]
mod if_then_else_tests {
    use super::*;

    #[test]
    fn format_restructures_multiline_if_then_else() {
        let source =
            "           clampedZ = if abs newPos.z > 0.5 then sign newPos.z *\n             0.5 else newPos.z\n";
        let result = format_default(source);
        // The formatter should restructure the multi-line if-then-else
        // so that `then` and `else` are on their own lines, indented one
        // level deeper than the binding.
        assert!(
            result.contains("\n             then"),
            "expected restructured then on separate line, got:\n{}",
            result
        );
        assert!(
            result.contains("\n             else"),
            "expected restructured else on separate line, got:\n{}",
            result
        );
        // The continuation `*` should be collapsed into the then-body.
        assert!(
            !result.contains("*\n"),
            "expected * continuation to be collapsed, got:\n{}",
            result
        );
    }

    #[test]
    fn format_preserves_single_line_if_then_else() {
        let source =
            "           clampedX = if abs newPos.x > 1.0 then sign newPos.x else newPos.x\n";
        let result = format_default(source);
        // Short single-line if-then-else should be kept as-is.
        assert_eq!(
            result,
            "           clampedX = if abs newPos.x > 1.0 then sign newPos.x else newPos.x\n"
        );
    }

    #[test]
    fn format_restructures_if_then_else_idempotent() {
        let source =
            "           clampedZ = if abs newPos.z > 0.5 then sign newPos.z *\n             0.5 else newPos.z\n";
        let first = format_default(source);
        let second = format_default(&first);
        assert_eq!(
            first, second,
            "if-then-else restructuring is not idempotent"
        );
    }
}
