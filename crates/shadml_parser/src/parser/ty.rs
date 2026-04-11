use super::*;

impl Parser {
    pub fn parse_type(&mut self) -> Type {
        self.skip_trivia();
        let lhs = self.parse_type_app();
        self.skip_trivia();

        // Arrow types are right-associative
        if self.at(SyntaxKind::Arrow) {
            self.bump();
            self.skip_trivia();
            let rhs = self.parse_type(); // right-recursive for right-assoc
            let span = lhs.span().merge(rhs.span());
            Type::Arrow(Box::new(lhs), Box::new(rhs), span)
        } else {
            lhs
        }
    }

    pub(crate) fn parse_type_app(&mut self) -> Type {
        self.skip_trivia();
        let start_col = self.column_of(self.current_span().start);
        let mut ty = self.parse_type_atom();

        loop {
            self.skip_trivia();
            // Stop at layout boundaries — a LayoutSemicolon means we're on a
            // new declaration line and should not consume further type args.
            if matches!(
                self.peek(),
                SyntaxKind::LayoutSemicolon | SyntaxKind::LayoutBraceClose
            ) {
                break;
            }
            let next = self.peek_non_trivia();
            // Also stop if the next token is on a new line at the same or
            // lesser column — this handles `when` blocks where the layout
            // resolver doesn't insert LayoutSemicolon tokens.
            let next_col = self.column_of(self.current_span().start);
            if next_col <= start_col && self.current_span().start > ty.span().end {
                break;
            }
            if matches!(
                next,
                SyntaxKind::UpperIdent
                    | SyntaxKind::Ident
                    | SyntaxKind::IntLiteral
                    | SyntaxKind::LParen
            ) {
                let arg = self.parse_type_atom();
                let span = ty.span().merge(arg.span());
                ty = Type::App(Box::new(ty), Box::new(arg), span);
            } else {
                break;
            }
        }

        ty
    }

    /// Try to parse `type UpperIdent = Type` (associated type definition).
    /// Returns `Some(AssociatedTypeDef)` if the current position matches,
    /// `None` otherwise (without consuming tokens).
    pub(crate) fn try_parse_assoc_type_def(&mut self) -> Option<AssociatedTypeDef> {
        if self.at(SyntaxKind::KwType) && self.peek_after_current() == SyntaxKind::UpperIdent {
            let type_start = self.current_span().start;
            self.bump(); // consume "type"
            self.skip_trivia();
            let name_tok = self.expect(SyntaxKind::UpperIdent);
            let assoc_name = self.text_of(&name_tok).to_owned();
            self.skip_trivia();
            self.expect(SyntaxKind::Equals);
            self.skip_trivia();
            let ty = self.parse_type();
            let assoc_span = self.span_from(type_start);
            self.skip_trivia();
            self.eat_layout_semi();
            Some(AssociatedTypeDef {
                name: assoc_name,
                ty,
                span: assoc_span,
            })
        } else {
            None
        }
    }

    /// Try to parse `type UpperIdent` (associated type declaration in a trait).
    /// Returns `Some(AssociatedTypeDecl)` if the current position matches,
    /// `None` otherwise (without consuming tokens).
    pub(crate) fn try_parse_assoc_type_decl(&mut self) -> Option<AssociatedTypeDecl> {
        if self.at(SyntaxKind::KwType) && self.peek_after_current() == SyntaxKind::UpperIdent {
            let type_start = self.current_span().start;
            self.bump(); // consume "type"
            self.skip_trivia();
            let name_tok = self.expect(SyntaxKind::UpperIdent);
            let assoc_name = self.text_of(&name_tok).to_owned();
            let assoc_span = self.span_from(type_start);
            self.skip_trivia();
            self.eat_layout_semi();
            Some(AssociatedTypeDecl {
                name: assoc_name,
                span: assoc_span,
            })
        } else {
            None
        }
    }

    pub(crate) fn parse_type_atom(&mut self) -> Type {
        self.skip_trivia();
        let start = self.current_span().start;

        let base = match self.peek() {
            SyntaxKind::UpperIdent => {
                let tok = self.bump();
                let name = self.text_of(&tok).to_owned();
                self.parse_angle_type_args(Type::Con(name, tok.span))
            }
            SyntaxKind::KwSelf => {
                let tok = self.bump();
                Type::Self_(tok.span)
            }
            SyntaxKind::Ident => {
                let tok = self.bump();
                let name = self.text_of(&tok).to_owned();
                Type::Var(name, tok.span)
            }
            SyntaxKind::IntLiteral => {
                let tok = self.bump();
                let value = parse_int_literal(self.text_of(&tok)).max(0) as u64;
                Type::Nat(value, tok.span)
            }
            SyntaxKind::LParen => {
                self.bump();
                self.skip_trivia();

                // Unit type `()`
                if self.at(SyntaxKind::RParen) {
                    self.bump();
                    let span = self.span_from(start);
                    return Type::Unit(span);
                }

                let first = self.parse_type();
                self.skip_trivia();

                if self.at(SyntaxKind::Comma) {
                    // Tuple type
                    let mut elems = vec![first];
                    while self.eat(SyntaxKind::Comma) {
                        self.skip_trivia();
                        elems.push(self.parse_type());
                        self.skip_trivia();
                    }
                    self.expect(SyntaxKind::RParen);
                    let span = self.span_from(start);
                    Type::Tuple(elems, span)
                } else {
                    self.expect(SyntaxKind::RParen);
                    let span = self.span_from(start);
                    Type::Paren(Box::new(first), span)
                }
            }
            _ => {
                let tok = self.current_token().clone();
                self.diagnostics.push(
                    Diagnostic::error(format!("unexpected token in type: {}", tok.kind))
                        .with_label(Label::primary(tok.span, "unexpected"))
                        .with_help("check the type expression syntax around this token"),
                );
                let span = tok.span;
                self.bump();
                Type::Con("<error>".to_owned(), span)
            }
        };

        // Parse type projections: `a.Output`
        self.parse_type_projections(base)
    }

    /// Parse chained type projections like `a.Output.SubType`
    pub(crate) fn parse_type_projections(&mut self, mut base: Type) -> Type {
        loop {
            self.skip_trivia();
            if self.at(SyntaxKind::Dot) {
                self.bump(); // consume `.`
                self.skip_trivia();
                let name_tok = self.expect(SyntaxKind::UpperIdent);
                let name = self.text_of(&name_tok).to_owned();
                let span = base.span().merge(name_tok.span);
                base = Type::Proj(Box::new(base), name, span);
            } else {
                break;
            }
        }
        base
    }

    pub(crate) fn parse_angle_type_args(&mut self, mut base: Type) -> Type {
        self.skip_trivia();
        if !self.at(SyntaxKind::Less) {
            return base;
        }
        self.bump(); // <
        self.skip_trivia();

        loop {
            let arg = self.parse_type();
            let span = base.span().merge(arg.span());
            base = Type::App(Box::new(base), Box::new(arg), span);
            self.skip_trivia();
            if self.eat(SyntaxKind::Comma) {
                self.skip_trivia();
                continue;
            }
            break;
        }

        let end = self.expect(SyntaxKind::Greater);
        match &mut base {
            Type::Con(_, span)
            | Type::Var(_, span)
            | Type::Nat(_, span)
            | Type::App(_, _, span)
            | Type::Arrow(_, _, span)
            | Type::Paren(_, span)
            | Type::Tuple(_, span)
            | Type::Unit(span)
            | Type::Proj(_, _, span)
            | Type::Self_(span) => {
                *span = span.merge(end.span);
            }
        }
        base
    }
}
