use super::*;

impl Parser {
pub fn parse_expr(&mut self) -> Expr {
    self.parse_expr_bp(0)
}

pub(crate) fn parse_expr_bp(&mut self, min_bp: u8) -> Expr {
    self.skip_trivia();

    // -- Prefix / atoms ------------------------------------------------
    let mut lhs = match self.peek_non_trivia() {
        SyntaxKind::Minus => {
            // Negation prefix
            self.skip_trivia();
            let start = self.current_span().start;
            self.bump();
            self.skip_trivia();
            let rhs = self.parse_expr_bp(21); // high precedence for neg
            let span = self.span_from(start);
            Expr::Neg(Box::new(rhs), span)
        }
        SyntaxKind::Bang => {
            // Boolean not prefix
            self.skip_trivia();
            let start = self.current_span().start;
            self.bump();
            self.skip_trivia();
            let rhs = self.parse_expr_bp(21); // high precedence for not
            let span = self.span_from(start);
            Expr::Not(Box::new(rhs), span)
        }
        SyntaxKind::Tilde => {
            // Bitwise not prefix
            self.skip_trivia();
            let start = self.current_span().start;
            self.bump();
            self.skip_trivia();
            let rhs = self.parse_expr_bp(21); // high precedence for bitwise not
            let span = self.span_from(start);
            Expr::BitNot(Box::new(rhs), span)
        }
        SyntaxKind::Backslash => self.parse_lambda(),
        SyntaxKind::KwIf => self.parse_if(),
        SyntaxKind::KwMatch => self.parse_match(),
        SyntaxKind::KwLet => self.parse_let(),
        SyntaxKind::KwDo => self.parse_do(),
        SyntaxKind::KwLoop => self.parse_loop(),
        _ => self.parse_atom(),
    };

    // -- Infix / postfix loop ------------------------------------------
    loop {
        if !self.consume_fuel() {
            break;
        }
        self.skip_trivia();
        let op_kind = self.peek_non_trivia();

        // Dot for field access
        if op_kind == SyntaxKind::Dot && min_bp <= 21 {
            self.skip_trivia();
            self.bump(); // consume `.`
            self.skip_trivia();
            if self.at(SyntaxKind::Ident) {
                let field_tok = self.bump();
                let field = self.text_of(&field_tok).to_owned();
                let span = lhs.span().merge(field_tok.span);
                lhs = Expr::FieldAccess(Box::new(lhs), field, span);
                continue;
            } else {
                // Error: expected field name after `.`
                let tok = self.current_token().clone();
                self.diagnostics.push(
                    Diagnostic::error("expected field name after '.'")
                        .with_label(Label::primary(tok.span, "expected identifier"))
                        .with_help("write a field access like `value.field`"),
                );
                break;
            }
        }

        // Index access: expr[expr]
        if op_kind == SyntaxKind::LBracket
            && min_bp <= 21
            && self.postfix_is_adjacent(lhs.span())
        {
            self.skip_trivia();
            self.bump();
            self.skip_trivia();
            let index_expr = self.parse_expr();
            self.skip_trivia();
            self.expect(SyntaxKind::RBracket);
            let span = lhs.span().merge(index_expr.span());
            lhs = Expr::Index(Box::new(lhs), Box::new(index_expr), span);
            continue;
        }

        // Function application (juxtaposition): if the next token could
        // start an atom and we have enough binding power.
        // Also treat `-<digit>` (whitespace before, no space after) as a
        // negative literal atom so `vec2 -0.35 0.5` works without parens.
        if (is_atom_start(op_kind) || self.is_negative_literal_ahead()) && min_bp <= 19 {
            self.skip_trivia();
            let arg = self.parse_atom();
            let span = lhs.span().merge(arg.span());
            lhs = Expr::App(Box::new(lhs), Box::new(arg), span);
            continue;
        }

        // Shift-right `>>`: two adjacent Greater tokens with no whitespace.
        // Must be checked BEFORE normal binary operators, since `>` is also
        // a comparison operator. We don't lex `>>` as a single token because
        // it conflicts with nested generic type closers like `Vec<4, Vec<3, F32>>`.
        if op_kind == SyntaxKind::Greater {
            let cur_pos = {
                let mut p = self.pos;
                while p < self.tokens.len() && self.tokens[p].kind.is_trivia() {
                    p += 1;
                }
                p
            };
            let next_idx = cur_pos + 1;
            let is_shift_right = next_idx < self.tokens.len()
                && self.tokens[next_idx].kind == SyntaxKind::Greater
                && self.tokens[cur_pos].span.end == self.tokens[next_idx].span.start;
            if is_shift_right {
                let (l_bp, r_bp) = (11, 12); // same as LessLess
                if l_bp >= min_bp {
                    self.skip_trivia();
                    self.bump(); // first >
                    self.bump(); // second >
                    self.skip_trivia();
                    let rhs = self.parse_expr_bp(r_bp);
                    let span = lhs.span().merge(rhs.span());
                    lhs = Expr::Infix(Box::new(lhs), ">>".to_string(), Box::new(rhs), span);
                    continue;
                }
            }
        }

        // Binary operators
        if let Some((l_bp, r_bp)) = infix_binding_power(op_kind) {
            if l_bp < min_bp {
                break;
            }
            self.skip_trivia();
            let op_tok = self.bump();
            let op_text = self.text_of(&op_tok).to_owned();
            self.skip_trivia();
            let rhs = self.parse_expr_bp(r_bp);
            let span = lhs.span().merge(rhs.span());
            lhs = Expr::Infix(Box::new(lhs), op_text, Box::new(rhs), span);
            continue;
        }

        // Backtick infix: expr `func` expr
        if op_kind == SyntaxKind::Backtick && min_bp <= 3 {
            self.skip_trivia();
            self.bump(); // consume opening backtick
            self.skip_trivia();
            let func_tok = self.bump();
            let func = self.text_of(&func_tok).to_owned();
            self.skip_trivia();
            self.expect(SyntaxKind::Backtick); // closing backtick
            self.skip_trivia();
            let rhs = self.parse_expr_bp(4);
            let span = lhs.span().merge(rhs.span());
            lhs = Expr::Infix(Box::new(lhs), func, Box::new(rhs), span);
            continue;
        }

        // Pipeline operator: x |> f  desugars to  f x
        if op_kind == SyntaxKind::PipeForward && min_bp <= 1 {
            self.skip_trivia();
            self.bump(); // consume `|>`
            self.skip_trivia();
            let func = self.parse_expr_bp(2);
            let span = lhs.span().merge(func.span());
            lhs = insert_pipeline_arg(func, lhs, span);
            continue;
        }

        break;
    }

    lhs
}

/// Parse an atom followed by any postfix `.field` or `[index]` operators.
/// This ensures that `tint.a` is parsed as a single unit in application
/// contexts like `f x tint.a`.
pub(crate) fn parse_atom(&mut self) -> Expr {
    let mut expr = self.parse_atom_core();

    // Consume postfix field access and indexing
    loop {
        self.skip_trivia();
        match self.peek_non_trivia() {
            SyntaxKind::Dot => {
                self.skip_trivia();
                self.bump(); // consume `.`
                self.skip_trivia();
                if self.at(SyntaxKind::Ident) {
                    let field_tok = self.bump();
                    let field = self.text_of(&field_tok).to_owned();
                    let span = expr.span().merge(field_tok.span);
                    expr = Expr::FieldAccess(Box::new(expr), field, span);
                } else {
                    break;
                }
            }
            SyntaxKind::LBracket if self.postfix_is_adjacent(expr.span()) => {
                self.skip_trivia();
                self.bump(); // consume `[`
                self.skip_trivia();
                let index = self.parse_expr();
                self.skip_trivia();
                self.expect(SyntaxKind::RBracket);
                let span = expr.span().merge(self.span_from(expr.span().start));
                expr = Expr::Index(Box::new(expr), Box::new(index), span);
            }
            SyntaxKind::LBrace if self.is_record_update_ahead() => {
                self.skip_trivia();
                let start = expr.span().start;
                self.bump(); // consume `{`
                let mut fields = Vec::new();
                loop {
                    self.skip_trivia();
                    if self.at(SyntaxKind::RBrace) || self.at_end() {
                        break;
                    }
                    let field_tok = self.expect(SyntaxKind::Ident);
                    let field_name = self.text_of(&field_tok).to_owned();
                    self.skip_trivia();
                    self.expect(SyntaxKind::Equals);
                    self.skip_trivia();
                    let value = self.parse_expr();
                    fields.push((field_name, value));
                    self.skip_trivia();
                    if !self.eat(SyntaxKind::Comma) {
                        break;
                    }
                }
                self.expect(SyntaxKind::RBrace);
                let span = self.span_from(start);
                expr = Expr::RecordUpdate(Box::new(expr), fields, span);
            }
            _ => break,
        }
    }

    expr
}

pub(crate) fn parse_atom_core(&mut self) -> Expr {
    self.skip_trivia();

    match self.peek() {
        SyntaxKind::IntLiteral => {
            let tok = self.bump();
            let text = self.text_of(&tok);
            match parse_int_literal_typed(text) {
                ParsedInt::Unsigned(v) => Expr::Lit(Lit::UInt(v), tok.span),
                ParsedInt::Signed(v) => Expr::Lit(Lit::Int(v), tok.span),
            }
        }
        SyntaxKind::FloatLiteral => {
            let tok = self.bump();
            let text = self.text_of(&tok);
            let val: f64 = text.parse().unwrap_or(0.0);
            Expr::Lit(Lit::Float(val), tok.span)
        }
        SyntaxKind::StringLiteral => {
            let tok = self.bump();
            let text = self.text_of(&tok);
            // Strip quotes
            let inner = &text[1..text.len() - 1];
            Expr::Lit(Lit::String(unescape_string(inner)), tok.span)
        }
        SyntaxKind::CharLiteral => {
            let tok = self.bump();
            let text = self.text_of(&tok);
            let inner = &text[1..text.len() - 1];
            let ch = unescape_char(inner);
            Expr::Lit(Lit::Char(ch), tok.span)
        }
        SyntaxKind::Ident => {
            let tok = self.bump();
            let name = self.text_of(&tok).to_owned();
            Expr::Var(name, tok.span)
        }
        SyntaxKind::UpperIdent => {
            let tok = self.bump();
            let name = self.text_of(&tok).to_owned();
            // Named record/bitfield construction: Name { field = expr, ... }
            self.skip_trivia();
            if self.at(SyntaxKind::LBrace) {
                return self.parse_named_record(name, tok.span.start);
            }
            Expr::Con(name, tok.span)
        }
        // Negative literal atom: `-0.35`, `-42` (no space between `-` and digit)
        SyntaxKind::Minus if self.is_negative_literal_ahead() => {
            let start = self.current_span().start;
            self.bump(); // consume `-`
            let tok = self.bump(); // consume adjacent numeric literal
            let text = self.text_of(&tok);
            let span = self.span_from(start);
            match tok.kind {
                SyntaxKind::FloatLiteral => {
                    let val: f64 = text.parse().unwrap_or(0.0);
                    Expr::Lit(Lit::Float(-val), span)
                }
                SyntaxKind::IntLiteral => {
                    match parse_int_literal_typed(text) {
                        ParsedInt::Unsigned(v) => {
                            // -42u → treat as Neg(UInt(42)) since unsigned can't be negative
                            Expr::Neg(Box::new(Expr::Lit(Lit::UInt(v), tok.span)), span)
                        }
                        ParsedInt::Signed(v) => Expr::Lit(Lit::Int(-v), span),
                    }
                }
                _ => unreachable!(),
            }
        }
        SyntaxKind::LParen => self.parse_paren_expr(),
        SyntaxKind::LBracket => self.parse_vec_lit(),
        SyntaxKind::KwIf => self.parse_if(),
        SyntaxKind::KwMatch => self.parse_match(),
        SyntaxKind::KwLet => self.parse_let(),
        SyntaxKind::KwDo => self.parse_do(),
        SyntaxKind::KwLoop => self.parse_loop(),
        SyntaxKind::Backslash => self.parse_lambda(),
        _ => {
            let tok = self.current_token().clone();
            self.diagnostics.push(
                Diagnostic::error(format!("unexpected token: {}", tok.kind))
                    .with_label(Label::primary(tok.span, "unexpected"))
                    .with_help("expressions start with a variable, literal, `let`, `if`, `match`, `loop`, or `(`"),
            );
            let span = tok.span;
            self.bump();
            // Return an error expression as a variable named `<error>`
            Expr::Var("<error>".to_owned(), span)
        }
    }
}

/// Parse a named record/bitfield construction: `Name { field = expr, ... }`
/// The `Name` has already been consumed; we're positioned at `{`.
pub(crate) fn parse_named_record(&mut self, name: String, start: u32) -> Expr {
    self.expect(SyntaxKind::LBrace);
    let mut fields = Vec::new();
    loop {
        self.skip_trivia();
        if self.at(SyntaxKind::RBrace) || self.at_end() {
            break;
        }
        let field_tok = self.expect(SyntaxKind::Ident);
        let field_name = self.text_of(&field_tok).to_owned();
        self.skip_trivia();
        self.expect(SyntaxKind::Equals);
        self.skip_trivia();
        let value = self.parse_expr();
        fields.push((field_name, value));
        self.skip_trivia();
        if !self.eat(SyntaxKind::Comma) {
            break;
        }
    }
    self.expect(SyntaxKind::RBrace);
    let span = self.span_from(start);
    Expr::Record(Some(name), fields, span)
}

pub(crate) fn parse_paren_expr(&mut self) -> Expr {
    let start = self.current_span().start;
    self.expect(SyntaxKind::LParen);
    self.skip_trivia();

    // Unit `()`
    if self.at(SyntaxKind::RParen) {
        self.bump();
        let span = self.span_from(start);
        return Expr::Tuple(Vec::new(), span);
    }

    // Operator section `(+)`, `(-)`, etc.
    // Special case: `(-expr)` is negation in parens, not an operator section.
    let is_shift_right_section = self.at(SyntaxKind::Greater)
        && {
            let next_idx = self.pos + 1;
            next_idx < self.tokens.len()
                && self.tokens[next_idx].kind == SyntaxKind::Greater
                && self.tokens[self.pos].span.end == self.tokens[next_idx].span.start
        };
    if is_operator_token(self.peek()) || is_shift_right_section {
        let current_op = if is_shift_right_section {
            ">>".to_owned()
        } else {
            self.text_of(self.current_token()).to_owned()
        };
        let is_prefix_unary = self.peek() == SyntaxKind::Minus
            || self.peek() == SyntaxKind::Bang
            || self.peek() == SyntaxKind::Tilde;
        // Peek ahead past operator + trivia to see if it's `(op)`
        let next_non_trivia = {
            let mut i = self.pos + if is_shift_right_section { 2 } else { 1 };
            while i < self.tokens.len() && self.tokens[i].kind.is_trivia() {
                i += 1;
            }
            if i < self.tokens.len() {
                self.tokens[i].kind
            } else {
                SyntaxKind::Eof
            }
        };
        if is_prefix_unary && next_non_trivia != SyntaxKind::RParen {
            // `(-expr)` or `(!expr)` — fall through to parse as normal expression
            // (the prefix operator will be handled by parse_expr)
        } else if next_non_trivia == SyntaxKind::RParen {
            // `(op)` — operator section
            if is_shift_right_section {
                self.bump();
                self.bump();
            } else {
                self.bump();
            }
            self.skip_trivia();
            self.bump(); // consume `)`
            let span = self.span_from(start);
            return Expr::OpSection(current_op, span);
        } else {
            // `(op expr)` — not a simple section, treat as error
            if is_shift_right_section {
                self.bump();
                self.bump();
            } else {
                self.bump();
            }
            while !self.at(SyntaxKind::RParen) && !self.at_end() && self.consume_fuel() {
                self.bump();
            }
            self.eat(SyntaxKind::RParen);
            let span = self.span_from(start);
            return Expr::OpSection(current_op, span);
        }
    }

    // Parse first expression
    let first = self.parse_expr();
    self.skip_trivia();

    // Tuple `(a, b, ...)`
    if self.at(SyntaxKind::Comma) {
        let mut elems = vec![first];
        while self.eat(SyntaxKind::Comma) {
            self.skip_trivia();
            elems.push(self.parse_expr());
            self.skip_trivia();
        }
        self.expect(SyntaxKind::RParen);
        let span = self.span_from(start);
        return Expr::Tuple(elems, span);
    }

    // Simple parenthesized expression
    self.expect(SyntaxKind::RParen);
    let span = self.span_from(start);
    Expr::Paren(Box::new(first), span)
}

pub(crate) fn parse_vec_lit(&mut self) -> Expr {
    let start = self.current_span().start;
    self.expect(SyntaxKind::LBracket);
    self.skip_trivia();

    let mut elems = Vec::new();

    // Empty vec literal `[]` is not valid for WGSL, but parse it anyway
    if !self.at(SyntaxKind::RBracket) && !self.at_end() {
        elems.push(self.parse_expr());
        self.skip_trivia();

        while self.eat(SyntaxKind::Comma) {
            self.skip_trivia();
            if self.at(SyntaxKind::RBracket) {
                break; // trailing comma
            }
            elems.push(self.parse_expr());
            self.skip_trivia();
        }
    }

    self.expect(SyntaxKind::RBracket);
    let span = self.span_from(start);
    Expr::VecLit(elems, span)
}

pub(crate) fn parse_if(&mut self) -> Expr {
    let start = self.current_span().start;
    self.expect(SyntaxKind::KwIf);
    self.skip_trivia();
    let cond = self.parse_expr();
    self.skip_trivia();
    self.expect(SyntaxKind::KwThen);
    self.skip_trivia();
    let then_expr = self.parse_expr();
    self.skip_trivia();
    self.expect(SyntaxKind::KwElse);
    self.skip_trivia();
    let else_expr = self.parse_expr();
    let span = self.span_from(start);
    Expr::If(
        Box::new(cond),
        Box::new(then_expr),
        Box::new(else_expr),
        span,
    )
}

pub(crate) fn parse_match(&mut self) -> Expr {
    let start = self.current_span().start;
    self.expect(SyntaxKind::KwMatch);
    self.skip_trivia();

    // Parse the scrutinee -- only parse atoms and applications,
    // stop before `|` (pipe) token. We use a limited expression parser.
    let scrutinee = self.parse_match_scrutinee();
    self.skip_trivia();

    // Parse arms: each starts with |
    let mut arms = Vec::new();
    loop {
        self.skip_trivia();
        if self.at_end() {
            break;
        }
        if !self.consume_fuel() {
            break;
        }
        if !self.at(SyntaxKind::Pipe) {
            break;
        }
        self.bump(); // consume |
        self.skip_trivia();

        let pat_start = self.current_span().start;
        let first_pat = self.parse_pat();
        self.skip_trivia();

        // Check for or-pattern: | pat1 | pat2 | pat3 -> body
        // After parsing first pattern, if we see `|` instead of `->`,
        // the `|` is an or-separator within the same arm.
        let pat = if self.at(SyntaxKind::Pipe) {
            let mut alternatives = vec![first_pat];
            while self.at(SyntaxKind::Pipe) {
                self.bump(); // consume |
                self.skip_trivia();
                alternatives.push(self.parse_pat());
                self.skip_trivia();
            }
            let span = self.span_from(pat_start);
            Pat::Or(alternatives, span)
        } else {
            first_pat
        };

        // Optional when-guard: `| pat when guard -> body`
        let guard = if self.at(SyntaxKind::KwWhen) {
            self.bump(); // consume `when`
            self.skip_trivia();
            Some(self.parse_expr())
        } else {
            None
        };

        self.expect(SyntaxKind::Arrow);
        self.skip_trivia();
        let body = self.parse_expr();
        arms.push((pat, guard, body));
    }

    let span = self.span_from(start);
    Expr::Case(Box::new(scrutinee), arms, span)
}

/// Parse the scrutinee of a match expression. Stops at `|` (Pipe) token or layout tokens.
pub(crate) fn parse_match_scrutinee(&mut self) -> Expr {
    self.skip_trivia();
    let mut expr = self.parse_match_scrutinee_atom();

    // Allow function application in scrutinee
    loop {
        self.skip_trivia();
        let k = self.peek_non_trivia();
        if k == SyntaxKind::Pipe
            || k == SyntaxKind::Eof
            || k == SyntaxKind::LayoutBraceOpen
            || k == SyntaxKind::LayoutSemicolon
            || k == SyntaxKind::LayoutBraceClose
        {
            break;
        }
        if !is_atom_start(k) {
            break;
        }
        if !self.consume_fuel() {
            break;
        }
        self.skip_trivia();
        let arg = self.parse_match_scrutinee_atom();
        let span = expr.span().merge(arg.span());
        expr = Expr::App(Box::new(expr), Box::new(arg), span);
    }

    expr
}

pub(crate) fn parse_match_scrutinee_atom(&mut self) -> Expr {
    self.skip_trivia();
    match self.peek() {
        SyntaxKind::Ident => {
            let tok = self.bump();
            let name = self.text_of(&tok).to_owned();
            Expr::Var(name, tok.span)
        }
        SyntaxKind::UpperIdent => {
            let tok = self.bump();
            let name = self.text_of(&tok).to_owned();
            Expr::Con(name, tok.span)
        }
        SyntaxKind::IntLiteral => {
            let tok = self.bump();
            let text = self.text_of(&tok);
            match parse_int_literal_typed(text) {
                ParsedInt::Unsigned(v) => Expr::Lit(Lit::UInt(v), tok.span),
                ParsedInt::Signed(v) => Expr::Lit(Lit::Int(v), tok.span),
            }
        }
        SyntaxKind::LParen => self.parse_paren_expr(),
        _ => {
            let tok = self.current_token().clone();
            let span = tok.span;
            Expr::Var("<error>".to_owned(), span)
        }
    }
}

pub(crate) fn parse_let(&mut self) -> Expr {
    let start = self.current_span().start;
    self.expect(SyntaxKind::KwLet);
    self.skip_trivia();

    // Layout brace open
    self.eat(SyntaxKind::LayoutBraceOpen);

    let mut binds = Vec::new();
    loop {
        self.skip_trivia();
        if self.at(SyntaxKind::KwIn) || self.at_layout_end() || self.at_end() {
            break;
        }
        if !self.consume_fuel() {
            break;
        }

        if self.at(SyntaxKind::Ident) {
            let name_tok = self.bump();
            let name = self.text_of(&name_tok).to_owned();
            let bind_start = name_tok.span.start;
            self.skip_trivia();
            self.expect(SyntaxKind::Equals);
            self.skip_trivia();
            let expr = self.parse_expr();
            binds.push(LocalBind {
                name,
                name_span: name_tok.span,
                expr,
                span: self.span_from(bind_start),
            });
            self.eat_layout_semi();
        } else {
            break;
        }
    }

    self.eat_layout_close();
    self.skip_trivia();
    self.expect(SyntaxKind::KwIn);
    self.skip_trivia();
    let body = self.parse_expr();
    let span = self.span_from(start);
    Expr::Let(binds, Box::new(body), span)
}

pub(crate) fn parse_do(&mut self) -> Expr {
    let start = self.current_span().start;
    self.expect(SyntaxKind::KwDo);
    self.skip_trivia();

    // Layout brace open
    self.eat(SyntaxKind::LayoutBraceOpen);

    let mut stmts = Vec::new();
    loop {
        self.skip_trivia();
        if self.at_layout_end() || self.at_end() {
            break;
        }
        if !self.consume_fuel() {
            break;
        }

        let stmt_start = self.current_span().start;

        // `let x = expr`
        if self.at(SyntaxKind::KwLet) {
            self.bump();
            self.skip_trivia();
            // Consume possible LayoutBraceOpen from let in do
            self.eat(SyntaxKind::LayoutBraceOpen);
            self.skip_trivia();
            let name_tok = self.expect(SyntaxKind::Ident);
            let name = self.text_of(&name_tok).to_owned();
            self.skip_trivia();
            self.expect(SyntaxKind::Equals);
            self.skip_trivia();
            let expr = self.parse_expr();
            let span = self.span_from(stmt_start);
            stmts.push(DoStmt::Let(LocalBind {
                name,
                name_span: name_tok.span,
                expr,
                span,
            }));
            self.eat_layout_semi();
            self.eat_layout_close();
            continue;
        }

        // Try to detect `x <- expr` pattern:
        // peek: Ident, then LeftArrow
        if self.at(SyntaxKind::Ident) {
            // Lookahead for `<-`
            let saved_pos = self.pos;
            let name_tok = self.bump();
            self.skip_trivia();
            if self.at(SyntaxKind::LeftArrow) {
                self.bump(); // consume `<-`
                self.skip_trivia();
                let name = self.text_of(&name_tok).to_owned();
                let expr = self.parse_expr();
                let span = self.span_from(stmt_start);
                stmts.push(DoStmt::Bind(LocalBind {
                    name,
                    name_span: name_tok.span,
                    expr,
                    span,
                }));
                self.eat_layout_semi();
                continue;
            } else {
                // Backtrack -- it's a plain expression
                self.pos = saved_pos;
            }
        }

        // Plain expression statement
        let expr = self.parse_expr();
        let span = self.span_from(stmt_start);
        stmts.push(DoStmt::Expr(expr, span));
        self.eat_layout_semi();
    }

    self.eat_layout_close();

    let span = self.span_from(start);
    Expr::Do(stmts, span)
}

/// Parse `loop go (i = 0) (acc = 0) in body`.
///
/// Named tail-recursive loop. Each binding is parenthesised: `(name = init)`.
/// The body may call `go arg1 arg2` to recurse (continue the loop).
pub(crate) fn parse_loop(&mut self) -> Expr {
    let start = self.current_span().start;
    self.expect(SyntaxKind::KwLoop);
    self.skip_trivia();

    // Parse loop name (e.g. `go`)
    let name_tok = self.expect(SyntaxKind::Ident);
    let name = self.text_of(&name_tok).to_owned();
    self.skip_trivia();

    // Parse bindings: `(name = init)` repeated
    let mut bindings = Vec::new();
    while self.at(SyntaxKind::LParen) && self.consume_fuel() {
        let bind_start = self.current_span().start;
        self.expect(SyntaxKind::LParen);
        self.skip_trivia();
        let bind_tok = self.expect(SyntaxKind::Ident);
        let bind_name = self.text_of(&bind_tok).to_owned();
        self.skip_trivia();
        self.expect(SyntaxKind::Equals);
        self.skip_trivia();
        let init = self.parse_expr();
        self.skip_trivia();
        self.expect(SyntaxKind::RParen);
        self.skip_trivia();
        bindings.push(LocalBind {
            name: bind_name,
            name_span: bind_tok.span,
            expr: init,
            span: self.span_from(bind_start),
        });
    }

    // `in` keyword
    self.expect(SyntaxKind::KwIn);
    self.skip_trivia();

    // Body expression
    let body = self.parse_expr();
    let span = self.span_from(start);
    Expr::Loop(name, bindings, Box::new(body), span)
}

pub(crate) fn parse_lambda(&mut self) -> Expr {
    let start = self.current_span().start;
    self.expect(SyntaxKind::Backslash);
    self.skip_trivia();

    let mut params = Vec::new();
    while !self.at(SyntaxKind::Arrow) && !self.at_end() && self.consume_fuel() {
        let p = self.parse_pat_atom();
        params.push(p);
        self.skip_trivia();
    }

    self.expect(SyntaxKind::Arrow);
    self.skip_trivia();
    let body = self.parse_expr();
    let span = self.span_from(start);
    Expr::Lambda(params, Box::new(body), span)
}

// ═════════════════════════════════════════════════════════════════════
// Patterns
// ═════════════════════════════════════════════════════════════════════

}
