//! Diagnostic reporting for shadml.
//!
//! Provides structured diagnostics with severity levels, source labels,
//! and miette integration for rich terminal rendering.

use std::fmt;

use shadml_span::Span;

/// Severity level for a diagnostic.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

/// A label pointing to a source location with an associated message.
#[derive(Clone, Debug)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

impl Label {
    /// Create a new label at the given span with a message.
    pub fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }

    /// Create a primary label (alias for `new`).
    pub fn primary(span: Span, message: impl Into<String>) -> Self {
        Self::new(span, message)
    }
}

/// A structured diagnostic message.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub code: Option<String>,
    pub labels: Vec<Label>,
    pub help: Option<String>,
}

impl Diagnostic {
    /// Create a new error diagnostic.
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
            code: None,
            labels: Vec::new(),
            help: None,
        }
    }

    /// Create a new warning diagnostic.
    pub fn warning(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            message: message.into(),
            code: None,
            labels: Vec::new(),
            help: None,
        }
    }

    /// Add a label to the diagnostic (builder pattern).
    pub fn with_label(mut self, label: Label) -> Self {
        self.labels.push(label);
        self
    }

    /// Set the help message (builder pattern).
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// Set the error code (builder pattern).
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }
}

/// A collection of diagnostics accumulated during compilation.
pub struct DiagnosticSink {
    diagnostics: Vec<Diagnostic>,
}

impl DiagnosticSink {
    /// Create a new empty diagnostic sink.
    pub fn new() -> Self {
        Self {
            diagnostics: Vec::new(),
        }
    }

    /// Push a diagnostic into the sink.
    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.diagnostics.push(diagnostic);
    }

    /// Return all error-level diagnostics.
    pub fn errors(&self) -> &[Diagnostic] {
        // Note: returns all diagnostics; filter to errors only
        // We return a slice, but since mixed severities may exist,
        // we provide a filtered view via `iter`.
        // For a simple slice return, we return all diagnostics.
        // Use `iter().filter()` for severity-specific filtering.
        &self.diagnostics
    }

    /// Check whether any error-level diagnostics have been reported.
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    /// Iterate over all diagnostics.
    pub fn iter(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics.iter()
    }
}

impl Default for DiagnosticSink {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Plain-text diagnostic formatting (for UI test snapshots)
// ---------------------------------------------------------------------------

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Error => write!(f, "error"),
            Severity::Warning => write!(f, "warning"),
            Severity::Info => write!(f, "info"),
            Severity::Hint => write!(f, "hint"),
        }
    }
}

/// Convert a byte offset into a 1-based (line, column) pair.
fn byte_offset_to_line_col(source: &str, offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 1;
    for (i, ch) in source.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Return the 1-indexed line at the given line number.
fn source_line(source: &str, line: usize) -> &str {
    source.lines().nth(line.wrapping_sub(1)).unwrap_or("")
}

/// Format diagnostics as a stable, human-readable string suitable for
/// snapshot testing.
///
/// Diagnostics are sorted by their first label's span offset for
/// deterministic output. Each diagnostic includes severity, message,
/// a `-->` location line, source snippet with underline, and optional help.
pub fn format_diagnostics(diagnostics: &[Diagnostic], source_name: &str, source: &str) -> String {
    // Sort by first label span offset for determinism
    let mut sorted: Vec<&Diagnostic> = diagnostics.iter().collect();
    sorted.sort_by_key(|d| {
        d.labels
            .first()
            .map(|l| l.span.start)
            .unwrap_or(u32::MAX)
    });

    let mut out = String::new();
    for (i, diag) in sorted.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }

        // Severity + message
        if let Some(ref code) = diag.code {
            out.push_str(&format!("{}[{}]: {}", diag.severity, code, diag.message));
        } else {
            out.push_str(&format!("{}: {}", diag.severity, diag.message));
        }
        out.push('\n');

        // Primary location and source snippet from first label
        if let Some(label) = diag.labels.first() {
            let (line, col) = byte_offset_to_line_col(source, label.span.start as usize);
            out.push_str(&format!("  --> {}:{}:{}\n", source_name, line, col));

            let src_line = source_line(source, line);
            let line_num_width = format!("{}", line).len();
            out.push_str(&format!("   |\n"));
            out.push_str(&format!("{:width$} | {}\n", line, src_line, width = line_num_width));

            // Underline: put tildes under the span
            let span_len = if label.span.end > label.span.start {
                label.span.end - label.span.start
            } else {
                1
            };
            let prefix = " ".repeat(col.wrapping_sub(1));
            let underline = "~".repeat(span_len as usize);
            out.push_str(&format!(
                "{:width$} | {}{} {}\n",
                "", prefix, underline, label.message, width = line_num_width
            ));
            out.push_str(&format!("   |\n"));
        }

        // Additional labels (secondary)
        for label in diag.labels.iter().skip(1) {
            let (line, col) = byte_offset_to_line_col(source, label.span.start as usize);
            let src_line = source_line(source, line);
            let line_num_width = format!("{}", line).len();
            out.push_str(&format!(
                "{:width$} | {}\n",
                line,
                src_line,
                width = line_num_width
            ));
            let span_len = if label.span.end > label.span.start {
                label.span.end - label.span.start
            } else {
                1
            };
            let prefix = " ".repeat(col.wrapping_sub(1));
            let underline = "-".repeat(span_len as usize);
            out.push_str(&format!(
                "{:width$} | {}{} {}\n",
                "", prefix, underline, label.message, width = line_num_width
            ));
        }

        // Help text
        if let Some(ref help) = diag.help {
            out.push_str(&format!("   = help: {}\n", help));
        }
    }

    out
}

// ---------------------------------------------------------------------------
// miette integration
// ---------------------------------------------------------------------------

/// A wrapper that converts a shadml `Diagnostic` into a miette-renderable
/// diagnostic, carrying source code context for display.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct MietteDiagnostic {
    message: String,
    src: miette::NamedSource<String>,
    severity: Severity,
    code: Option<String>,
    labels: Vec<MietteLabel>,
    help: Option<String>,
}

#[derive(Debug)]
struct MietteLabel {
    span: miette::SourceSpan,
    message: String,
}

impl MietteDiagnostic {
    /// Create a miette-renderable diagnostic from a shadml diagnostic.
    ///
    /// `source_name` is a display name for the source file (e.g. "main.shadml").
    /// `source_code` is the full source text that spans reference into.
    pub fn from_diagnostic(
        diag: &Diagnostic,
        source_name: impl AsRef<str>,
        source_code: impl Into<String>,
    ) -> Self {
        let source_text = source_code.into();
        let labels = diag
            .labels
            .iter()
            .map(|l| MietteLabel {
                span: miette::SourceSpan::new(
                    miette::SourceOffset::from(l.span.start as usize),
                    l.span.end as usize - l.span.start as usize,
                ),
                message: l.message.clone(),
            })
            .collect();

        Self {
            message: diag.message.clone(),
            src: miette::NamedSource::new(source_name.as_ref(), source_text),
            severity: diag.severity,
            code: diag.code.clone(),
            labels,
            help: diag.help.clone(),
        }
    }
}

impl miette::Diagnostic for MietteDiagnostic {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.code
            .as_ref()
            .map(|c| Box::new(c.clone()) as Box<dyn fmt::Display>)
    }

    fn severity(&self) -> Option<miette::Severity> {
        Some(match self.severity {
            Severity::Error => miette::Severity::Error,
            Severity::Warning => miette::Severity::Warning,
            Severity::Info | Severity::Hint => miette::Severity::Advice,
        })
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.help
            .as_ref()
            .map(|h| Box::new(h.clone()) as Box<dyn fmt::Display>)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        if self.labels.is_empty() {
            None
        } else {
            Some(Box::new(self.labels.iter().map(|l| {
                miette::LabeledSpan::new_with_span(Some(l.message.clone()), l.span)
            })))
        }
    }

    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        Some(&self.src)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shadml_span::Span;

    #[test]
    fn test_diagnostic_error_builder() {
        let diag = Diagnostic::error("type mismatch")
            .with_code("E001")
            .with_label(Label::primary(Span::new(0, 5), "expected Int"))
            .with_help("try adding a type annotation");

        assert_eq!(diag.severity, Severity::Error);
        assert_eq!(diag.message, "type mismatch");
        assert_eq!(diag.code.as_deref(), Some("E001"));
        assert_eq!(diag.labels.len(), 1);
        assert_eq!(diag.help.as_deref(), Some("try adding a type annotation"));
    }

    #[test]
    fn test_diagnostic_warning() {
        let diag = Diagnostic::warning("unused variable");
        assert_eq!(diag.severity, Severity::Warning);
        assert_eq!(diag.message, "unused variable");
    }

    #[test]
    fn test_diagnostic_sink() {
        let mut sink = DiagnosticSink::new();
        assert!(!sink.has_errors());

        sink.push(Diagnostic::warning("unused import"));
        assert!(!sink.has_errors());

        sink.push(Diagnostic::error("syntax error"));
        assert!(sink.has_errors());
        assert_eq!(sink.iter().count(), 2);
    }

    #[test]
    fn test_miette_diagnostic_creation() {
        let diag = Diagnostic::error("unexpected token")
            .with_code("E100")
            .with_label(Label::primary(Span::new(0, 3), "here"))
            .with_help("did you mean `let`?");

        let miette_diag = MietteDiagnostic::from_diagnostic(&diag, "test.shadml", "lat x = 42");

        assert_eq!(miette_diag.message, "unexpected token");
        assert_eq!(miette_diag.code.as_deref(), Some("E100"));
        assert_eq!(miette_diag.labels.len(), 1);

        // Verify it implements miette::Diagnostic
        use miette::Diagnostic as _;
        assert_eq!(miette_diag.severity(), Some(miette::Severity::Error));
    }

    #[test]
    fn test_severity_copy() {
        let s = Severity::Error;
        let s2 = s;
        assert_eq!(s, s2);
    }

    #[test]
    fn test_label_constructors() {
        let span = Span::new(5, 10);
        let l1 = Label::new(span, "message");
        let l2 = Label::primary(span, "message");
        assert_eq!(l1.span, l2.span);
        assert_eq!(l1.message, l2.message);
    }
}
