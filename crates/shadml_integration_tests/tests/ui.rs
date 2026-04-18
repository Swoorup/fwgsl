//! UI (compile-fail) test runner using insta snapshots.
//!
//! Each `.shadml` file under `tests/ui/` is compiled, and the formatted
//! diagnostics are snapshotted. Directives in `//` comments control
//! per-test behavior.

use std::fs;
use std::path::PathBuf;

use shadml_ast_lowering::AstLowering;
use shadml_diagnostics::format_diagnostics;
use shadml_parser::parser::Parser;
use shadml_semantic::SemanticAnalyzer;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn with_prelude(program: &mut shadml_parser::parser::Program) {
    let prelude = shadml_parser::prelude_program();
    let mut combined = prelude.decls.clone();
    combined.append(&mut program.decls);
    program.decls = combined;
}

/// Parsed `// ui-*` directives from a test file.
struct UiConfig {
    /// Whether to prepend the prelude before analysis. Default: true.
    prelude: bool,
    /// Whether to also run AST lowering. Default: false ("semantic").
    stage: String,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            prelude: true,
            stage: "semantic".into(),
        }
    }
}

/// Extract `// ui-*` directives from source text, returning the config
/// and the source with directive lines stripped.
fn parse_directives(source: &str) -> (UiConfig, String) {
    let mut config = UiConfig::default();
    let mut clean_lines = Vec::new();

    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("// ui-") {
            // Parse directive
            let directive = trimmed.trim_start_matches("// ").trim();
            if let Some(value) = directive.strip_prefix("ui-prelude:") {
                config.prelude = value.trim().parse().unwrap_or(true);
            } else if let Some(value) = directive.strip_prefix("ui-stage:") {
                config.stage = value.trim().to_string();
            }
            // Skip the directive line from the source
        } else {
            clean_lines.push(line);
        }
    }

    (config, clean_lines.join("\n"))
}

fn run_ui_test(path: &std::path::Path) -> String {
    let source_raw = fs::read_to_string(path).expect("failed to read test file");
    let (config, source) = parse_directives(&source_raw);
    let file_name = path
        .file_name()
        .unwrap_or_default()
        .to_str()
        .unwrap_or("unknown");

    // Parse
    let mut parser = Parser::new(&source);
    let mut program = parser.parse_program();
    let _parse_errors = parser.diagnostics().has_errors();

    // Optionally prepend prelude
    if config.prelude {
        with_prelude(&mut program);
    }

    // Semantic analysis
    let mut sa = SemanticAnalyzer::new();
    sa.analyze(&program);

    // Collect diagnostics so far
    let mut all_diagnostics: Vec<shadml_diagnostics::Diagnostic> = Vec::new();
    all_diagnostics.extend(parser.diagnostics().iter().cloned());
    all_diagnostics.extend(sa.diagnostics().iter().cloned());

    // Optionally run lowering
    if config.stage == "lowering" {
        let mut lowering = AstLowering::new(&sa);
        lowering.lower_program(&program);
        all_diagnostics.extend(lowering.diagnostics().iter().cloned());
    }

    // Use the cleaned source (without directive lines) for span resolution
    format_diagnostics(&all_diagnostics, file_name, &source)
}

fn collect_shadml_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if dir.is_dir() {
        for entry in fs::read_dir(dir).expect("failed to read dir") {
            let entry = entry.expect("failed to read dir entry");
            let path = entry.path();
            if path.is_dir() {
                files.extend(collect_shadml_files(&path));
            } else if path.extension().is_some_and(|ext| ext == "shadml") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

// ---------------------------------------------------------------------------
// Test runner
// ---------------------------------------------------------------------------

#[test]
fn ui_tests() {
    let ui_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("ui");

    let files = collect_shadml_files(&ui_dir);
    assert!(!files.is_empty(), "no .shadml test files found in {ui_dir:?}");

    for path in &files {
        let file_stem = path.file_stem().unwrap().to_str().unwrap().to_string();
        let output = run_ui_test(path);
        insta::assert_snapshot!(file_stem, output);
    }
}