use super::*;

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Infix operator binding powers: returns `(left_bp, right_bp)`.
///
/// Left-associative:  `l_bp < r_bp`
/// Right-associative: `l_bp > r_bp`
pub fn infix_binding_power(kind: SyntaxKind) -> Option<(u8, u8)> {
    match kind {
        SyntaxKind::Dollar => Some((1, 0)), // right-assoc (l > r)
        SyntaxKind::OrOr => Some((1, 2)),   // ||
        SyntaxKind::AndAnd => Some((3, 4)), // &&
        // Note: `|` (Pipe) is NOT an infix operator here — it's used for
        // guard clauses, match arms, data constructors, and or-patterns.
        // Bitwise OR is available via the `bor` builtin function.
        SyntaxKind::Caret => Some((5, 6)),     // ^ (bitwise xor)
        SyntaxKind::Ampersand => Some((7, 8)), // & (bitwise and)
        SyntaxKind::EqualEqual
        | SyntaxKind::NotEqual
        | SyntaxKind::Less
        | SyntaxKind::Greater
        | SyntaxKind::LessEqual
        | SyntaxKind::GreaterEqual => Some((9, 10)), // comparisons
        SyntaxKind::LessLess => Some((11, 12)), // <<
        // >> is handled specially in the parser loop (two Greater tokens)
        SyntaxKind::Plus | SyntaxKind::Minus => Some((13, 14)), // + -
        SyntaxKind::Star | SyntaxKind::Slash | SyntaxKind::Percent => Some((15, 16)), // * / %
        _ => None,
    }
}

/// Whether a token kind can begin an atom expression.
pub fn is_atom_start(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::IntLiteral
            | SyntaxKind::FloatLiteral
            | SyntaxKind::StringLiteral
            | SyntaxKind::CharLiteral
            | SyntaxKind::Ident
            | SyntaxKind::UpperIdent
            | SyntaxKind::LParen
            | SyntaxKind::LBracket
            | SyntaxKind::Backslash
    )
}

pub fn span_touches(lhs: Span, rhs: Span) -> bool {
    lhs.end == rhs.start
}

pub fn flatten_app(expr: Expr) -> (Expr, Vec<Expr>) {
    let mut args = Vec::new();
    let mut head = expr;
    while let Expr::App(f, a, _) = head {
        args.push(*a);
        head = *f;
    }
    args.reverse();
    (head, args)
}

pub fn fold_app(mut head: Expr, args: Vec<Expr>) -> Expr {
    for arg in args {
        let span = head.span().merge(arg.span());
        head = Expr::App(Box::new(head), Box::new(arg), span);
    }
    head
}

pub fn insert_pipeline_arg(rhs: Expr, lhs: Expr, span: Span) -> Expr {
    match rhs {
        Expr::App(_, _, _) => {
            let (head, mut args) = flatten_app(rhs);
            let mut with_lhs = Vec::with_capacity(args.len() + 1);
            with_lhs.push(lhs);
            with_lhs.append(&mut args);
            let mut applied = fold_app(head, with_lhs);
            // Ensure outer span preserves full pipe expression.
            match &mut applied {
                Expr::App(_, _, s)
                | Expr::Infix(_, _, _, s)
                | Expr::Paren(_, s)
                | Expr::FieldAccess(_, _, s)
                | Expr::Index(_, _, s)
                | Expr::Neg(_, s)
                | Expr::Not(_, s)
                | Expr::BitNot(_, s)
                | Expr::Do(_, s)
                | Expr::VecLit(_, s)
                | Expr::Lit(_, s)
                | Expr::Var(_, s)
                | Expr::Con(_, s)
                | Expr::Lambda(_, _, s)
                | Expr::Let(_, _, s)
                | Expr::Case(_, _, s)
                | Expr::If(_, _, _, s)
                | Expr::Tuple(_, s)
                | Expr::Record(_, _, s)
                | Expr::OpSection(_, s)
                | Expr::Loop(_, _, _, s)
                | Expr::RecordUpdate(_, _, s) => *s = span,
            }
            applied
        }
        other => Expr::App(Box::new(other), Box::new(lhs), span),
    }
}

/// Whether a token kind can begin a pattern atom.
pub fn is_pat_atom_start(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::Underscore
            | SyntaxKind::Ident
            | SyntaxKind::UpperIdent
            | SyntaxKind::IntLiteral
            | SyntaxKind::FloatLiteral
            | SyntaxKind::StringLiteral
            | SyntaxKind::CharLiteral
            | SyntaxKind::LParen
    )
}

pub fn is_operator_token(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::Plus
            | SyntaxKind::Minus
            | SyntaxKind::Star
            | SyntaxKind::Slash
            | SyntaxKind::Percent
            | SyntaxKind::Less
            | SyntaxKind::Greater
            | SyntaxKind::LessEqual
            | SyntaxKind::GreaterEqual
            | SyntaxKind::EqualEqual
            | SyntaxKind::NotEqual
            | SyntaxKind::AndAnd
            | SyntaxKind::OrOr
            | SyntaxKind::Ampersand
            | SyntaxKind::Caret
            | SyntaxKind::Tilde
            | SyntaxKind::LessLess
            | SyntaxKind::GreaterGreater
            | SyntaxKind::Dollar
            | SyntaxKind::Bang
            | SyntaxKind::Dot
    )
}

/// Result of parsing an integer literal — either signed (i64) or unsigned (u64).
pub enum ParsedInt {
    Signed(i64),
    Unsigned(u64),
}

pub fn parse_int_literal_typed(text: &str) -> ParsedInt {
    // Strip optional suffix
    let (digits, is_unsigned) = if let Some(stripped) = text.strip_suffix('u') {
        (stripped, true)
    } else if let Some(stripped) = text.strip_suffix('i') {
        (stripped, false)
    } else {
        (text, false)
    };

    let value = if digits.starts_with("0x") || digits.starts_with("0X") {
        u64::from_str_radix(&digits[2..], 16).unwrap_or(0)
    } else if digits.starts_with("0o") || digits.starts_with("0O") {
        u64::from_str_radix(&digits[2..], 8).unwrap_or(0)
    } else if digits.starts_with("0b") || digits.starts_with("0B") {
        u64::from_str_radix(&digits[2..], 2).unwrap_or(0)
    } else {
        digits.parse::<u64>().unwrap_or(0)
    };

    if is_unsigned {
        ParsedInt::Unsigned(value)
    } else {
        ParsedInt::Signed(value as i64)
    }
}

pub fn parse_int_literal(text: &str) -> i64 {
    match parse_int_literal_typed(text) {
        ParsedInt::Signed(v) => v,
        ParsedInt::Unsigned(v) => v as i64,
    }
}

pub fn unescape_string(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('0') => out.push('\0'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn unescape_char(s: &str) -> char {
    let mut chars = s.chars();
    match chars.next() {
        Some('\\') => match chars.next() {
            Some('n') => '\n',
            Some('t') => '\t',
            Some('r') => '\r',
            Some('\\') => '\\',
            Some('\'') => '\'',
            Some('0') => '\0',
            Some(c) => c,
            None => '\\',
        },
        Some(c) => c,
        None => '\0',
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════
