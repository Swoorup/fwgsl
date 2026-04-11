use super::*;

impl Parser {
    pub(crate) fn parse_pat(&mut self) -> Pat {
        self.skip_trivia();
        let start = self.current_span().start;

        match self.peek() {
            SyntaxKind::UpperIdent => {
                // Constructor pattern, possibly with sub-patterns
                let name_tok = self.bump();
                let name = self.text_of(&name_tok).to_owned();
                self.skip_trivia();

                // Check for record pattern Con { ... }
                if self.at(SyntaxKind::LBrace) {
                    return self.parse_record_pat(name, start);
                }

                // Collect sub-patterns (atoms only)
                let mut sub_pats = Vec::new();
                while is_pat_atom_start(self.peek_non_trivia()) && self.consume_fuel() {
                    self.skip_trivia();
                    sub_pats.push(self.parse_pat_atom());
                }
                let span = self.span_from(start);
                Pat::Con(name, sub_pats, span)
            }
            _ => self.parse_pat_atom(),
        }
    }

    pub(crate) fn parse_pat_atom(&mut self) -> Pat {
        self.skip_trivia();
        let start = self.current_span().start;

        match self.peek() {
            SyntaxKind::Underscore => {
                let tok = self.bump();
                Pat::Wild(tok.span)
            }
            SyntaxKind::Ident => {
                let tok = self.bump();
                let name = self.text_of(&tok).to_owned();
                // Check for as-pattern: name@pat
                if self.at(SyntaxKind::At) {
                    self.bump();
                    self.skip_trivia();
                    let inner = self.parse_pat_atom();
                    let span = self.span_from(start);
                    Pat::As(name, Box::new(inner), span)
                } else {
                    Pat::Var(name, tok.span)
                }
            }
            SyntaxKind::UpperIdent => {
                let tok = self.bump();
                let name = self.text_of(&tok).to_owned();
                Pat::Con(name, Vec::new(), tok.span)
            }
            SyntaxKind::IntLiteral => {
                let tok = self.bump();
                let text = self.text_of(&tok);
                match parse_int_literal_typed(text) {
                    ParsedInt::Unsigned(v) => Pat::Lit(Lit::UInt(v), tok.span),
                    ParsedInt::Signed(v) => Pat::Lit(Lit::Int(v), tok.span),
                }
            }
            SyntaxKind::FloatLiteral => {
                let tok = self.bump();
                let text = self.text_of(&tok);
                let val: f64 = text.parse().unwrap_or(0.0);
                Pat::Lit(Lit::Float(val), tok.span)
            }
            SyntaxKind::StringLiteral => {
                let tok = self.bump();
                let text = self.text_of(&tok);
                let inner = &text[1..text.len() - 1];
                Pat::Lit(Lit::String(unescape_string(inner)), tok.span)
            }
            SyntaxKind::CharLiteral => {
                let tok = self.bump();
                let text = self.text_of(&tok);
                let inner = &text[1..text.len() - 1];
                Pat::Lit(Lit::Char(unescape_char(inner)), tok.span)
            }
            SyntaxKind::LParen => {
                self.bump();
                self.skip_trivia();
                // Unit `()`
                if self.at(SyntaxKind::RParen) {
                    self.bump();
                    let span = self.span_from(start);
                    return Pat::Tuple(Vec::new(), span);
                }
                let first = self.parse_pat();
                self.skip_trivia();
                if self.at(SyntaxKind::Comma) {
                    // Tuple pattern
                    let mut elems = vec![first];
                    while self.eat(SyntaxKind::Comma) {
                        self.skip_trivia();
                        elems.push(self.parse_pat());
                        self.skip_trivia();
                    }
                    self.expect(SyntaxKind::RParen);
                    let span = self.span_from(start);
                    Pat::Tuple(elems, span)
                } else {
                    self.expect(SyntaxKind::RParen);
                    let span = self.span_from(start);
                    Pat::Paren(Box::new(first), span)
                }
            }
            _ => {
                let tok = self.current_token().clone();
                self.diagnostics.push(
                Diagnostic::error(format!("unexpected token in pattern: {}", tok.kind))
                    .with_label(Label::primary(tok.span, "unexpected"))
                    .with_help("patterns can be: variables, constructors, literals, wildcards (_), or tuples"),
            );
                let span = tok.span;
                self.bump();
                Pat::Wild(span)
            }
        }
    }

    pub(crate) fn parse_record_pat(&mut self, con_name: String, start: u32) -> Pat {
        self.expect(SyntaxKind::LBrace);
        let mut fields = Vec::new();
        let mut has_rest = false;
        loop {
            self.skip_trivia();
            if self.at(SyntaxKind::RBrace) || self.at_end() {
                break;
            }

            if self.at(SyntaxKind::DotDot) {
                self.bump();
                has_rest = true;
                self.skip_trivia();
                break;
            }

            let field_tok = self.expect(SyntaxKind::Ident);
            let field_name = self.text_of(&field_tok).to_owned();
            self.skip_trivia();

            if self.at(SyntaxKind::Equals) {
                self.bump();
                self.skip_trivia();
                let pat = self.parse_pat();
                fields.push((field_name, Some(pat)));
            } else {
                // Punned field: `field` means `field = field`
                fields.push((field_name, None));
            }

            self.skip_trivia();
            if self.eat(SyntaxKind::Comma) {
                self.skip_trivia();
                if self.at(SyntaxKind::DotDot) {
                    self.bump();
                    has_rest = true;
                    self.skip_trivia();
                    break;
                }
            } else {
                break;
            }
        }
        self.expect(SyntaxKind::RBrace);
        let span = self.span_from(start);
        Pat::Record(con_name, fields, has_rest, span)
    }

    // ═════════════════════════════════════════════════════════════════════
    // Types
    // ═════════════════════════════════════════════════════════════════════
}
