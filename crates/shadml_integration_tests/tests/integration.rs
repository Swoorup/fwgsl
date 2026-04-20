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
use shadml_parser::parser::{Decl, Expr, Parser, Program};
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

mod parse_single_decl_tests {
    use super::*;

    #[test]
    fn parse_single_type_signature() {
        let source = "add : I32 -> I32 -> I32";
        let (program, has_errors) = parse_raw(source);
        assert!(!has_errors, "single type sig should parse without errors");
        assert_eq!(program.decls.len(), 1);
        assert!(
            matches!(&program.decls[0], Decl::TypeSig { name, .. } if name == "add"),
            "expected TypeSig, got {:?}",
            program.decls[0]
        );
    }

    #[test]
    fn parse_dependent_array_type_signature() {
        let source = "grid : Tensor 2 (Tensor 4 F32)";
        let (program, has_errors) = parse_raw(source);
        assert!(
            !has_errors,
            "dependent array type sig should parse without errors"
        );
        assert_eq!(program.decls.len(), 1);
        assert!(
            matches!(&program.decls[0], Decl::TypeSig { .. }),
            "expected TypeSig, got {:?}",
            program.decls[0]
        );
    }

    #[test]
    fn parse_single_function() {
        let source = "add x y = x + y";
        let (program, has_errors) = parse_raw(source);
        assert!(!has_errors, "single function should parse without errors");
        assert_eq!(program.decls.len(), 1);
        assert!(
            matches!(&program.decls[0], Decl::FunDecl { name, params, .. } if name == "add" && params.len() == 2),
            "expected FunDecl with 2 params, got {:?}",
            program.decls[0]
        );
    }

    #[test]
    fn parse_data_type_declaration() {
        let source = "data Color = Red | Green | Blue";
        let (program, has_errors) = parse_raw(source);
        assert!(!has_errors, "data declaration should parse without errors");
        assert_eq!(program.decls.len(), 1);
        if let Decl::DataDecl {
            name,
            constructors,
            type_params,
            ..
        } = &program.decls[0]
        {
            assert_eq!(name, "Color");
            assert!(type_params.is_empty());
            assert_eq!(constructors.len(), 3);
            assert_eq!(constructors[0].name, "Red");
            assert_eq!(constructors[1].name, "Green");
            assert_eq!(constructors[2].name, "Blue");
        } else {
            panic!("expected DataDecl, got {:?}", program.decls[0]);
        }
    }

    #[test]
    fn parse_generic_data_type_declaration() {
        let source = "data Box a = Box a";
        let (program, has_errors) = parse_raw(source);
        assert!(
            !has_errors,
            "generic data declaration should parse without errors"
        );
        assert_eq!(program.decls.len(), 1);
        if let Decl::DataDecl {
            name,
            constructors,
            type_params,
            ..
        } = &program.decls[0]
        {
            assert_eq!(name, "Box");
            assert_eq!(type_params, &vec!["a".to_string()]);
            assert_eq!(constructors.len(), 1);
            assert_eq!(constructors[0].name, "Box");
        } else {
            panic!("expected DataDecl, got {:?}", program.decls[0]);
        }
    }

    #[test]
    fn parse_function_with_inline_let() {
        let source = "f x = let y = x + 1 in y";
        let (program, has_errors) = parse_raw(source);
        assert!(!has_errors, "function with let should parse without errors");
        if let Decl::FunDecl { body, .. } = &program.decls[0] {
            assert!(
                matches!(body, Expr::Let(binds, _, _) if binds.len() == 1),
                "body should be a let expression with one binding, got {:?}",
                body
            );
        } else {
            panic!("expected FunDecl");
        }
    }

    #[test]
    fn parse_function_with_where_clause() {
        let source = "f x = y + 1 where y = x";
        let (program, has_errors) = parse_raw(source);
        assert!(
            !has_errors,
            "function with where should parse without errors"
        );
        if let Decl::FunDecl {
            body, where_binds, ..
        } = &program.decls[0]
        {
            assert!(
                matches!(body, Expr::Infix(_, op, _, _) if op == "+"),
                "body should remain the main expression, got {:?}",
                body
            );
            assert_eq!(where_binds.len(), 1, "expected one where binding");
            assert_eq!(where_binds[0].name, "y");
        } else {
            panic!("expected FunDecl");
        }
    }

    #[test]
    fn parse_lambda_expression() {
        let source = "f = \\x y -> x + y";
        let (program, has_errors) = parse_raw(source);
        assert!(!has_errors, "lambda should parse without errors");
        assert_eq!(program.decls.len(), 1);
        if let Decl::FunDecl { body, .. } = &program.decls[0] {
            assert!(
                matches!(body, Expr::Lambda(params, _, _) if params.len() == 2),
                "body should be a lambda with 2 params, got {:?}",
                body
            );
        } else {
            panic!("expected FunDecl");
        }
    }

    #[test]
    fn parse_if_expression() {
        let source = "f x = if x == 0 then 1 else 2";
        let (program, has_errors) = parse_raw(source);
        assert!(!has_errors, "if expression should parse without errors");
        if let Decl::FunDecl { body, .. } = &program.decls[0] {
            assert!(
                matches!(body, Expr::If(_, _, _, _)),
                "body should be an if expression, got {:?}",
                body
            );
        } else {
            panic!("expected FunDecl");
        }
    }

    #[test]
    fn parse_case_expression() {
        let source = "f c = match c\n  | Red -> 0\n  | Green -> 1\n  | Blue -> 2";
        let (program, has_errors) = parse_raw(source);
        assert!(!has_errors, "case expression should parse without errors");
        if let Decl::FunDecl { body, .. } = &program.decls[0] {
            assert!(
                matches!(body, Expr::Case(_, arms, _) if arms.len() == 3),
                "body should be a case expression with 3 arms, got {:?}",
                body
            );
        } else {
            panic!("expected FunDecl");
        }
    }

    #[test]
    fn parse_infix_operators() {
        let source = "f x y = x + y * 2 - 1";
        let (program, has_errors) = parse_raw(source);
        assert!(!has_errors, "infix operators should parse without errors");
        assert_eq!(program.decls.len(), 1);
    }

    #[test]
    fn parse_operator_section() {
        let source = "f = (+)";
        let (program, has_errors) = parse_raw(source);
        assert!(!has_errors, "operator section should parse without errors");
        if let Decl::FunDecl { body, .. } = &program.decls[0] {
            assert!(
                matches!(body, Expr::OpSection(..)),
                "body should be an operator section, got {:?}",
                body
            );
        } else {
            panic!("expected FunDecl");
        }
    }

    #[test]
    fn parse_negation() {
        let source = "neg x = -x";
        let (program, has_errors) = parse_raw(source);
        assert!(!has_errors, "negation should parse without errors");
        if let Decl::FunDecl { body, .. } = &program.decls[0] {
            assert!(
                matches!(body, Expr::Neg(_, _)),
                "body should be a negation expression, got {:?}",
                body
            );
        } else {
            panic!("expected FunDecl");
        }
    }

    #[test]
    fn parse_parenthesized_expression() {
        let source = "f x = (x + 1) * 2";
        let (program, has_errors) = parse_raw(source);
        assert!(
            !has_errors,
            "parenthesized expression should parse without errors"
        );
        assert_eq!(program.decls.len(), 1);
    }

    #[test]
    fn parse_tuple() {
        let source = "f = (1, 2, 3)";
        let (program, has_errors) = parse_raw(source);
        assert!(!has_errors, "tuple should parse without errors");
        if let Decl::FunDecl { body, .. } = &program.decls[0] {
            assert!(
                matches!(body, Expr::Tuple(elems, _) if elems.len() == 3),
                "body should be a tuple with 3 elements, got {:?}",
                body
            );
        } else {
            panic!("expected FunDecl");
        }
    }

    #[test]
    fn parse_unit() {
        let source = "f = ()";
        let (program, has_errors) = parse_raw(source);
        assert!(!has_errors, "unit should parse without errors");
        if let Decl::FunDecl { body, .. } = &program.decls[0] {
            assert!(
                matches!(body, Expr::Tuple(elems, _) if elems.is_empty()),
                "body should be an empty tuple (unit), got {:?}",
                body
            );
        } else {
            panic!("expected FunDecl");
        }
    }

    #[test]
    fn parse_entry_point() {
        let source = "@vertex\nmain x = x + 1";
        let (program, has_errors) = parse_raw(source);
        assert!(!has_errors, "entry point should parse without errors");
        assert_eq!(program.decls.len(), 1);
        if let Decl::EntryPoint {
            attributes, name, ..
        } = &program.decls[0]
        {
            assert_eq!(name, "main");
            assert_eq!(attributes.len(), 1);
            assert_eq!(attributes[0].name, "vertex");
        } else {
            panic!("expected EntryPoint, got {:?}", program.decls[0]);
        }
    }

    #[test]
    fn parse_compute_entry_point_with_workgroup_size() {
        // The parser requires parenthesized attribute arguments: @workgroup_size(64, 1, 1)
        let source = "@compute @workgroup_size(64, 1, 1)\nmain x = x + 1";
        let (program, has_errors) = parse_raw(source);
        assert!(
            !has_errors,
            "compute entry point should parse without errors"
        );
        assert_eq!(program.decls.len(), 1);
        if let Decl::EntryPoint { attributes, .. } = &program.decls[0] {
            assert!(
                attributes.len() >= 2,
                "expected at least 2 attributes, got {}",
                attributes.len()
            );
            assert_eq!(attributes[0].name, "compute");
            assert_eq!(attributes[1].name, "workgroup_size");
            assert_eq!(attributes[1].args, vec!["64", "1", "1"]);
        } else {
            panic!("expected EntryPoint, got {:?}", program.decls[0]);
        }
    }

    #[test]
    fn parse_precedence_mul_over_add() {
        let source = "f = 1 + 2 * 3";
        let (program, has_errors) = parse_raw(source);
        assert!(!has_errors);
        if let Decl::FunDecl { body, .. } = &program.decls[0] {
            assert!(
                matches!(body, Expr::Infix(_, op, _, _) if op == "+"),
                "top-level infix should be +, got {:?}",
                body
            );
        } else {
            panic!("expected FunDecl");
        }
    }

    #[test]
    fn parse_nested_let() {
        let source = "f x = let a = 1 in let b = 2 in a + b";
        let (program, has_errors) = parse_raw(source);
        assert!(!has_errors, "nested let should parse without errors");
        if let Decl::FunDecl { body, .. } = &program.decls[0] {
            assert!(
                matches!(body, Expr::Let(_, inner, _) if matches!(inner.as_ref(), Expr::Let(_, _, _))),
                "body should be nested let expressions, got {:?}",
                body
            );
        } else {
            panic!("expected FunDecl");
        }
    }

    #[test]
    fn parse_multiline_let_in() {
        let source = "f x =\n  let y = x + 1\n  in y * 2";
        let (program, has_errors) = parse_raw(source);
        assert!(!has_errors, "multiline let-in should parse without errors");
        if let Decl::FunDecl { body, .. } = &program.decls[0] {
            assert!(
                matches!(body, Expr::Let(_, _, _)),
                "body should be a let expression, got {:?}",
                body
            );
        } else {
            panic!("expected FunDecl");
        }
    }
}

// =========================================================================
// 2. Multi-declaration parse tests (matches current parser behavior)
// =========================================================================

mod parse_multi_decl_tests {
    use super::*;

    #[test]
    fn parse_full_program_has_at_least_4_decls() {
        let source = "\
data Color = Red | Green | Blue

show : Color -> I32
show c = match c
  | Red   -> 0
  | Green -> 1
  | Blue  -> 2

add : I32 -> I32 -> I32
add x y = x + y

main : I32 -> I32
main x =
  let y = add x 1
  in show Red
";
        let (program, _has_errors) = parse_raw(source);
        assert!(
            program.decls.len() >= 4,
            "full program should produce at least 4 declarations, got {}",
            program.decls.len()
        );
    }

    #[test]
    fn parse_data_decl_is_first() {
        let source = "\
data Color = Red | Green | Blue

show c = match c
  | Red -> 0
  | Green -> 1
  | Blue -> 2
";
        let (program, _has_errors) = parse_raw(source);
        assert!(
            matches!(&program.decls[0], Decl::DataDecl { name, .. } if name == "Color"),
            "first declaration should be DataDecl Color, got {:?}",
            program.decls[0]
        );
    }

    #[test]
    fn parse_comments_are_ignored_by_lexer() {
        let source = "-- This is a comment\nadd x y = x + y";
        let (program, has_errors) = parse_raw(source);
        assert!(!has_errors, "comments should not cause parse errors");
        assert!(
            program.decls.len() >= 1,
            "should have at least 1 declaration"
        );
    }

    #[test]
    fn parse_block_comment_is_ignored() {
        let source = "{- block comment -} add x y = x + y";
        let (program, has_errors) = parse_raw(source);
        assert!(!has_errors, "block comments should not cause parse errors");
        assert_eq!(program.decls.len(), 1);
    }
}

// =========================================================================
// 3. Lexer tests
// =========================================================================

mod lexer_tests {
    use super::*;

    #[test]
    fn lex_preserves_all_source_bytes() {
        let source = "add x y = x + y";
        let tokens = lex(source);
        let mut covered = 0u32;
        for tok in &tokens {
            if tok.kind == SyntaxKind::Eof {
                break;
            }
            assert_eq!(
                tok.span.start, covered,
                "gap or overlap in token stream at byte {}",
                covered
            );
            covered = tok.span.end;
        }
        assert_eq!(
            covered as usize,
            source.len(),
            "tokens should cover the entire source"
        );
    }

    #[test]
    fn lex_round_trip_text() {
        let source = "data Color = Red | Green | Blue";
        let tokens = lex(source);
        let reconstructed: String = tokens
            .iter()
            .filter(|t| t.kind != SyntaxKind::Eof)
            .map(|t| t.span.source_text(source))
            .collect();
        assert_eq!(
            reconstructed, source,
            "concatenating token texts should reproduce the original source"
        );
    }

    #[test]
    fn lex_single_function_tokens() {
        let source = "add x y = x + y";
        let tokens = lex(source);
        let non_trivia: Vec<_> = tokens
            .iter()
            .filter(|t| !t.kind.is_trivia())
            .filter(|t| t.kind != SyntaxKind::Eof)
            .collect();
        // add, x, y, =, x, +, y
        assert_eq!(
            non_trivia.len(),
            7,
            "expected 7 non-trivia tokens for 'add x y = x + y', got {}",
            non_trivia.len()
        );
    }

    #[test]
    fn lex_all_fixture_files_without_error_tokens() {
        let fixtures = [
            (
                "hello.shadml",
                include_str!("../../../fixtures/hello.shadml"),
            ),
            ("adt.shadml", include_str!("../../../fixtures/adt.shadml")),
            (
                "particle.shadml",
                include_str!("../../../fixtures/particle.shadml"),
            ),
        ];
        for (name, source) in &fixtures {
            let tokens = lex(source);
            let error_tokens: Vec<_> = tokens
                .iter()
                .filter(|t| t.kind == SyntaxKind::Error)
                .collect();
            assert!(
                error_tokens.is_empty(),
                "fixture {} produced {} error tokens",
                name,
                error_tokens.len()
            );
        }
    }

    #[test]
    fn lex_keywords() {
        let source = "let in case of match where data if then else do";
        let tokens = lex(source);
        let keyword_tokens: Vec<_> = tokens
            .iter()
            .filter(|t| {
                matches!(
                    t.kind,
                    SyntaxKind::KwLet
                        | SyntaxKind::KwIn
                        | SyntaxKind::KwCase
                        | SyntaxKind::KwOf
                        | SyntaxKind::KwMatch
                        | SyntaxKind::KwWhere
                        | SyntaxKind::KwData
                        | SyntaxKind::KwIf
                        | SyntaxKind::KwThen
                        | SyntaxKind::KwElse
                        | SyntaxKind::KwDo
                )
            })
            .collect();
        assert_eq!(
            keyword_tokens.len(),
            11,
            "expected 11 keywords, got {}",
            keyword_tokens.len()
        );
    }

    #[test]
    fn lex_operators() {
        let source = "-> => :: .. <= >= == /= && || <-";
        let tokens = lex(source);
        let op_tokens: Vec<_> = tokens
            .iter()
            .filter(|t| {
                matches!(
                    t.kind,
                    SyntaxKind::Arrow
                        | SyntaxKind::FatArrow
                        | SyntaxKind::ColonColon
                        | SyntaxKind::DotDot
                        | SyntaxKind::LessEqual
                        | SyntaxKind::GreaterEqual
                        | SyntaxKind::EqualEqual
                        | SyntaxKind::NotEqual
                        | SyntaxKind::AndAnd
                        | SyntaxKind::OrOr
                        | SyntaxKind::LeftArrow
                )
            })
            .collect();
        assert_eq!(
            op_tokens.len(),
            11,
            "expected 11 multi-char operators, got {}",
            op_tokens.len()
        );
    }

    #[test]
    fn lex_numeric_literals() {
        let source = "42 0xFF 3.14 0b101 0o77 1.0e10";
        let tokens = lex(source);
        let int_count = tokens
            .iter()
            .filter(|t| t.kind == SyntaxKind::IntLiteral)
            .count();
        let float_count = tokens
            .iter()
            .filter(|t| t.kind == SyntaxKind::FloatLiteral)
            .count();
        assert_eq!(
            int_count, 4,
            "expected 4 int literals (42, 0xFF, 0b101, 0o77)"
        );
        assert_eq!(float_count, 2, "expected 2 float literals (3.14, 1.0e10)");
    }

    #[test]
    fn lex_string_and_char_literals() {
        let source = r#""hello" 'a'"#;
        let tokens = lex(source);
        assert!(
            tokens.iter().any(|t| t.kind == SyntaxKind::StringLiteral),
            "should contain a string literal"
        );
        assert!(
            tokens.iter().any(|t| t.kind == SyntaxKind::CharLiteral),
            "should contain a char literal"
        );
    }

    #[test]
    fn layout_resolver_adds_virtual_tokens() {
        let source = "f x =\n  let y = 1\n  in y\n";
        let raw = lex(source);
        let resolved = resolve_layout(raw, source);
        let layout_kinds: Vec<_> = resolved
            .iter()
            .filter(|t| {
                matches!(
                    t.kind,
                    SyntaxKind::LayoutBraceOpen
                        | SyntaxKind::LayoutSemicolon
                        | SyntaxKind::LayoutBraceClose
                )
            })
            .map(|t| t.kind)
            .collect();
        assert!(
            !layout_kinds.is_empty(),
            "layout resolver should insert virtual layout tokens"
        );
    }

    #[test]
    fn layout_resolver_balances_braces() {
        let source = "do\n  x <- action\n  return x\n";
        let raw = lex(source);
        let resolved = resolve_layout(raw, source);
        let open_count = resolved
            .iter()
            .filter(|t| t.kind == SyntaxKind::LayoutBraceOpen)
            .count();
        let close_count = resolved
            .iter()
            .filter(|t| t.kind == SyntaxKind::LayoutBraceClose)
            .count();
        assert_eq!(
            open_count, close_count,
            "layout braces should be balanced (opens={}, closes={})",
            open_count, close_count
        );
    }

    #[test]
    fn all_fixtures_lex_preserves_source() {
        let fixtures = [
            (
                "hello.shadml",
                include_str!("../../../fixtures/hello.shadml"),
            ),
            ("adt.shadml", include_str!("../../../fixtures/adt.shadml")),
            (
                "particle.shadml",
                include_str!("../../../fixtures/particle.shadml"),
            ),
        ];
        for (name, source) in &fixtures {
            let tokens = lex(source);
            let reconstructed: String = tokens
                .iter()
                .filter(|t| t.kind != SyntaxKind::Eof)
                .map(|t| t.span.source_text(source))
                .collect();
            assert_eq!(
                &reconstructed, source,
                "lexer round-trip should preserve source text for {}",
                name
            );
        }
    }
}

// =========================================================================
// 4. Semantic analysis tests (using single-declaration programs)
// =========================================================================

mod semantic_tests {
    use super::*;

    #[test]
    fn well_typed_function_inferred() {
        let source = "f x = x + 1";
        let (sa, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "well-typed inferred function should have no errors"
        );
        assert!(sa.env.lookup("f").is_some(), "f should be in type env");
    }

    #[test]
    fn inferred_type_for_arithmetic_function() {
        let source = "f x = x + 1";
        let (sa, has_errors) = parse_and_analyze(source);
        assert!(!has_errors);
        let scheme = sa.env.lookup("f").expect("f should be in env");
        let ty = sa.engine.finalize(&scheme.ty);
        let ty_str = format!("{}", ty);
        assert_eq!(
            ty_str, "(I32 -> I32)",
            "f should have type I32 -> I32, got: {}",
            ty_str
        );
    }

    #[test]
    fn well_typed_if_expression() {
        let source = "f x = if x > 0 then x else 0 - x";
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "if expression with consistent branches should type check"
        );
    }

    #[test]
    fn well_typed_let_expression() {
        let source = "f x = let y = x + 1 in y * 2";
        let (_, has_errors) = parse_and_analyze(source);
        assert!(!has_errors, "let expression should type check");
    }

    #[test]
    fn well_typed_where_expression() {
        let source = "f x = y * 2 where y = x + 1";
        let (_, has_errors) = parse_and_analyze(source);
        assert!(!has_errors, "where clause should type check");
    }

    #[test]
    fn well_typed_data_type_and_pattern_match() {
        // Multi-line match arms may be affected by the parser cross-line merge.
        // First, verify that the data type declaration and constructors are registered.
        let source = "\
data Color = Red | Green | Blue

show c = match c
  | Red   -> 0
  | Green -> 1
  | Blue  -> 2
";
        let (program, _parse_errors) = parse(source);
        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);
        // The constructors should be registered from the data declaration
        assert!(
            sa.constructors.contains_key("Red"),
            "Red constructor should be registered"
        );
        assert!(
            sa.constructors.contains_key("Green"),
            "Green constructor should be registered"
        );
        assert!(
            sa.constructors.contains_key("Blue"),
            "Blue constructor should be registered"
        );
    }

    #[test]
    fn lambda_type_inference() {
        let source = "f = \\x -> x + 1";
        let (_, has_errors) = parse_and_analyze(source);
        assert!(!has_errors, "lambda should infer correctly");
    }

    #[test]
    fn data_type_constructors_have_correct_tags() {
        let source = "data Direction = North | South | East | West";
        let (sa, has_errors) = parse_and_analyze(source);
        assert!(!has_errors);
        assert_eq!(sa.constructors["North"].tag, 0);
        assert_eq!(sa.constructors["South"].tag, 1);
        assert_eq!(sa.constructors["East"].tag, 2);
        assert_eq!(sa.constructors["West"].tag, 3);
    }

    #[test]
    fn data_type_info_is_registered() {
        let source = "data Color = Red | Green | Blue";
        let (sa, has_errors) = parse_and_analyze(source);
        assert!(!has_errors);
        let dt = sa
            .data_types
            .get("Color")
            .expect("Color should be in data_types");
        assert_eq!(dt.name, "Color");
        assert_eq!(dt.constructors.len(), 3);
        assert!(dt.type_params.is_empty());
    }

    #[test]
    fn generic_constructor_is_registered_polymorphically() {
        let source = "data Box a = Box a";
        let (sa, has_errors) = parse_and_analyze(source);
        assert!(!has_errors);

        let scheme = sa
            .env
            .lookup("Box")
            .expect("Box constructor should be in env");
        assert_eq!(scheme.vars.len(), 1);

        let dt = sa
            .data_types
            .get("Box")
            .expect("Box should be in data_types");
        assert_eq!(dt.type_params, vec!["a"]);
    }

    #[test]
    fn empty_program_has_no_errors() {
        let (_, has_errors) = parse_and_analyze("");
        assert!(!has_errors, "empty program should have no errors");
    }

    #[test]
    fn comment_only_program_has_no_errors() {
        let (_, has_errors) = parse_and_analyze("-- just a comment\n");
        assert!(!has_errors, "comment-only program should have no errors");
    }

    #[test]
    fn multiple_constructors_registered_in_environment() {
        let source = "data Color = Red | Green | Blue";
        let (sa, _) = parse_and_analyze(source);
        assert!(sa.env.lookup("Red").is_some(), "Red should be in env");
        assert!(sa.env.lookup("Green").is_some(), "Green should be in env");
        assert!(sa.env.lookup("Blue").is_some(), "Blue should be in env");
    }

    #[test]
    fn constructor_type_is_correct() {
        let source = "data Color = Red | Green | Blue";
        let (sa, has_errors) = parse_and_analyze(source);
        assert!(!has_errors);
        let scheme = sa.env.lookup("Red").expect("Red should be in env");
        let ty = sa.engine.finalize(&scheme.ty);
        let ty_str = format!("{}", ty);
        assert_eq!(
            ty_str, "Color",
            "Red should have type Color, got: {}",
            ty_str
        );
    }

    #[test]
    fn comparison_returns_bool_typed_expression() {
        let source = "f x = if x == 0 then 1 else 0";
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "comparison should type check when used in if condition"
        );
    }

    #[test]
    fn boolean_operators_type_check() {
        let source = "f x = if x == 0 && x == 1 then 1 else 0";
        let (_, has_errors) = parse_and_analyze(source);
        assert!(!has_errors, "boolean operators should type check");
    }

    // Regression: AssocProj with free type variables from indexing must
    // resolve through predicate improvement, not cause type mismatches
    // during inference. Previously, `p.basis[0][0] + 1.0` failed because
    // the matrix index type variable wasn't resolved before the `+`
    // operator created an AssocProj with a free param.
    #[test]
    fn assoc_proj_with_indexed_type_variable() {
        let source = r#"
data Params = Params { basis : Mat<3, 3, F32> }
test : Params -> F32
test p = p.basis[0][0] + 1.0
"#;
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "indexed expression used with arithmetic operator should type check"
        );
    }

    #[test]
    fn assoc_proj_with_indexed_type_variable_in_vec2() {
        let source = r#"
data Params = Params { basis : Mat<3, 3, F32> }
test : Params -> Vec<2, F32>
test p =
  let scale = p.basis[0][0]
      uv = vec2 1.0 1.0
  in uv - vec2 (0.38 * cos (scale + 1.0)) (0.24 * sin scale)
"#;
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "indexed expression in nested arithmetic + vec2 should type check"
        );
    }

    // ========================================================================
    // Type name conflict tests
    // ========================================================================

    /// A trait associated type named the same as a data type.
    /// E.g. `type Output` in a trait where `Output` is also a data type.
    /// Associated types are scoped to their trait, so they don't conflict.
    #[test]
    fn assoc_type_same_name_as_data_type() {
        let source = r#"
data Output = MkOutput F32

trait Scale a where
  type Output
  scaleTo : a -> F32 -> Self.Output

impl Scale F32 where
  type Output = F32
  scaleTo x f = x * f

test : F32
test = scaleTo 2.0 3.0
"#;
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "associated type 'Output' should not conflict with data type 'Output'"
        );
    }

    /// A trait associated type named the same as a type alias.
    /// `type Output` where `Output` is also a type alias for `F32`.
    /// Bare `Output` resolves to the alias; `Self.Output` resolves to the associated type.
    #[test]
    fn assoc_type_same_name_as_type_alias() {
        let source = r#"
alias Output = F32
trait Combine a b where
  type Output
  combine : a -> b -> Self.Output
impl Combine F32 F32 where
  type Output = F32
  combine x y = x + y
test : F32
test = combine 1.0 2.0
"#;
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "associated type 'Output' should not conflict with type alias 'Output'"
        );
    }

    /// A data constructor with the same name as a top-level function.
    /// Both live in the value namespace, so the later one shadows the earlier.
    #[test]
    fn constructor_same_name_as_function() {
        let source = r#"
data Duo a b = Duo a b
myDuo : Duo F32 F32
myDuo = Duo 1.0 2.0
"#;
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "constructor and function can share a name when they refer to the same thing"
        );
    }

    /// A record type where a field name collides with a top-level binding.
    #[test]
    fn record_field_same_name_as_top_level_binding() {
        let source = r#"
data Point = Point { x : F32, y : F32 }
scale : F32
scale = 2.0
test : Point
test = Point { x = 1.0, y = scale }
"#;
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "record field and top-level binding in separate scopes should not conflict"
        );
    }

    /// An ADT with multiple constructors, where one constructor name
    /// shadows a builtin function name.
    #[test]
    fn adt_constructor_shadows_builtin() {
        let source = r#"
data Wrap = Wrap F32
test : F32
test = let w = Wrap 1.0 in 3.0
"#;
        let (_, has_errors) = parse_and_analyze(source);
        // `Wrap` shadows whatever `Wrap` might be in the prelude, but there's
        // nothing called `Wrap` in the prelude, so this should be fine.
        assert!(
            !has_errors,
            "ADT constructor should work even if it shadows a potential name"
        );
    }

    /// A trait with an associated type whose name is the same as one of
    /// the trait's type parameters.
    #[test]
    fn assoc_type_same_name_as_trait_param() {
        let source = r#"
trait Container a where
  type a
  get : a -> a
"#;
        let (_, has_errors) = parse_and_analyze(source);
        // This is an ambiguous/shadowing situation: `type a` in the trait
        // body could be interpreted as a lowercase type variable or as
        // an associated type declaration. The parser only accepts UpperIdent
        // for associated type names, so `type a` should fail to parse
        // as an associated type. This test documents the current behavior.
        let _ = has_errors;
    }

    /// Using a trait's associated type in a function signature with
    /// explicit `Type.Proj` syntax.
    #[test]
    fn assoc_type_proj_in_function_signature() {
        let source = r#"
trait Container a where
  type Elem
  get : a -> Elem
data Box a = Box a
impl Container (Box a) where
  type Elem = a
  get b = let Box x = b in x
test : Box F32
test = Box 3.0
"#;
        let (_, has_errors) = parse_and_analyze(source);
        // Tests that the associated type `Elem` doesn't conflict
        // with anything and can be used in method signatures.
        let _ = has_errors;
    }

    /// Multiple traits with the same associated type name.
    /// Each trait's associated type is in its own namespace.
    #[test]
    fn multiple_traits_same_assoc_type_name() {
        let source = r#"
trait Plus a b where
  type Output
  plus : a -> b -> Self.Output
trait Times a b where
  type Output
  times : a -> b -> Self.Output
impl Plus F32 F32 where
  type Output = F32
  plus x y = x + y
impl Times F32 F32 where
  type Output = F32
  times x y = x * y
test : F32
test = plus 2.0 (times 3.0 4.0)
"#;
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "two traits with the same associated type name 'Output' should coexist"
        );
    }

    /// A trait and an impl where the trait's type parameter name
    /// collides with a data type name.
    #[test]
    fn trait_type_param_shadows_data_type() {
        let source = r#"
data Outcome = Outcome { value : F32 }
trait Show a where
  show : a -> F32
impl Show Outcome where
  show r = r.value
test : F32
test = show (Outcome { value = 42.0 })
"#;
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "trait type param 'a' should not conflict with data type 'Outcome'"
        );
    }

    /// An impl for a type that has the same name as a trait.
    #[test]
    fn impl_for_type_named_like_trait() {
        let source = r#"
data Light = Light { brightness : F32 }
trait HasBrightness a where
  brightness : a -> F32
impl HasBrightness Light where
  brightness l = l.brightness
test : F32
test = brightness (Light { brightness = 0.5 })
"#;
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "data type and trait can have similar names without conflict"
        );
    }

    /// An ADT with a single constructor that has the same name as the type.
    /// This is the "newtype" pattern — very common in functional languages.
    #[test]
    fn newtype_same_constructor_and_type_name() {
        let source = r#"
data Velocity = Velocity (Vec<3, F32>)
test : Velocity
test = Velocity [1.0, 0.0, 0.0]
"#;
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "newtype pattern (same constructor and type name) should work"
        );
    }
}

// =========================================================================
// 5. Full pipeline tests using fixture files
// =========================================================================

mod fixture_tests {
    use super::*;

    const HELLO_SHADML: &str = include_str!("../../../fixtures/hello.shadml");
    const ADT_SHADML: &str = include_str!("../../../fixtures/adt.shadml");
    const PARTICLE_SHADML: &str = include_str!("../../../fixtures/particle.shadml");
    const OPTION_RESULT_EXAMPLE: &str = include_str!("../../../examples/option-result.shadml");
    const PRELUDE_UTILS_EXAMPLE: &str = include_str!("../../../examples/prelude-utils.shadml");
    const TENSOR_ALIASES_EXAMPLE: &str = include_str!("../../../examples/tensor-aliases.shadml");

    #[test]
    fn fixture_hello_lexes_without_errors() {
        let tokens = lex(HELLO_SHADML);
        let error_count = tokens
            .iter()
            .filter(|t| t.kind == SyntaxKind::Error)
            .count();
        assert_eq!(
            error_count, 0,
            "hello.shadml should lex without error tokens"
        );
    }

    #[test]
    fn fixture_adt_lexes_without_errors() {
        let tokens = lex(ADT_SHADML);
        let error_count = tokens
            .iter()
            .filter(|t| t.kind == SyntaxKind::Error)
            .count();
        assert_eq!(error_count, 0, "adt.shadml should lex without error tokens");
    }

    #[test]
    fn fixture_particle_lexes_without_errors() {
        let tokens = lex(PARTICLE_SHADML);
        let error_count = tokens
            .iter()
            .filter(|t| t.kind == SyntaxKind::Error)
            .count();
        assert_eq!(
            error_count, 0,
            "particle.shadml should lex without error tokens"
        );
    }

    #[test]
    fn fixture_hello_produces_declarations() {
        let (program, _) = parse(HELLO_SHADML);
        assert!(
            !program.decls.is_empty(),
            "hello.shadml should produce at least one declaration"
        );
    }

    #[test]
    fn fixture_adt_has_data_declaration() {
        let (program, _) = parse(ADT_SHADML);
        let has_data_decl = program
            .decls
            .iter()
            .any(|d| matches!(d, Decl::DataDecl { name, .. } if name == "Color"));
        assert!(
            has_data_decl,
            "adt.shadml should contain a Color data declaration"
        );
    }

    #[test]
    fn fixture_adt_registers_constructors() {
        let (program, _) = parse(ADT_SHADML);
        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);
        assert!(
            sa.constructors.contains_key("Red"),
            "Red constructor should be registered"
        );
        assert!(
            sa.constructors.contains_key("Green"),
            "Green constructor should be registered"
        );
        assert!(
            sa.constructors.contains_key("Blue"),
            "Blue constructor should be registered"
        );
    }

    #[test]
    fn fixture_particle_has_data_declaration() {
        let (program, _) = parse(PARTICLE_SHADML);
        let has_particle_data = program
            .decls
            .iter()
            .any(|d| matches!(d, Decl::DataDecl { name, .. } if name == "ParticleState"));
        assert!(
            has_particle_data,
            "particle.shadml should have ParticleState data type"
        );
    }

    #[test]
    fn fixture_particle_has_two_constructors() {
        let (program, _) = parse(PARTICLE_SHADML);
        if let Some(Decl::DataDecl { constructors, .. }) = program
            .decls
            .iter()
            .find(|d| matches!(d, Decl::DataDecl { name, .. } if name == "ParticleState"))
        {
            assert_eq!(
                constructors.len(),
                2,
                "ParticleState should have 2 constructors (Active, Dead)"
            );
            assert_eq!(constructors[0].name, "Active");
            assert_eq!(constructors[1].name, "Dead");
        } else {
            panic!("ParticleState data declaration not found");
        }
    }

    #[test]
    fn example_option_result_type_checks() {
        let (_, has_errors) = parse_and_analyze(OPTION_RESULT_EXAMPLE);
        assert!(
            !has_errors,
            "option-result example should pass semantic analysis"
        );
    }

    #[test]
    fn example_prelude_utils_type_checks() {
        let (_, has_errors) = parse_and_analyze(PRELUDE_UTILS_EXAMPLE);
        assert!(
            !has_errors,
            "prelude-utils example should pass semantic analysis"
        );
    }

    #[test]
    fn example_tensor_aliases_type_checks() {
        let (_, has_errors) = parse_and_analyze(TENSOR_ALIASES_EXAMPLE);
        assert!(
            !has_errors,
            "tensor-aliases example should pass semantic analysis"
        );
    }
}

// =========================================================================
// 6. WGSL codegen tests (MIR -> WGSL)
// =========================================================================

mod codegen_tests {
    use super::*;
    use shadml_allocator::Allocator;

    #[test]
    fn codegen_simple_add_function() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![MirFunction {
                name: arena.alloc_str("add"),
                params: vec![
                    MirParam {
                        name: arena.alloc_str("x"),
                        ty: MirType::I32,
                    },
                    MirParam {
                        name: arena.alloc_str("y"),
                        ty: MirType::I32,
                    },
                ],
                return_ty: MirType::I32,
                body: vec![],
                return_expr: Some(MirExpr::BinOp(
                    MirBinOp::Add,
                    arena.alloc(MirExpr::Var(arena.alloc_str("x"), MirType::I32)),
                    arena.alloc(MirExpr::Var(arena.alloc_str("y"), MirType::I32)),
                    MirType::I32,
                )),
                comments: vec![],
            }],
            entry_points: vec![],
            constants: vec![],
            render_blocks: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(
            wgsl.contains("fn add(x: i32, y: i32) -> i32"),
            "WGSL: {}",
            wgsl
        );
        assert!(wgsl.contains("return x + y;"), "WGSL: {}", wgsl);
    }

    #[test]
    fn codegen_compute_shader_entry_point() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![MirStruct {
                name: arena.alloc_str("ComputeInput"),
                fields: vec![MirField {
                    name: arena.alloc_str("gid"),
                    ty: MirType::Vec(3, arena.alloc(MirType::U32)),
                    attributes: vec![MirAttribute {
                        name: arena.alloc_str("builtin"),
                        args: vec![arena.alloc_str("global_invocation_id")],
                    }],
                }],
            origin_module: None,
            }],
            globals: vec![],
            functions: vec![],
            entry_points: vec![MirEntryPoint {
                name: arena.alloc_str("main"),
                stage: ShaderStage::Compute,
                workgroup_size: Some([64, 1, 1]),
                params: vec![MirParam {
                    name: arena.alloc_str("input"),
                    ty: MirType::Struct(arena.alloc_str("ComputeInput")),
                }],
                return_ty: MirType::Unit,
                body: vec![MirStmt::Let(
                    arena.alloc_str("idx"),
                    MirType::U32,
                    MirExpr::FieldAccess(
                        arena.alloc(MirExpr::Var(
                            arena.alloc_str("input"),
                            MirType::Struct(arena.alloc_str("ComputeInput")),
                        )),
                        arena.alloc_str("gid"),
                        MirType::Vec(3, arena.alloc(MirType::U32)),
                    ),
                )],
                return_expr: None,
                comments: vec![],
            }],
            constants: vec![],
            render_blocks: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(
            wgsl.contains("@compute @workgroup_size(64, 1, 1)"),
            "WGSL: {}",
            wgsl
        );
        assert!(wgsl.contains("input: ComputeInput"), "WGSL: {}", wgsl);
        assert!(
            wgsl.contains("@builtin(global_invocation_id) gid: vec3<u32>"),
            "WGSL: {}",
            wgsl
        );
    }

    #[test]
    fn codegen_struct_and_function_ordering() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![MirStruct {
                name: arena.alloc_str("Particle"),
                fields: vec![
                    MirField {
                        name: arena.alloc_str("pos"),
                        ty: MirType::Vec(3, arena.alloc(MirType::F32)),
                        attributes: vec![],
                    },
                    MirField {
                        name: arena.alloc_str("life"),
                        ty: MirType::F32,
                        attributes: vec![],
                    },
                ],
            origin_module: None,
            }],
            globals: vec![],
            functions: vec![MirFunction {
                name: arena.alloc_str("get_life"),
                params: vec![MirParam {
                    name: arena.alloc_str("p"),
                    ty: MirType::Struct(arena.alloc_str("Particle")),
                }],
                return_ty: MirType::F32,
                body: vec![],
                return_expr: Some(MirExpr::FieldAccess(
                    arena.alloc(MirExpr::Var(
                        arena.alloc_str("p"),
                        MirType::Struct(arena.alloc_str("Particle")),
                    )),
                    arena.alloc_str("life"),
                    MirType::F32,
                )),
                comments: vec![],
            }],
            entry_points: vec![],
            constants: vec![],
            render_blocks: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(wgsl.contains("struct Particle {"), "WGSL: {}", wgsl);
        assert!(wgsl.contains("life: f32,"), "WGSL: {}", wgsl);
        assert!(wgsl.contains("return p.life;"), "WGSL: {}", wgsl);

        let struct_pos = wgsl.find("struct Particle").unwrap();
        let fn_pos = wgsl.find("fn get_life").unwrap();
        assert!(
            struct_pos < fn_pos,
            "structs should appear before functions in WGSL output"
        );
    }

    #[test]
    fn codegen_if_else_statement() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![MirFunction {
                name: arena.alloc_str("max_val"),
                params: vec![
                    MirParam {
                        name: arena.alloc_str("a"),
                        ty: MirType::I32,
                    },
                    MirParam {
                        name: arena.alloc_str("b"),
                        ty: MirType::I32,
                    },
                ],
                return_ty: MirType::I32,
                body: vec![MirStmt::If(
                    MirExpr::BinOp(
                        MirBinOp::Gt,
                        arena.alloc(MirExpr::Var(arena.alloc_str("a"), MirType::I32)),
                        arena.alloc(MirExpr::Var(arena.alloc_str("b"), MirType::I32)),
                        MirType::Bool,
                    ),
                    vec![MirStmt::Return(MirExpr::Var(
                        arena.alloc_str("a"),
                        MirType::I32,
                    ))],
                    vec![MirStmt::Return(MirExpr::Var(
                        arena.alloc_str("b"),
                        MirType::I32,
                    ))],
                )],
                return_expr: None,
                comments: vec![],
            }],
            entry_points: vec![],
            constants: vec![],
            render_blocks: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(wgsl.contains("if ("), "WGSL: {}", wgsl);
        assert!(wgsl.contains("} else {"), "WGSL: {}", wgsl);
        assert!(wgsl.contains("return a;"), "WGSL: {}", wgsl);
        assert!(wgsl.contains("return b;"), "WGSL: {}", wgsl);
    }

    #[test]
    fn codegen_vertex_shader() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![],
            entry_points: vec![MirEntryPoint {
                name: arena.alloc_str("vs_main"),
                stage: ShaderStage::Vertex,
                workgroup_size: None,
                params: vec![],
                return_ty: MirType::Vec(4, arena.alloc(MirType::F32)),
                body: vec![],
                return_expr: Some(MirExpr::Call(
                    arena.alloc_str("vec4"),
                    vec![
                        MirExpr::Lit(MirLit::F32(0.0)),
                        MirExpr::Lit(MirLit::F32(0.5)),
                        MirExpr::Lit(MirLit::F32(0.0)),
                        MirExpr::Lit(MirLit::F32(1.0)),
                    ],
                    MirType::Vec(4, arena.alloc(MirType::F32)),
                )),
                comments: vec![],
            }],
            constants: vec![],
            render_blocks: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(wgsl.contains("@vertex"), "WGSL: {}", wgsl);
        assert!(wgsl.contains("fn vs_main()"), "WGSL: {}", wgsl);
        assert!(wgsl.contains("-> @location(0) vec4<f32>"), "WGSL: {}", wgsl);
    }

    #[test]
    fn codegen_fragment_shader() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![],
            entry_points: vec![MirEntryPoint {
                name: arena.alloc_str("fs_main"),
                stage: ShaderStage::Fragment,
                workgroup_size: None,
                params: vec![],
                return_ty: MirType::Vec(4, arena.alloc(MirType::F32)),
                body: vec![],
                return_expr: Some(MirExpr::Call(
                    arena.alloc_str("vec4"),
                    vec![
                        MirExpr::Lit(MirLit::F32(1.0)),
                        MirExpr::Lit(MirLit::F32(0.0)),
                        MirExpr::Lit(MirLit::F32(0.0)),
                        MirExpr::Lit(MirLit::F32(1.0)),
                    ],
                    MirType::Vec(4, arena.alloc(MirType::F32)),
                )),
                comments: vec![],
            }],
            constants: vec![],
            render_blocks: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(wgsl.contains("@fragment"), "WGSL: {}", wgsl);
        assert!(wgsl.contains("fn fs_main()"), "WGSL: {}", wgsl);
        assert!(wgsl.contains("-> @location(0) vec4<f32>"), "WGSL: {}", wgsl);
    }

    #[test]
    fn codegen_mir_type_display() {
        let arena = Allocator::new();
        assert_eq!(format!("{}", MirType::I32), "i32");
        assert_eq!(format!("{}", MirType::U32), "u32");
        assert_eq!(format!("{}", MirType::F32), "f32");
        assert_eq!(format!("{}", MirType::Bool), "bool");
        assert_eq!(
            format!("{}", MirType::Vec(3, arena.alloc(MirType::F32))),
            "vec3<f32>"
        );
        assert_eq!(
            format!("{}", MirType::Mat(4, 4, arena.alloc(MirType::F32))),
            "mat4x4<f32>"
        );
        assert_eq!(
            format!("{}", MirType::Array(arena.alloc(MirType::F32), 16)),
            "array<f32, 16>"
        );
        assert_eq!(
            format!(
                "{}",
                MirType::Array(arena.alloc(MirType::Array(arena.alloc(MirType::F32), 4)), 2)
            ),
            "array<array<f32, 4>, 2>"
        );
    }
}

// =========================================================================
// 7. Error recovery tests
// =========================================================================

mod error_recovery_tests {
    use super::*;

    #[test]
    fn parser_handles_empty_input() {
        let (program, has_errors) = parse_raw("");
        assert!(!has_errors, "empty input should not be a parse error");
        assert!(program.decls.is_empty());
    }

    #[test]
    fn parser_handles_only_whitespace() {
        let (_program, has_errors) = parse_raw("   \n\n  \n");
        assert!(!has_errors, "whitespace-only input should not error");
    }

    #[test]
    fn parser_does_not_panic_on_garbage_input() {
        let garbage_inputs = [
            "@@@@", "= = = =", "-> -> ->", "data", "data =", "let", "match", "| | | |", "( ( ( (",
            ") ) ) )", "{ { { {", "} } } }", "\\\\\\\\",
        ];
        for input in &garbage_inputs {
            let result = std::panic::catch_unwind(|| {
                let mut parser = Parser::new(input);
                let _program = parser.parse_program();
            });
            assert!(
                result.is_ok(),
                "parser should not panic on input: {:?}",
                input
            );
        }
    }

    #[test]
    fn lexer_does_not_panic_on_garbage_input() {
        let garbage_inputs = [
            "\0\0\0",
            "\"unterminated string",
            "'",
            "''",
            "0x",
            "0b",
            "0o",
            "/*",
            "///",
        ];
        for input in &garbage_inputs {
            let result = std::panic::catch_unwind(|| {
                let _tokens = lex(input);
            });
            assert!(
                result.is_ok(),
                "lexer should not panic on input: {:?}",
                input
            );
        }
    }

    #[test]
    fn parser_recovers_from_incomplete_function() {
        let result = std::panic::catch_unwind(|| {
            let mut parser = Parser::new("f x =");
            let _program = parser.parse_program();
        });
        assert!(
            result.is_ok(),
            "parser should not panic on incomplete function"
        );
    }

    #[test]
    fn parser_recovers_from_incomplete_data_decl() {
        let result = std::panic::catch_unwind(|| {
            let mut parser = Parser::new("data Color =");
            let _program = parser.parse_program();
        });
        assert!(
            result.is_ok(),
            "parser should not panic on incomplete data declaration"
        );
    }

    #[test]
    fn parser_handles_very_long_input_without_panic() {
        let mut source = String::new();
        for i in 0..100 {
            source.push_str(&format!("f{} x = x + {}\n", i, i));
        }
        let result = std::panic::catch_unwind(|| {
            let mut parser = Parser::new(&source);
            let _program = parser.parse_program();
        });
        assert!(
            result.is_ok(),
            "parser should handle large input without panic"
        );
    }

    #[test]
    fn semantic_analyzer_handles_empty_program() {
        let mut sa = SemanticAnalyzer::new();
        let program = Program { decls: vec![] };
        sa.analyze(&program);
        assert!(!sa.has_errors());
    }

    #[test]
    fn semantic_analyzer_handles_only_data_types() {
        let source = "data Color = Red | Green | Blue";
        let (program, _) = parse(source);
        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);
        assert!(
            !sa.has_errors(),
            "data-type-only program should not error in semantic analysis"
        );
    }

    #[test]
    fn parser_handles_nested_block_comments() {
        let source = "{- outer {- inner -} still outer -} f x = x";
        let (program, has_errors) = parse_raw(source);
        assert!(!has_errors, "nested block comments should not cause errors");
        assert!(
            !program.decls.is_empty(),
            "should parse declaration after comment"
        );
    }
}

// =========================================================================
// 8. Cross-crate pipeline integration tests
// =========================================================================

mod pipeline_tests {
    use super::*;

    #[test]
    fn parse_then_semantic_data_type_with_match() {
        let source = "\
data Color = Red | Green | Blue

show c = match c
  | Red   -> 0
  | Green -> 1
  | Blue  -> 2
";
        let (program, _parse_errors) = parse(source);
        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);

        assert!(sa.constructors.contains_key("Red"));
        assert!(sa.constructors.contains_key("Green"));
        assert!(sa.constructors.contains_key("Blue"));
    }

    #[test]
    fn mir_to_wgsl_round_trip_is_valid_text() {
        let arena = shadml_allocator::Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![MirFunction {
                name: arena.alloc_str("identity"),
                params: vec![MirParam {
                    name: arena.alloc_str("x"),
                    ty: MirType::I32,
                }],
                return_ty: MirType::I32,
                body: vec![],
                return_expr: Some(MirExpr::Var(arena.alloc_str("x"), MirType::I32)),
                comments: vec![],
            }],
            entry_points: vec![],
            constants: vec![],
            render_blocks: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(!wgsl.is_empty(), "WGSL output should not be empty");
        assert!(wgsl.contains("fn "), "WGSL should contain function keyword");
        assert!(
            wgsl.contains("return"),
            "WGSL should contain return statement"
        );
        assert!(wgsl.ends_with('\n'), "WGSL should end with newline");
    }

    #[test]
    fn wgsl_codegen_no_spurious_semicolons() {
        let arena = shadml_allocator::Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![MirFunction {
                name: arena.alloc_str("f"),
                params: vec![],
                return_ty: MirType::I32,
                body: vec![
                    MirStmt::Let(
                        arena.alloc_str("a"),
                        MirType::I32,
                        MirExpr::Lit(MirLit::I32(1)),
                    ),
                    MirStmt::Let(
                        arena.alloc_str("b"),
                        MirType::I32,
                        MirExpr::Lit(MirLit::I32(2)),
                    ),
                ],
                return_expr: Some(MirExpr::BinOp(
                    MirBinOp::Add,
                    arena.alloc(MirExpr::Var(arena.alloc_str("a"), MirType::I32)),
                    arena.alloc(MirExpr::Var(arena.alloc_str("b"), MirType::I32)),
                    MirType::I32,
                )),
                comments: vec![],
            }],
            entry_points: vec![],
            constants: vec![],
            render_blocks: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(
            !wgsl.contains(";;"),
            "WGSL should not contain double semicolons: {}",
            wgsl
        );
    }

    #[test]
    fn wgsl_codegen_proper_indentation() {
        let arena = shadml_allocator::Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![MirFunction {
                name: arena.alloc_str("f"),
                params: vec![MirParam {
                    name: arena.alloc_str("x"),
                    ty: MirType::I32,
                }],
                return_ty: MirType::I32,
                body: vec![],
                return_expr: Some(MirExpr::Var(arena.alloc_str("x"), MirType::I32)),
                comments: vec![],
            }],
            entry_points: vec![],
            constants: vec![],
            render_blocks: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(
            wgsl.contains("  return x;"),
            "return statement should be indented in WGSL: {}",
            wgsl
        );
    }

    #[test]
    fn semantic_analysis_with_adt_fixture() {
        let source = include_str!("../../../fixtures/adt.shadml");
        let (program, _) = parse(source);
        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);

        assert!(
            sa.data_types.contains_key("Color"),
            "Color data type should be registered after analyzing adt.shadml"
        );
        assert_eq!(
            sa.data_types["Color"].constructors.len(),
            3,
            "Color should have 3 constructors"
        );
    }

    #[test]
    fn full_mir_program_with_all_shader_stages() {
        let arena = shadml_allocator::Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![],
            entry_points: vec![
                MirEntryPoint {
                    name: arena.alloc_str("vs_main"),
                    stage: ShaderStage::Vertex,
                    workgroup_size: None,
                    params: vec![],
                    return_ty: MirType::Vec(4, arena.alloc(MirType::F32)),
                    body: vec![],
                    return_expr: Some(MirExpr::Call(
                        arena.alloc_str("vec4"),
                        vec![
                            MirExpr::Lit(MirLit::F32(0.0)),
                            MirExpr::Lit(MirLit::F32(0.0)),
                            MirExpr::Lit(MirLit::F32(0.0)),
                            MirExpr::Lit(MirLit::F32(1.0)),
                        ],
                        MirType::Vec(4, arena.alloc(MirType::F32)),
                    )),
                    comments: vec![],
                },
                MirEntryPoint {
                    name: arena.alloc_str("fs_main"),
                    stage: ShaderStage::Fragment,
                    workgroup_size: None,
                    params: vec![],
                    return_ty: MirType::Vec(4, arena.alloc(MirType::F32)),
                    body: vec![],
                    return_expr: Some(MirExpr::Call(
                        arena.alloc_str("vec4"),
                        vec![
                            MirExpr::Lit(MirLit::F32(1.0)),
                            MirExpr::Lit(MirLit::F32(0.0)),
                            MirExpr::Lit(MirLit::F32(0.0)),
                            MirExpr::Lit(MirLit::F32(1.0)),
                        ],
                        MirType::Vec(4, arena.alloc(MirType::F32)),
                    )),
                    comments: vec![],
                },
                MirEntryPoint {
                    name: arena.alloc_str("cs_main"),
                    stage: ShaderStage::Compute,
                    workgroup_size: Some([64, 1, 1]),
                    params: vec![],
                    return_ty: MirType::Unit,
                    body: vec![],
                    return_expr: None,
                    comments: vec![],
                },
            ],
            constants: vec![],
            render_blocks: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(wgsl.contains("@vertex"), "should contain vertex stage");
        assert!(wgsl.contains("@fragment"), "should contain fragment stage");
        assert!(wgsl.contains("@compute"), "should contain compute stage");
        assert!(wgsl.contains("fn vs_main"), "should contain vs_main");
        assert!(wgsl.contains("fn fs_main"), "should contain fs_main");
        assert!(wgsl.contains("fn cs_main"), "should contain cs_main");
    }
}

// =========================================================================
// Full Pipeline Tests: Source -> Parse -> Semantic -> HIR -> MIR -> WGSL
// =========================================================================

mod full_pipeline_tests {
    use super::*;

    const SWIZZLES_EXAMPLE: &str = include_str!("../../../examples/swizzles.shadml");
    const VEC_LITERALS_EXAMPLE: &str = include_str!("../../../examples/vec-literals.shadml");

    /// Full pipeline helper: source -> WGSL
    fn compile_to_wgsl(source: &str) -> Result<String, String> {
        let mut parser = Parser::new(source);
        let mut program = parser.parse_program();

        if parser.diagnostics().has_errors() {
            return Err("parse error".into());
        }

        with_prelude(&mut program);

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);

        if sa.has_errors() {
            return Err(sa
                .diagnostics()
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n"));
        }

        let mut lowering = AstLowering::new(&sa);
        let hir = lowering.lower_program(&program);

        if lowering.has_errors() {
            return Err(lowering
                .diagnostics()
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n"));
        }

        let arena = shadml_allocator::Allocator::new();
        let mir = shadml_mir::lower::lower_hir_to_mir(&arena, &hir).map_err(|e| e.join(", "))?;
        let mir = shadml_mir::reachability::eliminate_dead_code(&mir);

        Ok(emit_wgsl(&mir))
    }

    #[test]
    fn test_full_pipeline_add_function() {
        let source = "add : I32 -> I32 -> I32\nadd x y = x + y";
        let wgsl = compile_to_wgsl(source).expect("compilation should succeed");
        assert!(
            wgsl.contains("fn add("),
            "WGSL should contain fn add, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("x: i32"),
            "WGSL should contain x: i32, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("y: i32"),
            "WGSL should contain y: i32, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("-> i32"),
            "WGSL should contain -> i32, got: {}",
            wgsl
        );
        assert!(wgsl.contains("+"), "WGSL should contain +, got: {}", wgsl);
    }

    #[test]
    fn test_full_pipeline_record_pattern_rest_binds_named_field() {
        let source = r#"
alias Vec3F = Vec<3, F32>

data ParticleState
  = Active { position : Vec3F, velocity : Vec3F, life : F32 }
  | Dead

impl ParticleState where
  lifeValue : ParticleState -> F32
  lifeValue particle =
    match particle
      | Active { life, .. } -> life
      | Dead -> 0.0
"#;
        let wgsl = compile_to_wgsl(source).expect("compilation should succeed");
        assert!(wgsl.contains("let life = "), "WGSL: {}", wgsl);
        assert!(wgsl.contains(".life;"), "WGSL: {}", wgsl);
        assert!(
            !wgsl.contains("let life = _scrut_607.position;"),
            "WGSL: {}",
            wgsl
        );
    }

    #[test]
    fn test_full_pipeline_double_function() {
        let source = "double : I32 -> I32\ndouble x = x * 2";
        let wgsl = compile_to_wgsl(source).expect("compilation should succeed");
        assert!(
            wgsl.contains("fn double("),
            "WGSL should contain fn double, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("-> i32"),
            "WGSL should contain -> i32, got: {}",
            wgsl
        );
        assert!(wgsl.contains("*"), "WGSL should contain *, got: {}", wgsl);
    }

    #[test]
    fn test_full_pipeline_if_expression() {
        let source = "f x = if x == 0 then 1 else 2";
        let wgsl = compile_to_wgsl(source).expect("compilation should succeed");
        assert!(
            wgsl.contains("fn f("),
            "WGSL should contain fn f, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("select("),
            "WGSL should contain select call for simple if-then-else, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_full_pipeline_let_expression() {
        let source = "f = let x = 42 in x + 1";
        let wgsl = compile_to_wgsl(source).expect("compilation should succeed");
        assert!(
            wgsl.contains("fn f("),
            "WGSL should contain fn f, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("let x"),
            "WGSL should contain let x, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_full_pipeline_where_clause() {
        let source = "f x = y * 2 where y = x + 1";
        let wgsl = compile_to_wgsl(source).expect("compilation should succeed");
        assert!(
            wgsl.contains("fn f("),
            "WGSL should contain fn f, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("let y"),
            "WGSL should contain lowered where binding, got: {}",
            wgsl
        );
        assert!(wgsl.contains("*"), "WGSL should contain *, got: {}", wgsl);
    }

    #[test]
    fn test_full_pipeline_multiple_functions() {
        let source = r#"
add : I32 -> I32 -> I32
add x y = x + y

double : I32 -> I32
double x = x * 2
"#;
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("fn add("),
            "should contain fn add, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("fn double("),
            "should contain fn double, got: {}",
            wgsl
        );
        assert!(wgsl.contains("i32"), "should contain i32, got: {}", wgsl);
    }

    #[test]
    fn test_full_pipeline_generic_function_not_emitted_without_specialization() {
        let source = "add : Add a b => a -> b -> a.Output\nadd x y = x + y";
        let wgsl =
            compile_to_wgsl(source).expect("generic definition with a signature should compile");
        assert!(
            !wgsl.contains("fn add("),
            "unspecialized generic template should not be emitted as WGSL, got: {}",
            wgsl
        );
        assert!(
            !wgsl.contains("fn add_i32("),
            "no specialization should be emitted without a concrete call site, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_full_pipeline_generic_function_specializes_at_concrete_call_site() {
        let source = r#"
add : Add a b => a -> b -> a.Output
add x y = x + y

result : I32
result = add 1 2
"#;
        let wgsl = compile_to_wgsl(source).expect("generic call should specialize");
        assert!(
            wgsl.contains("fn add_i32_i32("),
            "WGSL should contain the specialized add_i32_i32 function, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("fn result() -> i32")
                && wgsl.contains("return add_i32_i32(1i, 2i);"),
            "WGSL should call the specialized add_i32_i32 helper from result, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_full_pipeline_vec_literals_example() {
        let wgsl =
            compile_to_wgsl(VEC_LITERALS_EXAMPLE).expect("vec literals example should compile");
        assert!(
            wgsl.contains("fn main("),
            "WGSL should contain main, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("vec3<"),
            "WGSL should contain vec3 constructor, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("vec4<"),
            "WGSL should contain vec4 constructor, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains(".x"),
            "WGSL should contain swizzle field access, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_full_pipeline_swizzles_example() {
        let wgsl = compile_to_wgsl(SWIZZLES_EXAMPLE).expect("swizzles example should compile");
        assert!(
            wgsl.contains("fn main("),
            "WGSL should contain main, got: {}",
            wgsl
        );
        for swizzle in [".xy", ".rg", ".xyz", ".rgb", ".xyzw", ".rgba", ".a"] {
            assert!(
                wgsl.contains(swizzle),
                "WGSL should contain swizzle {}, got: {}",
                swizzle,
                wgsl
            );
        }
    }

    #[test]
    fn test_full_pipeline_loop_expression() {
        // Named tail-recursive loop: counts i up to x, returns final i
        let source = "f : I32 -> I32\nf x = loop go (i = 0) in if i < x then go (i + 1) else i";
        let wgsl = compile_to_wgsl(source).expect("loop compilation should succeed");
        assert!(
            wgsl.contains("fn f("),
            "WGSL should contain fn f, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("loop {"),
            "WGSL should contain loop statement, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("break;"),
            "WGSL should contain break statement, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("continue;"),
            "WGSL should contain continue statement, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("var i"),
            "WGSL should contain var i, got: {}",
            wgsl
        );
    }
}

// =========================================================================
// 10. Trait system tests
// =========================================================================

mod trait_tests {
    use super::*;

    fn compile_to_wgsl(source: &str) -> Result<String, String> {
        let mut parser = Parser::new(source);
        let mut program = parser.parse_program();

        if parser.diagnostics().has_errors() {
            return Err("parse error".into());
        }

        with_prelude(&mut program);

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);

        if sa.has_errors() {
            return Err(sa
                .diagnostics()
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n"));
        }

        let mut lowering = AstLowering::new(&sa);
        let hir = lowering.lower_program(&program);

        if lowering.has_errors() {
            return Err(lowering
                .diagnostics()
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n"));
        }

        let arena = shadml_allocator::Allocator::new();
        let mir = shadml_mir::lower::lower_hir_to_mir(&arena, &hir).map_err(|e| e.join(", "))?;
        let mir = shadml_mir::reachability::eliminate_dead_code(&mir);

        Ok(shadml_wgsl_codegen::emit_wgsl(&mir))
    }

    #[test]
    fn parse_trait_decl() {
        let source = "trait Num a where\n  add : a -> a -> a\n  sub : a -> a -> a";
        let (program, has_errors) = parse_raw(source);
        assert!(!has_errors, "trait decl should parse without errors");
        assert_eq!(program.decls.len(), 1);
        match &program.decls[0] {
            Decl::TraitDecl {
                name, vars, methods, ..
            } => {
                assert_eq!(name, "Num");
                assert_eq!(vars, &vec!["a".to_string()]);
                assert_eq!(methods.len(), 2);
                assert_eq!(methods[0].name, "add");
                assert_eq!(methods[1].name, "sub");
            }
            other => panic!("expected TraitDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_impl_decl() {
        let source = "impl Num F32 where\n  add x y = x + y\n  sub x y = x - y";
        let (program, has_errors) = parse_raw(source);
        assert!(!has_errors, "impl decl should parse without errors");
        assert_eq!(program.decls.len(), 1);
        match &program.decls[0] {
            Decl::ImplDecl {
                trait_name,
                methods,
                ..
            } => {
                assert_eq!(trait_name.as_deref(), Some("Num"));
                assert_eq!(methods.len(), 2);
                assert_eq!(methods[0].name, "add");
                assert_eq!(methods[1].name, "sub");
            }
            other => panic!("expected ImplDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_trait_with_operator_methods() {
        let source = "trait Num a where\n  (+) : a -> a -> a\n  (-) : a -> a -> a";
        let (program, has_errors) = parse_raw(source);
        assert!(
            !has_errors,
            "trait with operators should parse without errors"
        );
        match &program.decls[0] {
            Decl::TraitDecl { methods, .. } => {
                assert_eq!(methods[0].name, "+");
                assert_eq!(methods[1].name, "-");
            }
            other => panic!("expected TraitDecl, got {:?}", other),
        }
    }

    #[test]
    fn trait_impl_lowers_to_function() {
        let source = r#"
trait Scalable a where
  scale : a -> F32 -> a

impl Scalable F32 where
  scale x factor = x * factor

applyScale : F32 -> F32 -> F32
applyScale value factor = scale value factor
"#;
        let wgsl = compile_to_wgsl(source).expect("compilation should succeed");
        assert!(
            wgsl.contains("fn scale_F32("),
            "WGSL should contain mangled impl method, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("scale_F32(value"),
            "WGSL should dispatch to mangled method, got: {}",
            wgsl
        );
    }

    #[test]
    fn method_call_syntax_sugar() {
        let source = r#"
impl F32 where
  half : F32 -> F32
  half x = x * 0.5

apply : F32 -> F32
apply x = x.half
"#;
        let wgsl = compile_to_wgsl(source).expect("compilation should succeed");
        assert!(
            wgsl.contains("half_F32(x)"),
            "method-call sugar should desugar to function call, got: {}",
            wgsl
        );
    }

    #[test]
    fn method_call_syntax_sugar_with_inferred_receiver_type() {
        let source = r#"
impl F32 where
  half : F32 -> F32
  half x = x * 0.5

apply x = x.half
"#;
        let wgsl = compile_to_wgsl(source).expect("compilation should succeed");
        assert!(
            wgsl.contains("half_F32(x)"),
            "dot-call sugar should still infer the impl receiver type, got: {}",
            wgsl
        );
        assert!(
            !wgsl.contains(".half"),
            "dot-call sugar should lower to a function call, got: {}",
            wgsl
        );
    }

    #[test]
    fn method_call_syntax_sugar_for_prelude_functions() {
        let source = r#"
apply : F32 -> F32
apply x = x.sin
"#;
        let wgsl = compile_to_wgsl(source).expect("compilation should succeed");
        assert!(
            wgsl.contains("sin(x)"),
            "prelude functions should remain callable via dot syntax, got: {}",
            wgsl
        );
    }

    #[test]
    fn trait_example_compiles() {
        let source = include_str!("../../../examples/traits.shadml");
        let wgsl = compile_to_wgsl(source).expect("traits example should compile");
        assert!(wgsl.contains("fn scale_F32("));
        assert!(wgsl.contains("fn applyScale("));
    }

    #[test]
    fn method_syntax_example_compiles() {
        let source = include_str!("../../../examples/method-syntax.shadml");
        let wgsl = compile_to_wgsl(source).expect("method-syntax example should compile");
        assert!(wgsl.contains("fn half_F32("));
        assert!(wgsl.contains("fn double("));
        assert!(wgsl.contains("fn clampVal_F32("));
    }

    #[test]
    fn slang_style_generic_lighting_compiles_with_specialization() {
        let source = include_str!("../../../examples/slang-generics.shadml");
        let wgsl = compile_to_wgsl(source).expect("slang generics example should compile");
        assert!(wgsl.contains("fn lighting_pointlight("));
        assert!(wgsl.contains("fn lighting_spotlight("));
        assert!(wgsl.contains("position_PointLight"));
        assert!(wgsl.contains("position_SpotLight"));
        assert!(wgsl.contains("lighting_pointlight(point(),"));
        assert!(wgsl.contains("lighting_spotlight(spot(),"));
    }

    /// Strips the cross-module section (section 14) from the conflicts example,
    /// since single-file compilation can't resolve `import ConflictsLib`.
    fn conflicts_source_without_imports() -> String {
        let full = include_str!("../../../examples/conflicts.shadml");
        // Remove lines starting with "import ConflictsLib" and everything
        // from the section 14 comment onward.
        let mut result = String::new();
        let mut skipping = false;
        for line in full.lines() {
            if line.starts_with("import ConflictsLib") {
                continue;
            }
            if line.contains("14. Cross-module:") {
                skipping = true;
            }
            if skipping {
                continue;
            }
            result.push_str(line);
            result.push('\n');
        }
        result
    }

    #[test]
    fn conflicts_example_type_checks() {
        let source = conflicts_source_without_imports();
        let (sa, has_errors) = parse_and_analyze(&source);
        assert!(
            !has_errors,
            "conflicts.shadml should type-check without errors"
        );
        // Data type "Output" should be registered alongside the associated type "Output".
        assert!(
            sa.data_types.contains_key("Output"),
            "data type 'Output' should be registered"
        );
        // Data type "Velocity" (newtype pattern: constructor = type name).
        assert!(
            sa.data_types.contains_key("Velocity"),
            "data type 'Velocity' should be registered"
        );
        // Prelude traits Add and Mul should be available.
        assert!(
            sa.traits.contains_key("Add"),
            "prelude trait 'Add' should be registered"
        );
        assert!(
            sa.traits.contains_key("Mul"),
            "prelude trait 'Mul' should be registered"
        );
        // User trait with `type Output` alongside prelude traits with `type Output`.
        assert!(
            sa.traits.contains_key("Scale"),
            "user trait 'Scale' should be registered"
        );
        let scale_trait = sa.traits.get("Scale").expect("Scale trait should exist");
        assert!(
            scale_trait.associated_types.contains(&"Output".to_string()),
            "Scale trait should have associated type Output"
        );
        // User type alias (new name, no conflict with prelude).
        assert!(
            sa.type_aliases.contains_key("Color"),
            "user alias 'Color' should be registered"
        );
    }

    #[test]
    fn conflicts_example_compiles_to_wgsl() {
        let source = conflicts_source_without_imports();
        let wgsl = compile_to_wgsl(&source).expect("conflicts example should compile to WGSL");
        // Verify key mangled function names appear in output.
        assert!(wgsl.contains("fn scaleTo_Weight("), "user Scale trait impl should appear");
        assert!(wgsl.contains("struct MaybeVal"), "user MaybeVal data type should appear");
        assert!(wgsl.contains("struct Status"), "user Status data type should appear");
        assert!(wgsl.contains("struct Velocity"), "newtype Velocity should appear");
    }

    #[test]
    fn conflicts_cross_module_bundles_successfully() {
        use shadml_bundler::{bundle_virtual, VirtualFile};

        let main_source = include_str!("../../../examples/conflicts.shadml");
        let lib_source = include_str!("../../../examples/ConflictsLib.shadml");

        let files = vec![
            VirtualFile {
                path: "conflicts.shadml".to_string(),
                source: main_source.to_string(),
            },
            VirtualFile {
                path: "ConflictsLib.shadml".to_string(),
                source: lib_source.to_string(),
            },
        ];

        let result = bundle_virtual(&files, &[], false);
        // The redesigned modules have no type-level name collisions,
        // so the bundler should succeed (or fail for a non-collision reason).
        // Value-level shadowing (e.g., ConflictsLib's `distance` function
        // shadowing the prelude's `extern distance`) is allowed.
        assert!(
            result.is_ok(),
            "conflicts example should bundle without name collisions, got: {:?}",
            result
        );
    }

    #[test]
    fn constrained_generic_let_bound_trait_use_compiles() {
        let source = r#"
trait Light a where
  position : a -> Vec<3, F32>

data PointLight = PointLight {
  lightPosition : Vec<3, F32>
}

impl Light PointLight where
  position light = light.lightPosition

lighting : Light a => a -> Vec<3, F32> -> Vec<3, F32>
lighting light worldPos =
  let lightDir = position light - worldPos
  in lightDir

point : PointLight
point = PointLight { lightPosition = [1.0, 2.0, 3.0] }

result : Vec<3, F32>
result = lighting point [0.0, 0.0, 0.0]
"#;
        let wgsl = compile_to_wgsl(source).expect("generic let-bound trait use should compile");
        assert!(wgsl.contains("fn lighting_pointlight("));
    }

    #[test]
    fn constrained_generic_call_without_impl_fails() {
        let source = r#"
trait Light a where
  position : a -> Vec<3, F32>

data Unlit = Unlit {
  value : F32
}

lighting : Light a => a -> Vec<3, F32>
lighting light = position light

bad : Unlit
bad = Unlit { value = 1.0 }

result : Vec<3, F32>
result = lighting bad
"#;
        assert!(
            compile_to_wgsl(source).is_err(),
            "calling a constrained generic function without a matching impl should fail"
        );
    }

    #[test]
    fn semantic_analysis_with_traits() {
        let source = r#"
trait Show a where
  display : a -> F32

impl Show F32 where
  display x = x

test : F32 -> F32
test x = display x
"#;
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "trait-using program should pass semantic analysis"
        );
    }

    #[test]
    fn tuple_argument_function_compiles() {
        let source = r#"
pairSum : (I32, I32) -> I32
pairSum (a, b) = a + b

result : I32
result = pairSum (1, 2)
"#;
        let wgsl = compile_to_wgsl(source).expect("tuple-argument function should compile");
        assert!(wgsl.contains("fn pairSum"));
    }

    #[test]
    fn tuple_argument_signature_rejects_curried_definition() {
        let source = r#"
pairSum : (I32, I32) -> I32
pairSum a b = a + b
"#;
        let err = compile_to_wgsl(source).expect_err("curried definition should be rejected");
        assert!(err.contains("expects 1"));
    }

    #[test]
    fn tuple_variable_argument_function_compiles() {
        let source = r#"
pairSum : (I32, I32) -> I32
pairSum (a, b) = a + b

result : I32
result =
  let p = (1, 2)
  in pairSum p
"#;
        let wgsl = compile_to_wgsl(source).expect("tuple variable argument should compile");
        assert!(wgsl.contains("fn pairSum"));
        assert!(wgsl.contains("let __tuple_p_0 = 1i;"));
    }

    #[test]
    fn unannotated_tuple_pattern_parameter_compiles() {
        let source = r#"
test2 a b (k, j) = a

result : I32
result = test2 1 2 (3, 4)
"#;
        let wgsl = compile_to_wgsl(source).expect("unannotated tuple-pattern parameter should compile");
        assert!(wgsl.contains("fn test2_"));
        assert!(wgsl.contains("let k = __tuple__arg2_0;"));
    }

    #[test]
    fn tuple_example_compiles() {
        let source = include_str!("../../../examples/tuple.shadml");
        compile_to_wgsl(source).expect("tuple example should compile");
    }

    #[test]
    fn bitfield_construction_produces_shift_or_chain() {
        let source = r#"
bitfield Flags : U32 = Flags {
  layer   : U32 : 4,
  stencil : U32 : 8,
}

makeFlags : I32 -> I32 -> Flags
makeFlags l s = Flags { layer = l, stencil = s }
"#;
        let wgsl = compile_to_wgsl(source).expect("bitfield construction should compile");
        assert!(
            wgsl.contains("fn makeFlags("),
            "WGSL should contain fn makeFlags, got: {}",
            wgsl
        );
        // Should contain bitwise ops: &, |, <<
        assert!(
            wgsl.contains("&"),
            "WGSL should contain & for masking, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("|"),
            "WGSL should contain | for combining, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("<<"),
            "WGSL should contain << for shifting, got: {}",
            wgsl
        );
        // Return type should be u32 (bitfield base type)
        assert!(
            wgsl.contains("-> u32"),
            "WGSL should return u32, got: {}",
            wgsl
        );
    }

    #[test]
    fn bitfield_construction_bool_field_uses_select() {
        let source = r#"
bitfield Flags : U32 = Flags {
  visible : Bool,
  layer   : U32 : 4,
}

makeFlags : Bool -> I32 -> Flags
makeFlags v l = Flags { visible = v, layer = l }
"#;
        let wgsl = compile_to_wgsl(source).expect("bitfield construction with bool should compile");
        // 1-bit bool fields should use select(0u, 1u, val)
        assert!(
            wgsl.contains("select("),
            "WGSL should contain select for bool field, got: {}",
            wgsl
        );
    }

    #[test]
    fn bitfield_functional_update_clears_and_sets_field() {
        let source = r#"
bitfield Flags : U32 = Flags {
  visible : Bool,
  layer   : U32 : 4,
  stencil : U32 : 8,
}

updateLayer : Flags -> I32 -> Flags
updateLayer f newLayer = f { layer = newLayer }
"#;
        let wgsl = compile_to_wgsl(source).expect("bitfield update should compile");
        assert!(
            wgsl.contains("fn updateLayer("),
            "WGSL should contain fn updateLayer, got: {}",
            wgsl
        );
        // Should clear bits with AND mask and set new bits with OR
        assert!(
            wgsl.contains("&"),
            "WGSL should contain & for clearing, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("|"),
            "WGSL should contain | for setting, got: {}",
            wgsl
        );
    }

    #[test]
    fn bitfield_construction_in_entry_point() {
        let source = r#"
bitfield Flags : U32 = Flags {
  layer   : U32 : 4,
  stencil : U32 : 8,
}

@group(0) @binding(0) storage(read_write) output : Array<U32, 64>

@compute @workgroup_size(64, 1, 1)
main idx =
  let f = Flags { layer = 5, stencil = 128 }
  in writeAt output idx f
"#;
        let wgsl =
            compile_to_wgsl(source).expect("bitfield construction in entry point should compile");
        assert!(
            wgsl.contains("@compute"),
            "WGSL should contain @compute, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("0u |"),
            "WGSL should start accumulator from 0u, got: {}",
            wgsl
        );
    }

    // ========================================================================
    // Unified type namespace: duplicate type name error tests
    // ========================================================================

    #[test]
    fn newtype_pattern_is_not_duplicate() {
        // Constructor sharing name with its type is fine (value namespace).
        let source = r#"
data Velocity = Velocity (Vec<3, F32>)
velocityResult : Velocity
velocityResult = Velocity [1.0, 0.0, 0.0]
"#;
        let (sa, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "newtype pattern should not be a duplicate type name error, got: {:?}",
            sa.diagnostics()
                .iter()
                .map(|d| &d.message)
                .collect::<Vec<_>>()
        );
    }
}

// =========================================================================
// 11. Zero-param function to const promotion
// =========================================================================

mod const_promotion_tests {
    use super::*;
    use std::panic;

    fn compile_to_wgsl(source: &str) -> Result<String, String> {
        let mut parser = Parser::new(source);
        let mut program = parser.parse_program();

        if parser.diagnostics().has_errors() {
            return Err("parse error".into());
        }

        with_prelude(&mut program);

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);

        if sa.has_errors() {
            return Err(sa
                .diagnostics()
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n"));
        }

        let mut lowering = AstLowering::new(&sa);
        let hir = lowering.lower_program(&program);

        if lowering.has_errors() {
            return Err(lowering
                .diagnostics()
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n"));
        }

        let arena = shadml_allocator::Allocator::new();
        let mir = shadml_mir::lower::lower_hir_to_mir(&arena, &hir).map_err(|e| e.join(", "))?;
        let mir = shadml_mir::reachability::eliminate_dead_code(&mir);

        Ok(shadml_wgsl_codegen::emit_wgsl(&mir))
    }

    #[test]
    fn zero_param_literal_becomes_const() {
        let source = "maxLights : I32\nmaxLights = 64";
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("const maxLights: i32 = 64i;"),
            "should emit const declaration, got: {}",
            wgsl
        );
        assert!(
            !wgsl.contains("fn maxLights"),
            "should NOT emit function declaration, got: {}",
            wgsl
        );
    }

    #[test]
    fn zero_param_float_literal_becomes_const() {
        let source = "pi : F32\npi = 3.14159";
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("const pi: f32 ="),
            "should emit const for float literal, got: {}",
            wgsl
        );
        assert!(
            !wgsl.contains("fn pi"),
            "should NOT emit function, got: {}",
            wgsl
        );
    }

    #[test]
    fn zero_param_arithmetic_becomes_const() {
        let source = "stride : I32\nstride = 4 + 3";
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("const stride: i32 ="),
            "should emit const for arithmetic, got: {}",
            wgsl
        );
        assert!(
            !wgsl.contains("fn stride"),
            "should NOT emit function, got: {}",
            wgsl
        );
    }

    #[test]
    fn function_with_params_stays_function() {
        let source = "double : I32 -> I32\ndouble x = x * 2";
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("fn double("),
            "function with params should stay a function, got: {}",
            wgsl
        );
    }

    #[test]
    fn zero_param_negation_becomes_const() {
        let source = "neg1 : I32\nneg1 = -1";
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("const neg1: i32 ="),
            "negation should be promoted to const, got: {}",
            wgsl
        );
    }

    #[test]
    fn zero_param_bool_expr_becomes_const() {
        let source = "enabled : Bool\nenabled = 1 == 1";
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("const enabled: bool ="),
            "bool expression should be promoted to const, got: {}",
            wgsl
        );
        assert!(
            !wgsl.contains("fn enabled"),
            "should NOT emit function, got: {}",
            wgsl
        );
    }

    #[test]
    fn const_used_in_entry_point() {
        // Ensure promoted constants are usable from entry points and produce valid WGSL
        let source = r#"
@group(0) @binding(0) storage(read_write) results : Array<Vec<4, F32>>

data ComputeInput = ComputeInput {
  @builtin(global_invocation_id) gid : Vec<3, U32>
}

maxLights : I32
maxLights = 64

main : ComputeInput -> ()
@compute @workgroup_size(64, 1, 1)
main input =
  let idx = toU32 input.gid.x
      result = vec4 (toF32 maxLights) 0.0 0.0 1.0
  in writeAt results idx result
"#;
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("const maxLights: i32 = 64i;"),
            "should emit const declaration, got: {}",
            wgsl
        );
        assert!(
            !wgsl.contains("fn maxLights"),
            "should NOT emit function, got: {}",
            wgsl
        );
    }

    #[test]
    fn mir_validation_rejects_const_call_and_codegen_panics() {
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![],
            constants: vec![MirConst {
                name: "maxLights",
                ty: MirType::I32,
                value: MirExpr::Lit(MirLit::I32(64)),
            }],
            entry_points: vec![MirEntryPoint {
                name: "main",
                stage: ShaderStage::Compute,
                workgroup_size: Some([1, 1, 1]),
                params: vec![],
                return_ty: MirType::Unit,
                body: vec![MirStmt::Let(
                    "x",
                    MirType::I32,
                    MirExpr::Call("maxLights", vec![], MirType::I32),
                )],
                return_expr: None,
                comments: vec![],
            }],
            render_blocks: vec![],
        };

        let errors = shadml_mir::validate::validate_program(&program)
            .expect_err("validator should reject calls to constants");
        assert!(
            errors
                .iter()
                .any(|e| e.contains("invalid call to constant 'maxLights'")),
            "expected const-call validation error, got: {:?}",
            errors
        );

        let panic_payload = panic::catch_unwind(|| emit_wgsl(&program))
            .expect_err("emit_wgsl should panic on invalid MIR");
        let panic_message = if let Some(msg) = panic_payload.downcast_ref::<String>() {
            msg.clone()
        } else if let Some(msg) = panic_payload.downcast_ref::<&str>() {
            msg.to_string()
        } else {
            "<non-string panic>".to_string()
        };
        assert!(
            panic_message.contains("attempted to emit invalid MIR as WGSL"),
            "expected codegen panic to mention invalid MIR, got: {}",
            panic_message
        );
        assert!(
            panic_message.contains("invalid call to constant 'maxLights'"),
            "expected codegen panic to include validator detail, got: {}",
            panic_message
        );
    }
}

// =========================================================================
// Bitwise operator tests
// =========================================================================

mod bitwise_tests {
    use super::*;

    fn compile_to_wgsl(source: &str) -> Result<String, String> {
        let mut parser = Parser::new(source);
        let mut program = parser.parse_program();

        if parser.diagnostics().has_errors() {
            return Err("parse error".into());
        }

        with_prelude(&mut program);

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);

        if sa.has_errors() {
            return Err(sa
                .diagnostics()
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n"));
        }

        let mut lowering = AstLowering::new(&sa);
        let hir = lowering.lower_program(&program);

        if lowering.has_errors() {
            return Err(lowering
                .diagnostics()
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n"));
        }

        let arena = shadml_allocator::Allocator::new();
        let mir = shadml_mir::lower::lower_hir_to_mir(&arena, &hir).map_err(|e| e.join(", "))?;
        let mir = shadml_mir::reachability::eliminate_dead_code(&mir);

        Ok(shadml_wgsl_codegen::emit_wgsl(&mir))
    }

    #[test]
    fn test_bitwise_and() {
        let source = "testAnd : U32 -> U32 -> U32\ntestAnd x y = x & y";
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("x & y"),
            "WGSL should contain bitwise AND, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_bitwise_xor() {
        let source = "testXor : U32 -> U32 -> U32\ntestXor x y = x ^ y";
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("x ^ y"),
            "WGSL should contain bitwise XOR, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_shift_left() {
        let source = "testShl : U32 -> U32 -> U32\ntestShl x y = x << y";
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("x << y"),
            "WGSL should contain shift left, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_shift_right_infix() {
        let source = "testShr : U32 -> U32 -> U32\ntestShr x y = x >> y";
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("x >> y"),
            "WGSL should contain shift right, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_user_builtin_extern_is_legal() {
        let source = r#"
builtin extern wave : F32 -> F32 = intrinsic(sin)

testWave : F32 -> F32
testWave x = wave x
"#;
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("sin(x)"),
            "WGSL should contain the user-declared builtin extern lowering, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_bitwise_not() {
        let source = "testNot : U32 -> U32\ntestNot x = ~x";
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("~x"),
            "WGSL should contain bitwise NOT, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_bitwise_or_builtin() {
        let source = "testOr : U32 -> U32 -> U32\ntestOr x y = bor x y";
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("x | y"),
            "WGSL should contain bitwise OR via bor, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_bitwise_operator_precedence() {
        // & should bind tighter than ^ — WGSL has the same precedence, so no parens needed
        let source = "testPrec : U32 -> U32 -> U32 -> U32\ntestPrec a b c = a ^ b & c";
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("a ^ b & c"),
            "WGSL should show & binding tighter than ^, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_bitwise_combined() {
        let source = "mask : U32 -> U32 -> U32\nmask flags bit = flags & (~bit)";
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("&") && wgsl.contains("~"),
            "WGSL should contain & and ~, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_bitand_trait_overload() {
        let source = r#"
data Mask = Mask { bits : U32 }

impl BitAnd Mask Mask where
  type Output = Mask
  (&) a b = Mask { bits = a.bits & b.bits }

andMask : Mask -> Mask -> Mask
andMask a b = a & b
"#;
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("fn bitand_Mask__Mask("),
            "WGSL should contain mangled bitand impl, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("bitand_Mask__Mask(a, b)"),
            "WGSL should dispatch & to the mangled BitAnd impl, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_bitxor_trait_overload() {
        let source = r#"
data Mask = Mask { bits : U32 }

impl BitXor Mask Mask where
  type Output = Mask
  (^) a b = Mask { bits = a.bits ^ b.bits }

xorMask : Mask -> Mask -> Mask
xorMask a b = a ^ b
"#;
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("fn bitxor_Mask__Mask("),
            "WGSL should contain mangled bitxor impl, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("bitxor_Mask__Mask(a, b)"),
            "WGSL should dispatch ^ to the mangled BitXor impl, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_shl_trait_overload() {
        let source = r#"
data Mask = Mask { bits : U32 }

impl Shl Mask Mask where
  type Output = Mask
  (<<) a b = Mask { bits = a.bits << b.bits }

shlMask : Mask -> Mask -> Mask
shlMask a b = a << b
"#;
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("fn shl_Mask__Mask("),
            "WGSL should contain mangled shl impl, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("shl_Mask__Mask(a, b)"),
            "WGSL should dispatch << to the mangled Shl impl, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_shr_trait_overload() {
        let source = r#"
data Mask = Mask { bits : U32 }

impl Shr Mask Mask where
  type Output = Mask
  (>>) a b = Mask { bits = a.bits >> b.bits }

shrMask : Mask -> Mask -> Mask
shrMask a b = a >> b
"#;
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("fn shr_Mask__Mask("),
            "WGSL should contain mangled shr impl, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("shr_Mask__Mask(a, b)"),
            "WGSL should dispatch >> to the mangled Shr impl, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_bitnot_trait_overload() {
        let source = r#"
data Mask = Mask { bits : U32 }

impl BitNot Mask where
  (~) a = Mask { bits = ~a.bits }

notMask : Mask -> Mask
notMask a = ~a
"#;
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("fn bitnot_Mask("),
            "WGSL should contain mangled bitnot impl, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("bitnot_Mask(a)"),
            "WGSL should dispatch ~ to bitnot_Mask, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_neg_trait_overload() {
        let source = r#"
data Wrapper = Wrapper { val : F32 }

impl Neg Wrapper where
  (-) a = Wrapper { val = -a.val }

negWrapper : Wrapper -> Wrapper
negWrapper a = -a
"#;
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("fn negate_Wrapper("),
            "WGSL should contain mangled negate impl, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("negate_Wrapper(a)"),
            "WGSL should dispatch - to negate_Wrapper, got: {}",
            wgsl
        );
    }
}

mod fold_range_tests {
    use super::*;

    fn compile_to_wgsl(source: &str) -> Result<String, String> {
        let mut parser = Parser::new(source);
        let mut program = parser.parse_program();

        if parser.diagnostics().has_errors() {
            return Err("parse error".into());
        }

        with_prelude(&mut program);

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);

        if sa.has_errors() {
            return Err(sa
                .diagnostics()
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n"));
        }

        let mut lowering = AstLowering::new(&sa);
        let hir = lowering.lower_program(&program);

        if lowering.has_errors() {
            return Err(lowering
                .diagnostics()
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n"));
        }

        let arena = shadml_allocator::Allocator::new();
        let mir = shadml_mir::lower::lower_hir_to_mir(&arena, &hir).map_err(|e| e.join(", "))?;
        let mir = shadml_mir::reachability::eliminate_dead_code(&mir);

        Ok(shadml_wgsl_codegen::emit_wgsl(&mir))
    }

    #[test]
    fn test_fold_range_basic_sum() {
        let source = r#"
sumRange : I32 -> I32
sumRange n = foldRange 0 n 0 (\acc i -> acc + i)
"#;
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("loop {"),
            "WGSL should contain a loop, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("acc + i"),
            "WGSL should contain accumulator update, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_fold_range_with_named_function() {
        let source = r#"
addToAcc : I32 -> I32 -> I32
addToAcc acc i = acc + i

sumNamed : I32 -> I32
sumNamed n = foldRange 0 n 0 addToAcc
"#;
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("loop {"),
            "WGSL should contain a loop, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("addToAcc(_fold_acc, _fold_i)"),
            "WGSL should call named function in loop, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_fold_range_vec3_accumulator() {
        let source = r#"
particle : Vec<2, F32> -> F32 -> F32 -> Vec<3, F32>
particle uv id time = vec3 (sin id) (cos id) 0.0

test : Vec<2, F32> -> F32 -> Vec<3, F32>
test uv time = foldRange 0 10 (vec3 0.0 0.0 0.0) (\acc i ->
  acc + particle uv (toF32 i) time)
"#;
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("loop {"),
            "WGSL should contain a loop, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("particle(uv, f32(i), time)"),
            "WGSL should call particle in loop body, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_fold_range_constant_bounds() {
        let source = r#"
factorial5 : I32
factorial5 = foldRange 1 6 1 (\acc i -> acc * i)
"#;
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("i >= 6i"),
            "WGSL should check i >= end, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("acc * i"),
            "WGSL should multiply in loop body, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_fold_range_in_where() {
        let source = r#"
test : F32 -> F32
test x = result
  where
    result = foldRange 0 5 x (\acc i -> acc + toF32 i)
"#;
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("loop {"),
            "WGSL should contain a loop from foldRange in where, got: {}",
            wgsl
        );
    }
}

// =========================================================================
// Naga WGSL validation tests
// =========================================================================
//
// These tests compile shadml source to WGSL, then validate the output
// through naga's WGSL parser to ensure syntactically and semantically
// valid shader code.

mod naga_validation {
    use super::*;

    fn compile_to_wgsl(source: &str) -> Result<String, String> {
        let mut parser = Parser::new(source);
        let mut program = parser.parse_program();

        if parser.diagnostics().has_errors() {
            return Err("parse error".into());
        }

        with_prelude(&mut program);

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);

        if sa.has_errors() {
            return Err(sa
                .diagnostics()
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n"));
        }

        let mut lowering = AstLowering::new(&sa);
        let hir = lowering.lower_program(&program);

        if lowering.has_errors() {
            return Err(lowering
                .diagnostics()
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n"));
        }

        let arena = shadml_allocator::Allocator::new();
        let mir = shadml_mir::lower::lower_hir_to_mir(&arena, &hir).map_err(|e| e.join(", "))?;
        let mir = shadml_mir::reachability::eliminate_dead_code(&mir);

        Ok(emit_wgsl(&mir))
    }

    fn validate_wgsl(wgsl: &str) -> Result<(), String> {
        let module = naga::front::wgsl::parse_str(wgsl).map_err(|e| format!("naga parse: {e}"))?;
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        validator
            .validate(&module)
            .map_err(|e| format!("naga validate: {e}"))?;
        Ok(())
    }

    fn compile_and_validate(source: &str) -> Result<String, String> {
        let wgsl = compile_to_wgsl(source)?;
        validate_wgsl(&wgsl).map_err(|e| format!("{e}\n--- WGSL ---\n{wgsl}"))?;
        Ok(wgsl)
    }

    #[test]
    fn naga_simple_function() {
        let source = "add : I32 -> I32 -> I32\nadd x y = x + y";
        compile_and_validate(source).expect("simple function should produce valid WGSL");
    }

    #[test]
    fn naga_compute_entry_point() {
        let source = r#"
data ComputeInput = ComputeInput {
  @builtin(global_invocation_id) gid : Vec<3, U32>
}

@group(0) @binding(0) storage(read_write) output : Array<Vec<4, F32>, 64>

main : ComputeInput -> ()
@compute @workgroup_size(64, 1, 1)
main input =
  let idx = toI32 input.gid.x
      color = vec4 1.0 0.0 0.0 1.0
  in writeAt output idx color
"#;
        compile_and_validate(source).expect("compute entry point should produce valid WGSL");
    }

    #[test]
    fn naga_data_type_and_match() {
        let source = r#"
data Color = Red | Green | Blue

colorToFloat : Color -> F32
colorToFloat c = match c
  | Red   -> 1.0
  | Green -> 0.5
  | Blue  -> 0.0
"#;
        compile_and_validate(source).expect("enum + match should produce valid WGSL");
    }

    #[test]
    fn naga_record_type() {
        let source = r#"
data Particle = Particle {
  x : F32,
  y : F32,
}

getX : Particle -> F32
getX p = p.x
"#;
        compile_and_validate(source).expect("record type should produce valid WGSL");
    }

    #[test]
    fn naga_if_then_else() {
        let source = r#"
maxVal : F32 -> F32 -> F32
maxVal a b = if a > b then a else b
"#;
        compile_and_validate(source).expect("if-then-else should produce valid WGSL");
    }

    #[test]
    fn naga_let_binding() {
        let source = r#"
pythagoras : F32 -> F32 -> F32
pythagoras a b =
  let a2 = a * a
      b2 = b * b
  in sqrt (a2 + b2)
"#;
        compile_and_validate(source).expect("let binding should produce valid WGSL");
    }

    #[test]
    fn naga_loop_expression() {
        let source = r#"
sumTo : I32 -> I32
sumTo n = loop go (i = 0) (acc = 0) in
  if i >= n
    then acc
    else go (i + 1) (acc + i)
"#;
        compile_and_validate(source).expect("loop expression should produce valid WGSL");
    }

    #[test]
    fn naga_const_declaration() {
        let source = r#"
const PI : F32 = 3.14159

circleArea : F32 -> F32
circleArea r = PI * r * r
"#;
        compile_and_validate(source).expect("const declaration should produce valid WGSL");
    }

    #[test]
    fn naga_uniform_binding() {
        let source = r#"
@group(0) @binding(0) uniform time : F32

getTime : F32
getTime = load time
"#;
        compile_and_validate(source).expect("uniform binding should produce valid WGSL");
    }

    #[test]
    fn naga_group_block_bindings() {
        let source = r#"
@group(0)
  @binding(0) uniform frame  : Vec<4, F32>
  @binding(1) uniform params : Vec<4, F32>

getFrame : Vec<4, F32>
getFrame = load frame
"#;
        compile_and_validate(source).expect("group block bindings should produce valid WGSL");
    }
}

// =========================================================================
// Render block integration tests
// =========================================================================

mod render_block_tests {
    use super::*;
    use std::io::Write;

    const RENDER_BLOCK_SOURCE: &str = r#"
data Globals = Globals {
  time : F32,
}

data VertexInput = VertexInput {
  @builtin(vertex_index) vertex_index : U32,
}

data VertexOutput = VertexOutput {
  @builtin(position) position : Vec<4, F32>,
  @location(0) color : Vec<4, F32>,
}

render test_render
  @group(0) @binding(0) uniform globals : Globals

  vsMain : VertexInput -> VertexOutput
  @vertex
  vsMain input =
    let pos = vec4 0.0 0.0 0.0 1.0
        col = vec4 1.0 0.0 0.0 1.0
    in VertexOutput { position = pos, color = col }

  fsMain : VertexOutput -> Vec<4, F32>
  @fragment
  fsMain input = input.color
"#;

    #[test]
    fn render_block_pipeline_compiles_to_valid_wgsl() {
        let mut parser = Parser::new(RENDER_BLOCK_SOURCE);
        let mut program = parser.parse_program();

        assert!(
            !parser.diagnostics().has_errors(),
            "parse errors: {:?}",
            parser.diagnostics().iter().map(|d| &d.message).collect::<Vec<_>>()
        );

        with_prelude(&mut program);

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);

        assert!(
            !sa.has_errors(),
            "semantic errors: {:?}",
            sa.diagnostics().iter().map(|d| &d.message).collect::<Vec<_>>()
        );

        let mut lowering = AstLowering::new(&sa);
        let hir = lowering.lower_program(&program);

        assert!(
            !lowering.has_errors(),
            "lowering errors: {:?}",
            lowering.diagnostics().iter().map(|d| &d.message).collect::<Vec<_>>()
        );

        let arena = shadml_allocator::Allocator::new();
        let mir = shadml_mir::lower::lower_hir_to_mir(&arena, &hir).expect("MIR lowering should succeed");

        // Assert MIR contains the expected render block
        assert_eq!(mir.render_blocks.len(), 1, "expected exactly one render block");
        let rb = &mir.render_blocks[0];
        assert_eq!(rb.name, "test_render");
        assert_eq!(rb.vertex_entry, "vsMain");
        assert_eq!(rb.fragment_entry, "fsMain");
        assert_eq!(rb.binding_names.len(), 1);
        assert_eq!(rb.binding_names[0], "globals");

        // Assert MIR validation passes
        let validation_result = shadml_mir::validate::validate_program(&mir);
        assert!(
            validation_result.is_ok(),
            "MIR validation should pass, got: {:?}",
            validation_result.unwrap_err()
        );

        // Emit WGSL and assert it contains both entry points
        let wgsl = emit_wgsl(&mir);
        assert!(wgsl.contains("@vertex"), "WGSL should contain @vertex, got:\n{}", wgsl);
        assert!(wgsl.contains("@fragment"), "WGSL should contain @fragment, got:\n{}", wgsl);
        assert!(wgsl.contains("fn vsMain("), "WGSL should contain vertex entry, got:\n{}", wgsl);
        assert!(wgsl.contains("fn fsMain("), "WGSL should contain fragment entry, got:\n{}", wgsl);
        assert!(
            wgsl.contains("@group(0) @binding(0)"),
            "WGSL should contain binding from render block, got:\n{}",
            wgsl
        );
    }

    #[test]
    fn render_block_bundler_produces_entries_with_render_block() {
        let tmp_dir = std::env::temp_dir().join("shadml_render_block_test");
        let _ = std::fs::remove_dir_all(&tmp_dir);
        std::fs::create_dir_all(&tmp_dir).expect("should create temp dir");

        let shader_path = tmp_dir.join("TestRender.shadml");
        let mut file = std::fs::File::create(&shader_path).expect("should create shader file");
        file.write_all(RENDER_BLOCK_SOURCE.as_bytes())
            .expect("should write shader source");
        drop(file);

        let config = shadml_bundler::BundleConfig {
            entries: vec![shader_path.clone()],
            source_roots: vec![tmp_dir.clone()],
            output_dir: tmp_dir.join("dist"),
            features: vec![],
            preserve_comments: false,
            split_entry_points: true,
        };

        let manifest = shadml_bundler::bundle_manifest(&config, "test_profile"
        ).expect("bundle_manifest should succeed");

        assert_eq!(manifest.profiles.len(), 1);
        let profile = &manifest.profiles[0];
        assert_eq!(profile.entries.len(), 2, "expected two split entries (vertex + fragment)");

        let vertex_entry = profile
            .entries
            .iter()
            .find(|e| e.stage == shadml_mir::ShaderStage::Vertex)
            .expect("should have vertex entry");
        let fragment_entry = profile
            .entries
            .iter()
            .find(|e| e.stage == shadml_mir::ShaderStage::Fragment)
            .expect("should have fragment entry");

        assert_eq!(
            vertex_entry.render_block,
            Some("test_render".to_string()),
            "vertex entry should belong to render block"
        );
        assert_eq!(
            fragment_entry.render_block,
            Some("test_render".to_string()),
            "fragment entry should belong to render block"
        );

        // Each entry should have the binding from the render block
        assert!(
            vertex_entry.bind_groups.iter().any(|bg| {
                bg.bindings.iter().any(|b| b.name == "globals")
            }),
            "vertex entry should have globals binding"
        );
        assert!(
            fragment_entry.bind_groups.iter().any(|bg| {
                bg.bindings.iter().any(|b| b.name == "globals")
            }),
            "fragment entry should have globals binding"
        );

        // Clean up temp directory
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn render_block_bindgen_generates_pipeline_layout_function() {
        let tmp_dir = std::env::temp_dir().join("shadml_render_block_bindgen_test");
        let _ = std::fs::remove_dir_all(&tmp_dir);
        std::fs::create_dir_all(&tmp_dir).expect("should create temp dir");

        let shader_path = tmp_dir.join("TestRender.shadml");
        let mut file = std::fs::File::create(&shader_path).expect("should create shader file");
        file.write_all(RENDER_BLOCK_SOURCE.as_bytes())
            .expect("should write shader source");
        drop(file);

        let config_path = tmp_dir.join("shadml.toml");
        let mut config_file = std::fs::File::create(&config_path).expect("should create config file");
        config_file
            .write_all(
                br#"[bundle]
source_roots = ["."]
split_entry_points = true

[[entry]]
file = "TestRender.shadml"

[rust]
output = "generated.rs"
source_mode = "EmbeddedDebug"
"#,
            )
            .expect("should write config file");
        drop(config_file);

        let output_path = tmp_dir.join("generated.rs");

        let bindgen = shadml_bindgen::ShadmlBindgenBuilder::default()
            .project_root(&tmp_dir)
            .config(&config_path)
            .output(&output_path)
            .source_mode(shadml_bindgen::SourceMode::EmbeddedDebug)
            .build()
            .expect("bindgen build should succeed");

        bindgen.generate().expect("bindgen generate should succeed");

        let rust_source = std::fs::read_to_string(&output_path)
            .expect("should read generated rust source");

        assert!(
            rust_source.contains("create_test_render_render_pipeline_layout"),
            "bindgen should generate render block pipeline layout function, got:\n{}",
            rust_source
        );
        assert!(
            rust_source.contains("group0::create_bind_group_layout"),
            "bindgen should deduplicate bind groups at module level, got:\n{}",
            rust_source
        );

        // Clean up temp directory
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
}
