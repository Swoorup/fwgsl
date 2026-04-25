use super::*;

const ATTR_GROUP: &str = "group";
const ATTR_BINDING: &str = "binding";
const GUARD_OTHERWISE: &str = "otherwise";
const BITFIELD_BOOL: &str = "Bool";

impl Parser {
    pub(crate) fn parse_decl(&mut self) -> Option<Decl> {
        // Drain any buffered decls from group block expansion first.
        if !self.pending_decls.is_empty() {
            return Some(self.pending_decls.remove(0));
        }
        self.parse_decl_core(DeclContext::ModuleScope)
    }

    /// Shared declaration parser used by both module-scope and render-block
    /// contexts.  Does **not** drain `pending_decls` — callers must do that
    /// themselves (as `parse_decl` and `parse_render_block` already do).
    pub(crate) fn parse_decl_core(&mut self, ctx: DeclContext) -> Option<Decl> {
        self.skip_trivia();
        match self.peek_non_trivia() {
            SyntaxKind::At => {
                // Peek ahead to determine if this is a binding declaration
                // (`@group(N) @binding(N) uniform/storage ...`) or an entry
                // point (`@vertex`, `@compute`, etc.).
                let start = self.current_span().start;

                // Check if the first attribute name is "group" — indicates a binding
                let is_binding = self.is_binding_decl_ahead();
                if is_binding {
                    let mut decls = self.parse_binding_decls(start);
                    if decls.is_empty() {
                        return None;
                    }
                    let first = decls.remove(0);
                    self.pending_decls = decls;
                    Some(first)
                } else {
                    // Parse attributes, then determine if this is an entry point
                    // (has shader stage attribute) or a function declaration.
                    let mut attributes = Vec::new();
                    while self.peek_non_trivia() == SyntaxKind::At {
                        self.skip_trivia();
                        attributes.push(self.parse_attribute());
                        self.skip_trivia();
                    }

                    // Check if any attribute is a shader stage (compute/vertex/fragment)
                    let is_entry_point = attributes
                        .iter()
                        .any(|a| matches!(a.name.as_str(), "compute" | "vertex" | "fragment"));

                    // Consume LayoutSemicolon between attributes and the declaration
                    self.eat_layout_semi();
                    self.skip_trivia();

                    // Parse optional type signature (e.g. `vsMain : VertexInput -> VertexOutput`)
                    // and preserve it as a separate declaration so it is registered in the
                    // type environment.  Without this, parameter types default to I32.
                    let type_sig_decl = if self.at(SyntaxKind::Ident) {
                        let saved = self.pos;
                        let name_tok = self.bump();
                        self.skip_trivia();
                        if self.at(SyntaxKind::Colon) {
                            let name = self.text_of(&name_tok).to_owned();
                            let ty_sig = self.parse_type_sig(name, name_tok.span.start);
                            self.eat_layout_semi();
                            self.skip_trivia();
                            Some(ty_sig)
                        } else {
                            self.pos = saved;
                            None
                        }
                    } else {
                        None
                    };

                    // Now parse the actual function declaration
                    self.skip_trivia();
                    let name_tok = self.expect(SyntaxKind::Ident);
                    let name = self.text_of(&name_tok).to_owned();
                    self.skip_trivia();

                    if is_entry_point {
                        let mut params = Vec::new();
                        while !self.at(SyntaxKind::Equals) && !self.at_end() && self.consume_fuel()
                        {
                            if matches!(
                                self.peek_non_trivia(),
                                SyntaxKind::LayoutSemicolon | SyntaxKind::LayoutBraceClose
                            ) {
                                break;
                            }
                            let p = self.parse_pat_atom();
                            params.push(p);
                            self.skip_trivia();
                        }

                        self.expect(SyntaxKind::Equals);
                        self.skip_trivia();
                        let body = self.parse_expr();

                        let span = self.span_from(start);
                        let entry_decl = Decl::EntryPoint {
                            attributes,
                            name,
                            params,
                            body,
                            span,
                            comments: vec![],
                        };
                        if let Some(ty_sig) = type_sig_decl {
                            self.pending_decls.push(entry_decl);
                            Some(ty_sig)
                        } else {
                            Some(entry_decl)
                        }
                    } else {
                        // Non-stage attributes (e.g. @const) → FunDecl
                        let fun_decl = self.parse_fun_decl(name, start, attributes);
                        if let Some(ty_sig) = type_sig_decl {
                            self.pending_decls.push(fun_decl);
                            Some(ty_sig)
                        } else {
                            Some(fun_decl)
                        }
                    }
                }
            }
            SyntaxKind::KwModule => {
                if matches!(ctx, DeclContext::ModuleScope) {
                    Some(self.parse_module_decl())
                } else {
                    None
                }
            }
            SyntaxKind::KwRender => {
                if matches!(ctx, DeclContext::ModuleScope) {
                    Some(self.parse_render_block())
                } else {
                    None
                }
            }
            SyntaxKind::KwImport => {
                if matches!(ctx, DeclContext::ModuleScope) {
                    Some(self.parse_import_decl())
                } else {
                    None
                }
            }
            SyntaxKind::KwData => Some(self.parse_data_decl()),
            SyntaxKind::KwAlias => Some(self.parse_alias_decl()),
            SyntaxKind::KwBuiltin => {
                if matches!(ctx, DeclContext::ModuleScope) {
                    Some(self.parse_builtin_decl())
                } else {
                    None
                }
            }
            SyntaxKind::KwExtern => {
                if matches!(ctx, DeclContext::ModuleScope) {
                    Some(self.parse_extern_decl())
                } else {
                    None
                }
            }
            SyntaxKind::KwUniform | SyntaxKind::KwStorage => {
                // Bare `uniform`/`storage` without `@group(...)` — parse as binding
                // with default group(0) binding(0). This supports shorthand usage.
                let start = self.current_span().start;
                Some(self.parse_binding_body(start, 0, 0))
            }
            SyntaxKind::KwImmediate => {
                // `immediate name : Type` — push constant without @group/@binding
                let start = self.current_span().start;
                Some(self.parse_binding_body(start, 0, 0))
            }
            SyntaxKind::KwBitfield => {
                if matches!(ctx, DeclContext::ModuleScope) {
                    Some(self.parse_bitfield_decl())
                } else {
                    None
                }
            }
            SyntaxKind::KwConst => Some(self.parse_const_decl()),
            SyntaxKind::KwTrait => {
                if matches!(ctx, DeclContext::ModuleScope) {
                    Some(self.parse_trait_decl())
                } else {
                    None
                }
            }
            SyntaxKind::KwImpl => {
                if matches!(ctx, DeclContext::ModuleScope) {
                    Some(self.parse_impl_decl())
                } else {
                    None
                }
            }
            SyntaxKind::KwWhen => {
                if matches!(ctx, DeclContext::ModuleScope) {
                    Some(self.parse_when_decl())
                } else {
                    None
                }
            }
            SyntaxKind::Ident => {
                // Could be a type signature or function declaration.
                // Look ahead: name then `:` means type sig; otherwise fun decl.
                self.skip_trivia();
                let name_tok = self.bump();
                let name = self.text_of(&name_tok).to_owned();
                let start = name_tok.span.start;

                self.skip_trivia();
                if self.at(SyntaxKind::Colon) {
                    Some(self.parse_type_sig(name, start))
                } else {
                    Some(self.parse_fun_decl(name, start, vec![]))
                }
            }
            _ => None,
        }
    }

    pub(crate) fn parse_type_sig(&mut self, name: String, start: u32) -> Decl {
        self.expect(SyntaxKind::Colon); // consume `:`
        self.skip_trivia();
        let lhs = self.parse_type();
        self.skip_trivia();
        let (constraints, ty) = if self.at(SyntaxKind::FatArrow) {
            self.bump();
            self.skip_trivia();
            let constraint = self
                .type_to_constraint(&lhs)
                .map(|constraint| vec![constraint])
                .unwrap_or_default();
            (constraint, self.parse_type())
        } else {
            (vec![], lhs)
        };
        let span = self.span_from(start);
        Decl::TypeSig {
            name,
            constraints,
            ty,
            span,
            comments: vec![],
        }
    }

    pub(crate) fn type_to_constraint(&mut self, ty: &Type) -> Option<TypeConstraint> {
        fn flatten_type_app<'a>(ty: &'a Type, out: &mut Vec<&'a Type>) {
            match ty {
                Type::App(f, arg, _) => {
                    flatten_type_app(f, out);
                    out.push(arg);
                }
                Type::Paren(inner, _) => flatten_type_app(inner, out),
                other => out.push(other),
            }
        }

        let mut parts = Vec::new();
        flatten_type_app(ty, &mut parts);
        match parts.split_first() {
            Some((Type::Con(trait_name, _), tys)) if !tys.is_empty() => Some(TypeConstraint {
                trait_name: trait_name.clone(),
                tys: tys.iter().map(|ty| (*ty).clone()).collect(),
                span: ty.span(),
            }),
            _ => {
                self.diagnostics.push(
                    Diagnostic::error("expected trait constraint before `=>`")
                        .with_label(Label::primary(ty.span(), "expected `TraitName t1 ... tn`"))
                        .with_help("write constraints like `Mul a b c => ...`"),
                );
                None
            }
        }
    }

    pub(crate) fn parse_fun_decl(
        &mut self,
        name: String,
        start: u32,
        attributes: Vec<Attribute>,
    ) -> Decl {
        // Parse patterns before `=` or `|` (guard)
        let mut params = Vec::new();
        self.skip_trivia();
        while !self.at(SyntaxKind::Equals)
            && !self.at(SyntaxKind::Pipe)
            && !self.at_end()
            && self.consume_fuel()
        {
            if matches!(
                self.peek_non_trivia(),
                SyntaxKind::LayoutSemicolon | SyntaxKind::LayoutBraceClose
            ) {
                break;
            }
            let p = self.parse_pat_atom();
            params.push(p);
            self.skip_trivia();
        }

        // Guard clauses: f x | cond1 = body1 | cond2 = body2 | otherwise = bodyN
        if self.at(SyntaxKind::Pipe) {
            let body = self.parse_guard_clauses(start);

            // Optional `where` clause
            let mut where_binds = Vec::new();
            self.skip_trivia();
            if self.eat(SyntaxKind::KwWhere) {
                where_binds = self.parse_where_binds();
            }

            let span = self.span_from(start);
            return Decl::FunDecl {
                name,
                params,
                body,
                where_binds,
                span,
                comments: vec![],
                attributes,
            };
        }

        self.expect(SyntaxKind::Equals);
        self.skip_trivia();
        let body = self.parse_expr();

        // Optional `where` clause
        let mut where_binds = Vec::new();
        self.skip_trivia();
        if self.eat(SyntaxKind::KwWhere) {
            where_binds = self.parse_where_binds();
        }

        let span = self.span_from(start);
        Decl::FunDecl {
            name,
            params,
            body,
            where_binds,
            span,
            comments: vec![],
            attributes,
        }
    }

    /// Parse guard clauses and desugar into nested if-then-else.
    /// `| cond1 = body1 | cond2 = body2 | otherwise = bodyN`
    pub(crate) fn parse_guard_clauses(&mut self, start: u32) -> Expr {
        let mut guards: Vec<(Expr, Expr)> = Vec::new();

        while self.at(SyntaxKind::Pipe) && self.consume_fuel() {
            self.bump(); // consume `|`
            self.skip_trivia();
            let cond = self.parse_expr();
            self.skip_trivia();
            self.expect(SyntaxKind::Equals);
            self.skip_trivia();
            let body = self.parse_expr();
            guards.push((cond, body));
            self.skip_trivia();
            // Consume layout semicolons between guards
            self.eat_layout_semi();
            self.skip_trivia();
        }

        // Desugar: fold guards right into if-then-else chain.
        // The last guard is treated as the else branch if its condition is
        // `otherwise` (i.e. Var(GUARD_OTHERWISE)) or `True`.
        if guards.is_empty() {
            let span = self.span_from(start);
            return Expr::Var("<error>".to_owned(), span);
        }

        let mut result = None;
        for (cond, body) in guards.into_iter().rev() {
            let span = cond.span().merge(body.span());
            if let Expr::Var(ref name, _) = cond {
                if name == GUARD_OTHERWISE {
                    // `otherwise` guard: becomes the else branch
                    result = Some(body);
                    continue;
                }
            }
            let else_branch = result.unwrap_or({
                // Fallback: return a zero literal if no otherwise clause
                Expr::Lit(Lit::Int(0), span)
            });
            result = Some(Expr::If(
                Box::new(cond),
                Box::new(body),
                Box::new(else_branch),
                span,
            ));
        }

        result.unwrap_or_else(|| {
            let span = self.span_from(start);
            Expr::Var("<error>".to_owned(), span)
        })
    }

    pub(crate) fn parse_where_binds(&mut self) -> Vec<LocalBind> {
        let mut binds = Vec::new();
        // Layout should have inserted LayoutBraceOpen
        self.skip_trivia();
        // Consume optional layout brace open
        self.eat(SyntaxKind::LayoutBraceOpen);

        loop {
            self.skip_trivia();
            if self.at_layout_end() || self.at_end() {
                break;
            }
            if !self.consume_fuel() {
                break;
            }

            if self.at(SyntaxKind::Ident) {
                let name_tok = self.bump();
                let name = self.text_of(&name_tok).to_owned();
                let start = name_tok.span.start;
                self.skip_trivia();
                self.expect(SyntaxKind::Equals);
                self.skip_trivia();
                let expr = self.parse_expr();
                binds.push(LocalBind {
                    name,
                    name_span: name_tok.span,
                    expr,
                    span: self.span_from(start),
                });
                self.eat_layout_semi();
            } else {
                break;
            }
        }

        self.eat_layout_close();
        binds
    }

    /// Parse `module Foo.Bar`.
    pub(crate) fn parse_module_decl(&mut self) -> Decl {
        let start = self.current_span().start;
        self.expect(SyntaxKind::KwModule);
        self.skip_trivia();

        let name = self.parse_dotted_upper_name();

        let span = self.span_from(start);
        Decl::ModuleDecl {
            name,
            span,
            comments: vec![],
        }
    }

    /// Parse a render block:
    ///   `render name { bindings; entry_points }`
    ///
    /// The block uses layout-based syntax — the layout resolver inserts
    /// `LayoutBraceOpen`, `LayoutSemicolon`, and `LayoutBraceClose` tokens
    /// around the indented body.
    pub(crate) fn parse_render_block(&mut self) -> Decl {
        let start = self.current_span().start;
        self.expect(SyntaxKind::KwRender);
        self.skip_trivia();

        let name_tok = self.expect(SyntaxKind::Ident);
        let name = self.text_of(&name_tok).to_owned();
        self.skip_trivia();

        // Consume layout brace open (inserted by layout resolver for the block body)
        self.eat(SyntaxKind::LayoutBraceOpen);

        let mut bindings = Vec::new();
        let mut entries = Vec::new();

        loop {
            // Drain any pending decls first (e.g. an EntryPoint buffered after
            // its preceding TypeSig), before consuming layout tokens or checking
            // for EOF.  This mirrors the top-level `parse_program` loop.
            if !self.pending_decls.is_empty() {
                let decl = self.pending_decls.remove(0);
                match decl {
                    d @ Decl::BindingDecl { .. } => bindings.push(d),
                    d @ Decl::EntryPoint { .. } => entries.push(d),
                    d @ Decl::TypeSig { .. } => entries.push(d),
                    other => entries.push(other),
                }
                continue;
            }

            self.skip_trivia();
            self.eat_layout_semi();
            self.skip_trivia();

            // Check for end of block
            if self.at_layout_end() || self.at_end() {
                break;
            }

            // Delegate to parse_decl_core for everything else.
            if let Some(decl) = self.parse_decl_core(DeclContext::RenderBlock) {
                match decl {
                    d @ Decl::BindingDecl { .. } => bindings.push(d),
                    d @ Decl::EntryPoint { .. } => entries.push(d),
                    // TypeSig declarations preceding an entry point — keep
                    // them in entries so the type environment is populated.
                    d @ Decl::TypeSig { .. } => entries.push(d),
                    other => {
                        // Other decls inside a render block (data, fun, etc.)
                        // are valid — treat them as module-scope items but also
                        // record them so the bundler can see them.
                        entries.push(other);
                    }
                }
            } else {
                // Error recovery: skip one token
                self.bump();
            }
        }

        self.eat_layout_close();

        let span = self.span_from(start);
        Decl::RenderBlock {
            name,
            bindings,
            entries,
            span,
            comments: vec![],
        }
    }

    /// Parse import declarations:
    ///   `import Foo.Bar`            — all public names
    ///   `import Foo.Bar (x, y)`     — selective
    ///   `import Foo.Bar as F`       — qualified
    ///   `import Foo.*`              — wildcard (all sub-modules)
    pub(crate) fn parse_import_decl(&mut self) -> Decl {
        let start = self.current_span().start;
        self.expect(SyntaxKind::KwImport);
        self.skip_trivia();

        let module_path = self.parse_dotted_upper_name();
        self.skip_trivia();

        // Check for wildcard: `import Foo.*`
        let (module_path, kind) = if self.at(SyntaxKind::Dot) {
            // Peek ahead for `*`
            let saved_pos = self.pos;
            self.bump(); // consume `.`
            if self.at(SyntaxKind::Star) {
                self.bump(); // consume `*`
                self.skip_trivia();
                (module_path, ImportKind::Wildcard)
            } else {
                // Not a wildcard — it's a dotted name continuation, restore
                self.pos = saved_pos;
                // Continue parsing the dotted name
                let mut full_name = module_path;
                while self.at(SyntaxKind::Dot) {
                    self.bump();
                    let part = self.expect(SyntaxKind::UpperIdent);
                    full_name.push('.');
                    full_name.push_str(self.text_of(&part));
                }
                self.skip_trivia();
                let kind = self.parse_import_suffix();
                (full_name, kind)
            }
        } else {
            let kind = self.parse_import_suffix();
            (module_path, kind)
        };

        // Check for optional postfix `when cfg.x`
        self.skip_trivia();
        let condition = if self.at(SyntaxKind::KwWhen) {
            self.bump(); // consume `when`
            self.skip_trivia();
            Some(self.parse_cfg_predicate())
        } else {
            None
        };

        let span = self.span_from(start);
        Decl::ImportDecl {
            module_path,
            kind,
            condition,
            span,
            comments: vec![],
        }
    }

    /// Parse the suffix of an import: `(names)`, `as Alias`, or nothing.
    pub(crate) fn parse_import_suffix(&mut self) -> ImportKind {
        if self.at(SyntaxKind::KwAs) {
            self.bump(); // consume `as`
            self.skip_trivia();
            let tok = self.expect(SyntaxKind::UpperIdent);
            let alias = self.text_of(&tok).to_owned();
            ImportKind::Qualified(alias)
        } else if self.at(SyntaxKind::LParen) {
            ImportKind::Selective(self.parse_name_list())
        } else {
            ImportKind::All
        }
    }

    /// Parse a dotted uppercase name like `Foo.Bar.Baz`.
    pub(crate) fn parse_dotted_upper_name(&mut self) -> String {
        let first = self.expect(SyntaxKind::UpperIdent);
        let mut name = self.text_of(&first).to_owned();
        while self.at(SyntaxKind::Dot) {
            self.bump(); // consume `.`
            let part = self.expect(SyntaxKind::UpperIdent);
            name.push('.');
            name.push_str(self.text_of(&part));
        }
        name
    }

    /// Parse a parenthesized list of names: `(foo, Bar, (+), ...)`.
    pub(crate) fn parse_name_list(&mut self) -> Vec<String> {
        self.expect(SyntaxKind::LParen);
        self.skip_trivia();
        let mut names = Vec::new();
        while !self.at(SyntaxKind::RParen) && !self.at_end() && self.consume_fuel() {
            let name = if self.at(SyntaxKind::Ident) || self.at(SyntaxKind::UpperIdent) {
                let tok = self.bump();
                self.text_of(&tok).to_owned()
            } else if self.at(SyntaxKind::LParen) {
                // Operator in parens: (+), (==), etc.
                self.bump(); // (
                self.skip_trivia();
                let mut op = String::new();
                while !self.at(SyntaxKind::RParen) && !self.at_end() {
                    let tok = self.bump();
                    op.push_str(self.text_of(&tok));
                }
                self.expect(SyntaxKind::RParen);
                format!("({})", op)
            } else {
                break;
            };
            names.push(name);
            self.skip_trivia();
            if self.at(SyntaxKind::Comma) {
                self.bump();
                self.skip_trivia();
            }
        }
        self.expect(SyntaxKind::RParen);
        names
    }

    /// Parse a `when` block for conditional compilation:
    /// ```text
    /// when cfg.debug
    ///   decl1
    ///   decl2
    /// else when cfg.mobile
    ///   decl3
    /// else
    ///   decl4
    /// ```
    pub(crate) fn parse_when_decl(&mut self) -> Decl {
        self.parse_when_decl_with_col(None)
    }

    pub(crate) fn parse_when_decl_with_col(&mut self, override_col: Option<u32>) -> Decl {
        let start = self.current_span().start;
        let when_col = override_col.unwrap_or_else(|| self.column_of(start));
        self.expect(SyntaxKind::KwWhen);
        self.skip_trivia();

        let condition = self.parse_cfg_predicate();
        self.skip_trivia();

        // Parse the then-branch: declarations indented further than `when`
        let then_decls = self.parse_when_body(when_col);

        // Check for `else` / `else when`
        self.skip_trivia();
        let else_decls = if self.at(SyntaxKind::KwElse)
            && self.column_of(self.current_span().start) == when_col
        {
            self.bump(); // consume `else`
            self.skip_trivia();
            if self.at(SyntaxKind::KwWhen) {
                // `else when` — parse recursively, inheriting the outer when_col
                // so that body indentation is measured against the original `when`
                vec![self.parse_when_decl_with_col(Some(when_col))]
            } else {
                // `else` block
                self.parse_when_body(when_col)
            }
        } else {
            vec![]
        };

        let span = self.span_from(start);
        Decl::CfgDecl {
            condition,
            then_decls,
            else_decls,
            span,
        }
    }

    /// Parse the body of a `when` or `else` block.
    /// Collects declarations that are indented further than `parent_col`.
    pub(crate) fn parse_when_body(&mut self, parent_col: u32) -> Vec<Decl> {
        let mut decls = Vec::new();
        loop {
            // Drain any buffered decls from group block expansion.
            if !self.pending_decls.is_empty() {
                decls.push(self.pending_decls.remove(0));
                continue;
            }

            self.skip_trivia();
            self.eat_layout_semi();
            self.skip_trivia();

            if self.at_end() || self.at(SyntaxKind::LayoutBraceClose) {
                break;
            }

            // Check if next non-trivia token is indented further than the parent
            let next_col = self.column_of(self.current_span().start);
            if next_col <= parent_col {
                break;
            }

            if let Some(decl) = self.parse_decl() {
                decls.push(decl);
            } else {
                if !self.at_end() {
                    self.bump();
                }
            }
        }
        decls
    }

    /// Parse a cfg predicate: `cfg.name`, `not cfg.name`, `pred && pred`, `pred || pred`.
    ///
    /// Precedence: `not` binds tightest, then `&&`, then `||`.
    pub(crate) fn parse_cfg_predicate(&mut self) -> CfgPredicate {
        self.parse_cfg_or()
    }

    /// Parse `||` (lowest precedence in cfg predicates).
    pub(crate) fn parse_cfg_or(&mut self) -> CfgPredicate {
        let mut left = self.parse_cfg_and();
        while self.at(SyntaxKind::OrOr) {
            self.bump(); // consume `||`
            self.skip_trivia();
            let right = self.parse_cfg_and();
            left = CfgPredicate::Or(Box::new(left), Box::new(right));
        }
        left
    }

    /// Parse `&&` (middle precedence in cfg predicates).
    pub(crate) fn parse_cfg_and(&mut self) -> CfgPredicate {
        let mut left = self.parse_cfg_atom();
        while self.at(SyntaxKind::AndAnd) {
            self.bump(); // consume `&&`
            self.skip_trivia();
            let right = self.parse_cfg_atom();
            left = CfgPredicate::And(Box::new(left), Box::new(right));
        }
        left
    }

    /// Parse a cfg atom: `not pred`, `cfg.name`, or `(pred)`.
    pub(crate) fn parse_cfg_atom(&mut self) -> CfgPredicate {
        // `not` prefix
        if self.at(SyntaxKind::Ident)
            && self.current_token().span.source_text(&self.source) == "not"
        {
            self.bump(); // consume `not`
            self.skip_trivia();
            let inner = self.parse_cfg_atom();
            return CfgPredicate::Not(Box::new(inner));
        }

        // `cfg.name`
        if self.at(SyntaxKind::KwCfg) {
            self.bump(); // consume `cfg`
            self.expect(SyntaxKind::Dot);
            let name_tok = self.expect(SyntaxKind::Ident);
            let name = self.text_of(&name_tok).to_owned();
            self.skip_trivia();
            return CfgPredicate::Feature(name);
        }

        // Parenthesized predicate
        if self.at(SyntaxKind::LParen) {
            self.bump(); // consume `(`
            self.skip_trivia();
            let inner = self.parse_cfg_predicate();
            self.skip_trivia();
            self.expect(SyntaxKind::RParen);
            self.skip_trivia();
            return inner;
        }

        // Error recovery: emit diagnostic and return a dummy predicate
        let span = self.current_span();
        self.diagnostics.push(
            Diagnostic::error("expected cfg predicate (e.g. `cfg.name`)")
                .with_label(Label::primary(span, "expected cfg predicate")),
        );
        CfgPredicate::Feature("__error__".to_string())
    }

    pub(crate) fn parse_data_decl(&mut self) -> Decl {
        let start = self.current_span().start;
        self.expect(SyntaxKind::KwData);
        self.skip_trivia();

        let name_tok = self.expect(SyntaxKind::UpperIdent);
        let name = self.text_of(&name_tok).to_owned();

        // Type parameters
        let mut type_params = Vec::new();
        self.skip_trivia();
        while self.at(SyntaxKind::Ident) {
            let p = self.bump();
            type_params.push(self.text_of(&p).to_owned());
            self.skip_trivia();
        }

        self.expect(SyntaxKind::Equals);
        self.skip_trivia();

        // Parse constructors separated by `|`
        let mut constructors = Vec::new();
        constructors.push(self.parse_con_decl());
        loop {
            self.skip_trivia();
            if self.eat(SyntaxKind::Pipe) {
                self.skip_trivia();
                constructors.push(self.parse_con_decl());
            } else {
                break;
            }
        }

        let span = self.span_from(start);
        Decl::DataDecl {
            name,
            type_params,
            constructors,
            span,
            comments: vec![],
        }
    }

    pub(crate) fn parse_con_decl(&mut self) -> ConDecl {
        let start = self.current_span().start;
        let name_tok = self.expect(SyntaxKind::UpperIdent);
        let name = self.text_of(&name_tok).to_owned();
        self.skip_trivia();

        // Optional explicit discriminant: `= <int>`
        let discriminant = if self.at(SyntaxKind::Equals) {
            // Peek ahead: if next non-trivia token after `=` is an IntLiteral,
            // treat as discriminant. Otherwise it could be a different production.
            let mut i = self.pos + 1;
            while i < self.tokens.len() && self.tokens[i].kind.is_trivia() {
                i += 1;
            }
            let next_is_int =
                i < self.tokens.len() && self.tokens[i].kind == SyntaxKind::IntLiteral;
            if next_is_int {
                self.bump(); // consume `=`
                self.skip_trivia();
                let int_tok = self.expect(SyntaxKind::IntLiteral);
                let val = parse_int_literal(self.text_of(&int_tok));
                self.skip_trivia();
                Some(val)
            } else {
                None
            }
        } else {
            None
        };

        // Check for record syntax `{`
        let fields = if self.at(SyntaxKind::LBrace) {
            self.bump();
            let mut flds = Vec::new();
            loop {
                self.skip_trivia();
                if self.at(SyntaxKind::RBrace) || self.at_end() {
                    break;
                }
                // Parse optional field attributes
                let mut field_attrs = Vec::new();
                while self.at(SyntaxKind::At) {
                    field_attrs.push(self.parse_attribute());
                    self.skip_trivia();
                }
                let field_name_tok = self.expect(SyntaxKind::Ident);
                let field_name = self.text_of(&field_name_tok).to_owned();
                self.skip_trivia();
                self.expect(SyntaxKind::Colon);
                self.skip_trivia();
                let ty = self.parse_type();
                flds.push(RecordField {
                    name: field_name,
                    ty,
                    attributes: field_attrs,
                    doc: None,
                });
                self.skip_trivia();
                if !self.eat(SyntaxKind::Comma) {
                    break;
                }
            }
            self.expect(SyntaxKind::RBrace);
            ConFields::Record(flds)
        } else {
            // Positional fields: type atoms until we see `|`, newline-level token, or EOF
            let mut tys = Vec::new();
            while !self.at_end() && self.consume_fuel() {
                let k = self.peek_non_trivia();
                if matches!(
                    k,
                    SyntaxKind::Pipe
                        | SyntaxKind::Eof
                        | SyntaxKind::LayoutSemicolon
                        | SyntaxKind::LayoutBraceClose
                        | SyntaxKind::KwData
                        | SyntaxKind::KwWhere
                        | SyntaxKind::Newline
                ) {
                    break;
                }
                // Only consume type atoms (Con / Var / Paren)
                if !matches!(
                    k,
                    SyntaxKind::UpperIdent | SyntaxKind::Ident | SyntaxKind::LParen
                ) {
                    break;
                }
                self.skip_trivia();
                let ty = self.parse_type_atom();
                tys.push(ty);
            }
            if tys.is_empty() {
                ConFields::Empty
            } else {
                ConFields::Positional(tys)
            }
        };

        let span = self.span_from(start);
        ConDecl {
            name,
            fields,
            discriminant,
            span,
            doc: None,
        }
    }

    pub(crate) fn parse_alias_decl(&mut self) -> Decl {
        let start = self.current_span().start;
        self.expect(SyntaxKind::KwAlias);
        self.skip_trivia();
        let name_tok = self.expect(SyntaxKind::UpperIdent);
        let name = self.text_of(&name_tok).to_owned();
        self.skip_trivia();
        self.expect(SyntaxKind::Equals);
        self.skip_trivia();
        let ty = self.parse_type();
        let span = self.span_from(start);
        Decl::TypeAlias {
            name,
            params: vec![],
            ty,
            span,
            comments: vec![],
        }
    }

    /// Peek ahead (without consuming tokens) to check if the current `@`
    /// starts a binding declaration (`@group(N) @binding(N) uniform/storage ...`).
    pub(crate) fn is_binding_decl_ahead(&self) -> bool {
        // We're positioned at `@`. Look ahead for `@` + `group`.
        let mut i = self.pos;
        // Skip trivia
        while i < self.tokens.len() && self.tokens[i].kind.is_trivia() {
            i += 1;
        }
        // Expect `@`
        if i >= self.tokens.len() || self.tokens[i].kind != SyntaxKind::At {
            return false;
        }
        i += 1;
        // Skip trivia
        while i < self.tokens.len() && self.tokens[i].kind.is_trivia() {
            i += 1;
        }
        // Check if the attribute name is `group`
        if i >= self.tokens.len() || self.tokens[i].kind != SyntaxKind::Ident {
            return false;
        }
        self.text_of(&self.tokens[i]) == ATTR_GROUP
    }

    /// Parse binding declaration(s) starting from `@group(N)`.
    ///
    /// Flat syntax (single binding):
    /// ```text
    /// @group(0) @binding(0) uniform name : Type
    /// ```
    ///
    /// Group block syntax (multiple bindings sharing a group):
    /// ```text
    /// @group(0)
    ///   @binding(0) uniform name1 : Type1
    ///   @binding(1) storage name2 : Type2
    /// ```
    /// Check whether there is a newline (or layout semicolon) between current
    /// position and the next non-trivia token. Used to distinguish flat
    /// `@group(N) @binding(N)` from group block syntax.
    pub(crate) fn has_newline_before_next_token(&self) -> bool {
        let mut i = self.pos;
        while i < self.tokens.len() {
            let kind = self.tokens[i].kind;
            if kind == SyntaxKind::Newline || kind == SyntaxKind::LayoutSemicolon {
                return true;
            }
            if !kind.is_trivia() {
                return false;
            }
            i += 1;
        }
        false
    }

    pub(crate) fn parse_binding_decls(&mut self, start: u32) -> Vec<Decl> {
        let group_col = self.column_of(self.current_span().start);
        let group = self.parse_binding_numbered_attr(ATTR_GROUP);

        // Determine flat vs group block: if there's a newline after @group(N)
        // before the next token, it's a group block.
        let is_group_block = self.has_newline_before_next_token();
        self.skip_trivia();

        if !is_group_block && self.is_binding_attr_ahead() {
            // Flat: @group(N) @binding(N) uniform/storage name : Type
            let binding = self.parse_binding_numbered_attr(ATTR_BINDING);
            self.skip_trivia();
            vec![self.parse_binding_body(start, group, binding)]
        } else {
            // Group block: collect indented @binding(...) declarations
            let mut decls = Vec::new();
            loop {
                self.skip_trivia();
                self.eat_layout_semi();
                self.skip_trivia();

                if self.at_end() || self.at(SyntaxKind::LayoutBraceClose) {
                    break;
                }

                let next_col = self.column_of(self.current_span().start);
                if next_col <= group_col {
                    break;
                }

                if !self.is_binding_attr_ahead() {
                    break;
                }

                let binding_start = self.current_span().start;
                let binding = self.parse_binding_numbered_attr(ATTR_BINDING);
                self.skip_trivia();
                decls.push(self.parse_binding_body(binding_start, group, binding));
            }
            decls
        }
    }

    /// Check if the next non-trivia tokens are `@binding` (without consuming).
    pub(crate) fn is_binding_attr_ahead(&self) -> bool {
        let mut i = self.pos;
        while i < self.tokens.len() && self.tokens[i].kind.is_trivia() {
            i += 1;
        }
        if i >= self.tokens.len() || self.tokens[i].kind != SyntaxKind::At {
            return false;
        }
        i += 1;
        while i < self.tokens.len() && self.tokens[i].kind.is_trivia() {
            i += 1;
        }
        i < self.tokens.len()
            && self.tokens[i].kind == SyntaxKind::Ident
            && self.text_of(&self.tokens[i]) == ATTR_BINDING
    }

    /// Parse `extern name : type` or `extern (+) : type`.
    /// Declares a built-in name with its type signature (no body).
    pub(crate) fn parse_extern_decl(&mut self) -> Decl {
        let start = self.current_span().start;
        self.expect(SyntaxKind::KwExtern);
        self.skip_trivia();

        let name = if self.at(SyntaxKind::LParen) {
            // Operator in parens: `(+)`, `(==)`, etc.
            self.bump(); // consume `(`
            self.skip_trivia();
            let op_tok = self.bump();
            let op = self.text_of(&op_tok).to_owned();
            self.skip_trivia();
            self.expect(SyntaxKind::RParen);
            op
        } else {
            let name_tok = self.expect(SyntaxKind::Ident);
            self.text_of(&name_tok).to_owned()
        };
        self.skip_trivia();
        self.expect(SyntaxKind::Colon);
        self.skip_trivia();
        let ty = self.parse_type();
        let span = self.span_from(start);
        Decl::ExternDecl {
            name,
            ty,
            span,
            comments: vec![],
        }
    }

    pub(crate) fn parse_builtin_decl(&mut self) -> Decl {
        let start = self.current_span().start;
        self.expect(SyntaxKind::KwBuiltin);
        self.skip_trivia();

        match self.peek_non_trivia() {
            SyntaxKind::KwExtern => self.parse_builtin_extern_decl(start),
            SyntaxKind::KwImpl => self.parse_builtin_impl_decl(start),
            SyntaxKind::KwType => self.parse_builtin_type_decl(start),
            _ => {
                self.diagnostics.push(
                    Diagnostic::error("expected `type`, `extern`, or `impl` after `builtin`")
                        .with_label(Label::primary(
                            self.current_span(),
                            "expected builtin declaration kind",
                        )),
                );
                Decl::ExternDecl {
                    name: "__error__".into(),
                    ty: Type::Con("__error__".into(), self.span_from(start)),
                    span: self.span_from(start),
                    comments: vec![],
                }
            }
        }
    }

    pub(crate) fn parse_builtin_type_decl(&mut self, start: u32) -> Decl {
        self.expect(SyntaxKind::KwType);
        self.skip_trivia();
        let name_tok = self.expect(SyntaxKind::UpperIdent);
        let name = self.text_of(&name_tok).to_owned();
        self.skip_trivia();
        let arity = if self.at(SyntaxKind::IntLiteral) {
            let arity_tok = self.bump();
            self.text_of(&arity_tok).parse::<usize>().unwrap_or(0)
        } else {
            0
        };
        let span = self.span_from(start);
        Decl::BuiltinTypeDecl {
            name,
            arity,
            span,
            comments: vec![],
        }
    }

    pub(crate) fn parse_builtin_extern_decl(&mut self, start: u32) -> Decl {
        self.expect(SyntaxKind::KwExtern);
        self.skip_trivia();
        let name = self.parse_value_decl_name();
        self.skip_trivia();
        self.expect(SyntaxKind::Colon);
        self.skip_trivia();
        let ty = self.parse_type();
        self.skip_trivia();
        self.expect(SyntaxKind::Equals);
        self.skip_trivia();
        let lowering = self.parse_builtin_lowering_spec();
        let span = self.span_from(start);
        Decl::BuiltinExternDecl {
            name,
            ty,
            lowering,
            span,
            comments: vec![],
        }
    }

    pub(crate) fn parse_builtin_impl_decl(&mut self, start: u32) -> Decl {
        self.expect(SyntaxKind::KwImpl);
        self.skip_trivia();
        let trait_name_tok = self.expect(SyntaxKind::UpperIdent);
        let trait_name = self.text_of(&trait_name_tok).to_owned();
        self.skip_trivia();

        let mut tys = Vec::new();
        while !self.at(SyntaxKind::KwWhere) && !self.at_end() && self.consume_fuel() {
            tys.push(self.parse_type_atom());
            self.skip_trivia();
        }

        self.expect(SyntaxKind::KwWhere);
        self.skip_trivia();
        self.eat(SyntaxKind::LayoutBraceOpen);
        self.skip_trivia();

        let mut associated_types = Vec::new();
        let mut methods = Vec::new();
        while !self.at_layout_end() && !self.at_end() && self.consume_fuel() {
            self.eat_layout_semi();
            self.skip_trivia();
            if self.at_layout_end() || self.at_end() {
                break;
            }

            if let Some(assoc_def) = self.try_parse_assoc_type_def() {
                associated_types.push(assoc_def);
                continue;
            }

            let mstart = self.current_span().start;
            let method_name = self.parse_method_name();
            self.skip_trivia();
            self.expect(SyntaxKind::Equals);
            self.skip_trivia();
            let lowering = self.parse_builtin_lowering_spec();
            let mspan = self.span_from(mstart);
            methods.push(BuiltinImplMethod {
                name: method_name,
                lowering,
                span: mspan,
                doc: None,
            });
            self.eat_layout_semi();
        }
        self.eat_layout_close();

        let span = self.span_from(start);
        Decl::BuiltinImplDecl {
            trait_name,
            tys,
            associated_types,
            methods,
            span,
            comments: vec![],
        }
    }

    pub(crate) fn parse_value_decl_name(&mut self) -> String {
        if self.at(SyntaxKind::LParen) {
            self.parse_parenthesized_operator_name()
        } else {
            let name_tok = self.expect(SyntaxKind::Ident);
            self.text_of(&name_tok).to_owned()
        }
    }

    pub(crate) fn parse_builtin_lowering_spec(&mut self) -> BuiltinLowering {
        let kind_tok = self.expect(SyntaxKind::Ident);
        let kind = self.text_of(&kind_tok).to_owned();
        self.skip_trivia();
        self.expect(SyntaxKind::LParen);
        self.skip_trivia();
        let arg = self.parse_builtin_lowering_arg();
        self.skip_trivia();
        self.expect(SyntaxKind::RParen);
        match kind.as_str() {
            "intrinsic" => BuiltinLowering::Intrinsic(arg),
            "native_binop" => BuiltinLowering::NativeBinOp(arg),
            "native_unary" => BuiltinLowering::NativeUnary(arg),
            _ => {
                self.diagnostics.push(
                    Diagnostic::error(format!("unknown builtin lowering kind `{}`", kind))
                        .with_label(Label::primary(kind_tok.span, "unknown lowering kind")),
                );
                BuiltinLowering::Intrinsic(arg)
            }
        }
    }

    /// Parse a single binding declaration after `@group(N) @binding(N)` have been consumed.
    /// Expects: `uniform name : Type` or `storage name : Type` or `storage(read_write) name : Type`
    pub(crate) fn parse_binding_body(&mut self, start: u32, group: u32, binding: u32) -> Decl {
        let address_space = if self.at(SyntaxKind::KwUniform) {
            self.bump();
            BindingAddressSpace::Uniform
        } else if self.at(SyntaxKind::KwStorage) {
            self.bump();
            self.skip_trivia();
            // Check for optional access mode: `storage(read_write)` or `storage(read)`
            if self.at(SyntaxKind::LParen) {
                self.bump();
                self.skip_trivia();
                let mode_tok = self.expect(SyntaxKind::Ident);
                let mode = self.text_of(&mode_tok).to_owned();
                self.skip_trivia();
                // Handle `read_write` which may be parsed as `read` `_` `write` or a single ident
                let space = if mode == "read_write" {
                    BindingAddressSpace::StorageReadWrite
                } else if mode == "read" {
                    // Check for `read_write` as two tokens: `read` then looking at next
                    self.skip_trivia();
                    BindingAddressSpace::StorageRead
                } else {
                    // Unknown mode, default to read
                    BindingAddressSpace::StorageRead
                };
                self.expect(SyntaxKind::RParen);
                space
            } else {
                // `storage` without parens — default is read (consistent with WGSL)
                BindingAddressSpace::StorageRead
            }
        } else if self.at(SyntaxKind::KwImmediate) {
            self.bump();
            BindingAddressSpace::Immediate
        } else {
            // Bare name after @group/@binding — opaque resource (texture/sampler)
            BindingAddressSpace::Opaque
        };
        self.skip_trivia();

        let name_tok = self.expect(SyntaxKind::Ident);
        let name = self.text_of(&name_tok).to_owned();
        self.skip_trivia();
        self.expect(SyntaxKind::Colon);
        self.skip_trivia();
        let ty = self.parse_type();

        let span = self.span_from(start);
        Decl::BindingDecl {
            name,
            ty,
            address_space,
            group,
            binding,
            span,
            comments: vec![],
        }
    }

    /// Parse `@name(N)` — a numbered binding attribute like `@group(0)` or `@binding(1)`.
    /// Consumes `@`, the identifier, `(`, integer literal, `)` and returns the integer value.
    pub(crate) fn parse_binding_numbered_attr(&mut self, expected: &str) -> u32 {
        let attr = self.parse_attribute();
        if attr.name != expected {
            self.diagnostics.push(
                Diagnostic::error(format!(
                    "expected '@{}(...)', found '@{}(...)'",
                    expected, attr.name
                ))
                .with_label(Label::primary(
                    attr.span,
                    format!("expected '@{}(...)'", expected),
                )),
            );
        }
        match attr.args.as_slice() {
            [AttrArg::Positional(AttrValue::UInt(v))] => *v as u32,
            [AttrArg::Positional(AttrValue::Int(v))] => (*v).max(0) as u32,
            _ => {
                self.diagnostics.push(
                    Diagnostic::error(format!(
                        "'@{}' expects a single unsigned integer argument",
                        expected
                    ))
                    .with_label(Label::primary(
                        attr.span,
                        format!("expected '@{}(N)'", expected),
                    )),
                );
                0
            }
        }
    }

    /// Parse `bitfield Name : U32 = { field1 : width, field2 : width, ... }`
    pub(crate) fn parse_bitfield_decl(&mut self) -> Decl {
        let start = self.current_span().start;
        self.expect(SyntaxKind::KwBitfield);
        self.skip_trivia();

        let name_tok = self.expect(SyntaxKind::UpperIdent);
        let name = self.text_of(&name_tok).to_owned();
        self.skip_trivia();

        // `: BaseType`
        self.expect(SyntaxKind::Colon);
        self.skip_trivia();
        let base_ty = self.parse_type_atom();
        self.skip_trivia();

        // `= ConstructorName`
        self.expect(SyntaxKind::Equals);
        self.skip_trivia();
        let constructor_tok = self.expect(SyntaxKind::UpperIdent);
        let constructor_name = self.text_of(&constructor_tok).to_owned();
        self.skip_trivia();

        // `{ field : kind, ... }`
        self.expect(SyntaxKind::LBrace);
        let mut fields = Vec::new();
        loop {
            self.skip_trivia();
            if self.at(SyntaxKind::RBrace) || self.at_end() {
                break;
            }
            let field_start = self.current_span().start;
            let field_name_tok = self.expect(SyntaxKind::Ident);
            let field_name = self.text_of(&field_name_tok).to_owned();
            self.skip_trivia();
            self.expect(SyntaxKind::Colon);
            self.skip_trivia();

            // Parse field kind:
            //   name : N              → Bare(N)
            //   name : Bool           → Bool
            //   name : Type : N       → Typed { ty, width }
            //   name : Type           → EnumInferred(type_name)
            let kind = if self.at(SyntaxKind::IntLiteral) {
                // Bare integer width
                let width_tok = self.bump();
                BitfieldFieldKind::Bare(parse_int_literal(self.text_of(&width_tok)).max(0) as u32)
            } else if self.at(SyntaxKind::UpperIdent) {
                let type_tok = self.bump();
                let type_name = self.text_of(&type_tok).to_owned();
                self.skip_trivia();
                if type_name == BITFIELD_BOOL {
                    // Bool — always 1 bit
                    BitfieldFieldKind::Bool
                } else if self.at(SyntaxKind::Colon) {
                    // Typed field with explicit width: `Type : N`
                    self.bump(); // consume ':'
                    self.skip_trivia();
                    let width_tok = self.expect(SyntaxKind::IntLiteral);
                    let width = parse_int_literal(self.text_of(&width_tok)).max(0) as u32;
                    BitfieldFieldKind::Typed {
                        ty: type_name,
                        width,
                    }
                } else {
                    // Enum-inferred width
                    BitfieldFieldKind::EnumInferred(type_name)
                }
            } else {
                // Fallback: expect an int literal (will error)
                let width_tok = self.expect(SyntaxKind::IntLiteral);
                BitfieldFieldKind::Bare(parse_int_literal(self.text_of(&width_tok)).max(0) as u32)
            };
            let field_span = self.span_from(field_start);
            fields.push(BitfieldField {
                name: field_name,
                kind,
                span: field_span,
                doc: None,
            });
            self.skip_trivia();
            if !self.eat(SyntaxKind::Comma) {
                break;
            }
        }
        self.expect(SyntaxKind::RBrace);

        let span = self.span_from(start);
        Decl::BitfieldDecl {
            name,
            base_ty,
            constructor_name,
            fields,
            span,
            comments: vec![],
        }
    }

    pub(crate) fn parse_const_decl(&mut self) -> Decl {
        let start = self.current_span().start;
        self.expect(SyntaxKind::KwConst);
        self.skip_trivia();
        // Const name: could be Ident or UpperIdent (SCREAMING_SNAKE_CASE)
        let name_tok = if self.at(SyntaxKind::UpperIdent) {
            self.bump()
        } else {
            self.expect(SyntaxKind::Ident)
        };
        let name = self.text_of(&name_tok).to_owned();
        self.skip_trivia();
        self.expect(SyntaxKind::Colon);
        self.skip_trivia();
        let ty = self.parse_type();
        self.skip_trivia();
        self.expect(SyntaxKind::Equals);
        self.skip_trivia();
        let value = self.parse_expr();
        let span = self.span_from(start);
        Decl::ConstDecl {
            name,
            ty,
            value,
            span,
            comments: vec![],
        }
    }

    /// Parse `trait Name a b c where method1 : Type ... methodN : Type`
    pub(crate) fn parse_trait_decl(&mut self) -> Decl {
        let start = self.current_span().start;
        self.expect(SyntaxKind::KwTrait);
        self.skip_trivia();

        let name_tok = self.expect(SyntaxKind::UpperIdent);
        let name = self.text_of(&name_tok).to_owned();
        self.skip_trivia();

        let mut vars = Vec::new();
        while self.at(SyntaxKind::Ident) {
            let tok = self.bump();
            vars.push(self.text_of(&tok).to_owned());
            self.skip_trivia();
        }

        self.expect(SyntaxKind::KwWhere);
        self.skip_trivia();

        // Parse associated type declarations and method signatures
        let mut associated_types = Vec::new();
        let mut methods = Vec::new();
        // Consume optional layout open brace
        self.eat(SyntaxKind::LayoutBraceOpen);
        self.skip_trivia();

        while !self.at_layout_end() && !self.at_end() && self.consume_fuel() {
            self.eat_layout_semi();
            self.skip_trivia();
            if self.at_layout_end() || self.at_end() {
                break;
            }

            if let Some(assoc_decl) = self.try_parse_assoc_type_decl() {
                associated_types.push(assoc_decl);
                continue;
            }

            let mstart = self.current_span().start;
            let method_name = self.parse_method_name();
            self.skip_trivia();
            self.expect(SyntaxKind::Colon);
            self.skip_trivia();
            let ty = self.parse_type();
            let mspan = self.span_from(mstart);
            methods.push(TraitMethod {
                name: method_name,
                ty,
                span: mspan,
                doc: None,
            });
            self.eat_layout_semi();
        }
        self.eat_layout_close();

        let span = self.span_from(start);
        Decl::TraitDecl {
            name,
            vars,
            associated_types,
            methods,
            span,
            comments: vec![],
        }
    }

    /// Parse `impl TraitName Type where ...` or `impl Type where ...`
    pub(crate) fn parse_impl_decl(&mut self) -> Decl {
        let start = self.current_span().start;
        self.expect(SyntaxKind::KwImpl);
        self.skip_trivia();

        // Parse the first type — could be trait name or the target type.
        let first_ty = self.parse_type_atom();
        self.skip_trivia();

        // If `where` follows immediately, this is a standalone impl (no trait).
        // Otherwise, the first type was the trait name and we parse a second type.
        let (trait_name, tys) = if self.at(SyntaxKind::KwWhere) {
            (None, vec![first_ty])
        } else {
            // first_ty should be a simple Con (the trait name)
            let tname = match &first_ty {
                Type::Con(name, _) => name.clone(),
                _ => {
                    let span = first_ty.span();
                    self.diagnostics.push(
                        Diagnostic::error("expected trait name".to_string())
                            .with_label(Label::primary(span, "expected uppercase identifier")),
                    );
                    "_unknown_".to_string()
                }
            };
            let mut tys = Vec::new();
            while !self.at(SyntaxKind::KwWhere) && !self.at_end() && self.consume_fuel() {
                tys.push(self.parse_type_atom());
                self.skip_trivia();
            }
            (Some(tname), tys)
        };

        self.expect(SyntaxKind::KwWhere);
        self.skip_trivia();

        // Parse associated type definitions and method implementations
        let mut associated_types = Vec::new();
        let mut methods = Vec::new();
        let mut method_sigs = HashMap::new();
        self.eat(SyntaxKind::LayoutBraceOpen);
        self.skip_trivia();

        while !self.at_layout_end() && !self.at_end() && self.consume_fuel() {
            self.eat_layout_semi();
            self.skip_trivia();
            if self.at_layout_end() || self.at_end() {
                break;
            }

            if let Some(assoc_def) = self.try_parse_assoc_type_def() {
                associated_types.push(assoc_def);
                continue;
            }

            let mstart = self.current_span().start;
            let method_name = self.parse_method_name();
            self.skip_trivia();

            if self.at(SyntaxKind::Colon) {
                self.bump();
                self.skip_trivia();
                let ty = self.parse_type();
                method_sigs.insert(method_name, ty);
                self.eat_layout_semi();
                continue;
            }

            // Parse params before `=`
            let mut params = Vec::new();
            while !self.at(SyntaxKind::Equals) && !self.at_end() && self.consume_fuel() {
                if matches!(
                    self.peek_non_trivia(),
                    SyntaxKind::LayoutSemicolon | SyntaxKind::LayoutBraceClose
                ) {
                    break;
                }
                params.push(self.parse_pat_atom());
                self.skip_trivia();
            }

            self.expect(SyntaxKind::Equals);
            self.skip_trivia();
            let body = self.parse_expr();
            let mspan = self.span_from(mstart);
            let method_ty = method_sigs.remove(&method_name);
            methods.push(ImplMethod {
                name: method_name,
                ty: method_ty,
                params,
                body,
                span: mspan,
            });
            self.eat_layout_semi();
        }
        self.eat_layout_close();

        for (name, _) in method_sigs {
            self.diagnostics.push(
                Diagnostic::error(format!(
                    "type signature for impl method '{}' has no definition",
                    name
                ))
                .with_help("add a matching method definition in the same impl block"),
            );
        }

        let span = self.span_from(start);
        Decl::ImplDecl {
            trait_name,
            tys,
            associated_types,
            methods,
            span,
            comments: vec![],
        }
    }

    pub(crate) fn parse_method_name(&mut self) -> String {
        if self.at(SyntaxKind::LParen) {
            self.parse_parenthesized_operator_name()
        } else {
            let tok = self.expect(SyntaxKind::Ident);
            self.text_of(&tok).to_owned()
        }
    }

    pub(crate) fn parse_parenthesized_operator_name(&mut self) -> String {
        self.expect(SyntaxKind::LParen);
        self.skip_trivia();
        let name = self.parse_operator_token_text();
        self.skip_trivia();
        self.expect(SyntaxKind::RParen);
        name
    }

    pub(crate) fn parse_builtin_lowering_arg(&mut self) -> String {
        match self.peek_non_trivia() {
            SyntaxKind::Ident => {
                self.skip_trivia();
                let tok = self.bump();
                self.text_of(&tok).to_owned()
            }
            _ => self.parse_operator_token_text(),
        }
    }

    pub(crate) fn parse_operator_token_text(&mut self) -> String {
        self.skip_trivia();
        let first = self.bump();
        if first.kind == SyntaxKind::Greater && self.peek_non_trivia() == SyntaxKind::Greater {
            self.skip_trivia();
            let second = self.bump();
            return format!("{}{}", self.text_of(&first), self.text_of(&second));
        }
        self.text_of(&first).to_owned()
    }

    pub(crate) fn parse_attribute(&mut self) -> Attribute {
        let start = self.current_span().start;
        self.expect(SyntaxKind::At);
        self.skip_trivia();

        // Attribute name: could be Ident, UpperIdent, or a reserved keyword
        // that is valid in attribute position such as `builtin` or `const`.
        let name_tok = if self.at(SyntaxKind::Ident)
            || self.at(SyntaxKind::KwBuiltin)
            || self.at(SyntaxKind::KwConst)
        {
            self.bump()
        } else {
            self.expect(SyntaxKind::UpperIdent)
        };
        let name = self.text_of(&name_tok).to_owned();

        // Optional arguments: parenthesized `@name(a, b)`.
        let mut args = Vec::new();
        self.skip_trivia();
        if self.at(SyntaxKind::LParen) {
            self.bump();
            loop {
                self.skip_trivia();
                if self.at(SyntaxKind::RParen) || self.at_end() {
                    break;
                }

                // Try to parse a named argument: `ident = value`
                let is_named = if self.at(SyntaxKind::Ident) {
                    let saved = self.pos;
                    self.bump();
                    self.skip_trivia();
                    let result = self.at(SyntaxKind::Equals);
                    self.pos = saved;
                    result
                } else {
                    false
                };

                if is_named {
                    let name_tok = self.bump();
                    let arg_name = self.text_of(&name_tok).to_owned();
                    self.skip_trivia();
                    self.expect(SyntaxKind::Equals);
                    self.skip_trivia();
                    let value = self.parse_attr_value();
                    args.push(AttrArg::Named(arg_name, value));
                } else {
                    let value = self.parse_attr_value();
                    args.push(AttrArg::Positional(value));
                }

                self.skip_trivia();
                if !self.eat(SyntaxKind::Comma) {
                    break;
                }
            }
            self.expect(SyntaxKind::RParen);
        }

        let span = self.span_from(start);
        Attribute { name, args, span }
    }

    /// Parse a single attribute value: identifier, string literal, int literal, or float literal.
    fn parse_attr_value(&mut self) -> AttrValue {
        match self.peek_non_trivia() {
            SyntaxKind::Ident => {
                let tok = self.bump();
                AttrValue::Ident(self.text_of(&tok).to_owned())
            }
            SyntaxKind::StringLiteral => {
                let tok = self.bump();
                let text = self.text_of(&tok);
                let inner = &text[1..text.len() - 1];
                AttrValue::String(unescape_string(inner))
            }
            SyntaxKind::IntLiteral => {
                let tok = self.bump();
                let text = self.text_of(&tok);
                match parse_int_literal_typed(text) {
                    ParsedInt::Unsigned(v) => AttrValue::UInt(v),
                    ParsedInt::Signed(v) => AttrValue::Int(v),
                }
            }
            SyntaxKind::FloatLiteral => {
                let tok = self.bump();
                let text = self.text_of(&tok);
                let val: f64 = text.parse().unwrap_or(0.0);
                AttrValue::Float(val)
            }
            _ => {
                let tok = self.bump();
                AttrValue::Ident(self.text_of(&tok).to_owned())
            }
        }
    }

    // ═════════════════════════════════════════════════════════════════════
    // Expressions (Pratt / precedence climbing)
    // ═════════════════════════════════════════════════════════════════════
}
