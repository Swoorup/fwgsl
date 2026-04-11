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
            include_str!("../../../../fixtures/hello.shadml"),
        ),
        (
            "adt.shadml",
            include_str!("../../../../fixtures/adt.shadml"),
        ),
        (
            "particle.shadml",
            include_str!("../../../../fixtures/particle.shadml"),
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
            include_str!("../../../../fixtures/hello.shadml"),
        ),
        (
            "adt.shadml",
            include_str!("../../../../fixtures/adt.shadml"),
        ),
        (
            "particle.shadml",
            include_str!("../../../../fixtures/particle.shadml"),
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
