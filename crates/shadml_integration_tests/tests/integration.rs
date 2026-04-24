//! End-to-end integration tests for the shadml compiler pipeline.
//!
//! These tests exercise the full compilation pipeline:
//!   source -> lex -> parse -> semantic analysis -> (HIR -> MIR -> WGSL codegen)
//!
//! They verify that crates interoperate correctly, not just that each crate
//! works in isolation.
//!
//! NOTE: The current parser has a known limitation where `skip_trivia` crosses
//! newlines, causing type signatures on one line followed by a function
//! definition on the next to be merged. Tests are written to account for this
//! behavior. Single-line declarations and indented continuation parse correctly.

use shadml_ast_lowering::AstLowering;
use shadml_mir::*;
use shadml_parser::lexer::lex;
use shadml_parser::parser::{AttrArg, AttrValue, Decl, Expr, Parser, Program};
use shadml_parser::resolve_layout;
use shadml_semantic::SemanticAnalyzer;
use shadml_syntax::SyntaxKind;
use shadml_wgsl_codegen::emit_wgsl;

// =========================================================================
// Helper utilities
// =========================================================================

fn with_prelude(program: &mut Program) {
    let prelude = shadml_parser::prelude_program();
    let mut combined = prelude.decls.clone();
    combined.append(&mut program.decls);
    program.decls = combined;
}

/// Parse shadml source without prelude. Use for parse-only tests that inspect decl counts/indices.
fn parse_raw(source: &str) -> (Program, bool) {
    let mut parser = Parser::new(source);
    let program = parser.parse_program();
    let has_errors = parser.diagnostics().has_errors();
    (program, has_errors)
}

/// Parse shadml source with prelude prepended. Use for semantic analysis and compilation tests.
fn parse(source: &str) -> (Program, bool) {
    let (mut program, has_errors) = parse_raw(source);
    with_prelude(&mut program);
    (program, has_errors)
}

/// Parse and run semantic analysis. Returns (analyzer, has_any_errors).
fn parse_and_analyze(source: &str) -> (SemanticAnalyzer, bool) {
    let (program, parse_errors) = parse(source);
    let mut sa = SemanticAnalyzer::new();
    sa.analyze(&program);
    let has_errors = parse_errors || sa.has_errors();
    (sa, has_errors)
}

// =========================================================================
// 1. Parse tests -- single-line and single-declaration inputs
// =========================================================================

#[path = "test_modules/bitwise_tests.rs"]
mod bitwise_tests;
#[path = "test_modules/codegen_tests.rs"]
mod codegen_tests;
#[path = "test_modules/const_promotion_tests.rs"]
mod const_promotion_tests;
#[path = "test_modules/error_recovery_tests.rs"]
mod error_recovery_tests;
#[path = "test_modules/fixture_tests.rs"]
mod fixture_tests;
#[path = "test_modules/fold_range_tests.rs"]
mod fold_range_tests;
#[path = "test_modules/full_pipeline_tests.rs"]
mod full_pipeline_tests;
#[path = "test_modules/lexer_tests.rs"]
mod lexer_tests;
#[path = "test_modules/naga_validation.rs"]
mod naga_validation;
#[path = "test_modules/parse_multi_decl_tests.rs"]
mod parse_multi_decl_tests;
#[path = "test_modules/parse_single_decl_tests.rs"]
mod parse_single_decl_tests;
#[path = "test_modules/pipeline_tests.rs"]
mod pipeline_tests;
#[path = "test_modules/render_block_tests.rs"]
mod render_block_tests;
#[path = "test_modules/semantic_tests.rs"]
mod semantic_tests;
#[path = "test_modules/trait_tests.rs"]
mod trait_tests;
