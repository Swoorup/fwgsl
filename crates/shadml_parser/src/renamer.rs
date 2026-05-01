use std::collections::{HashMap, HashSet};

use shadml_diagnostics::{Diagnostic, Label};
use shadml_span::Span;

use crate::module_merge::exported_names;
use crate::module_resolver::{ModuleGraph, ParsedModule};
use crate::parser::*;

/// Resolve all module-qualified and unqualified imported names into
/// `Expr::Resolved` / `Type::Resolved` nodes.
///
/// The Renamer consumes the `ModuleGraph` and produces a flat `Program` where
/// every cross-module reference has been resolved to its original module and
/// name.  Local bindings and declarations are left untouched.
pub struct Renamer<'a> {
    module_graph: &'a ModuleGraph,
    /// module_name → exported names (precomputed).
    exports: HashMap<String, Vec<String>>,
    /// Diagnostics collected during renaming.
    diagnostics: Vec<Diagnostic>,
}

/// Mutable per-module renaming context.
struct RenameCtx<'a> {
    renamer: &'a Renamer<'a>,
    #[allow(dead_code)]
    current_module: String,
    /// Stack of scope frames.  Each frame holds locally-defined names.
    local_scope: Vec<HashSet<String>>,
    /// Unqualified imports: name → source module path.
    unqualified_names: HashMap<String, String>,
    /// Qualified imports / module aliases: alias → real module path.
    module_aliases: HashMap<String, String>,
    /// Accumulated output declarations.
    output_decls: Vec<Decl>,
    /// Diagnostics collected during renaming.
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Renamer<'a> {
    pub fn new(module_graph: &'a ModuleGraph) -> Self {
        let mut exports = HashMap::new();
        for module in &module_graph.modules {
            let names: Vec<String> = module
                .program
                .decls
                .iter()
                .flat_map(exported_names)
                .collect();
            exports.insert(module.name.clone(), names);
        }
        Self {
            module_graph,
            exports,
            diagnostics: Vec::new(),
        }
    }

    pub fn run(&mut self) -> Program {
        let mut output_decls = Vec::new();
        for module in &self.module_graph.modules {
            let mut ctx = RenameCtx::new(self, module.name.clone());
            ctx.process_module(module);
            output_decls.extend(ctx.output_decls);
            self.diagnostics.extend(ctx.diagnostics);
        }
        Program {
            decls: output_decls,
        }
    }

    /// Return any diagnostics collected during renaming.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

impl<'a> RenameCtx<'a> {
    fn new(renamer: &'a Renamer<'a>, current_module: String) -> Self {
        Self {
            renamer,
            current_module,
            local_scope: Vec::new(),
            unqualified_names: HashMap::new(),
            module_aliases: HashMap::new(),
            output_decls: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    // ------------------------------------------------------------------
    // Scope helpers
    // ------------------------------------------------------------------

    fn push_scope(&mut self) {
        self.local_scope.push(HashSet::new());
    }

    fn pop_scope(&mut self) {
        self.local_scope.pop();
    }

    fn define_local(&mut self, name: &str) {
        if let Some(frame) = self.local_scope.last_mut() {
            frame.insert(name.to_owned());
        }
    }

    fn is_local(&self, name: &str) -> bool {
        self.local_scope.iter().any(|f| f.contains(name))
    }

    // ------------------------------------------------------------------
    // Module-level processing
    // ------------------------------------------------------------------

    fn process_module(&mut self, module: &ParsedModule) {
        // 1. Build import tables from ImportDecls.
        for decl in &module.program.decls {
            if let Decl::ImportDecl {
                module_path,
                kind,
                span,
                ..
            } = decl
            {
                self.register_import(module_path, kind, *span);
            }
        }

        // 2. Collect all top-level names defined in this module into a
        //    single scope frame.  This makes mutually recursive top-level
        //    declarations visible to each other.
        self.push_scope();
        for decl in &module.program.decls {
            self.collect_decl_names(decl);
        }

        // 3. Rename each declaration and accumulate output.
        for decl in &module.program.decls {
            if matches!(decl, Decl::ModuleDecl { .. } | Decl::ImportDecl { .. }) {
                continue;
            }
            let renamed = self.rename_decl(decl);
            self.output_decls.push(renamed);
        }

        self.pop_scope();
    }

    // ------------------------------------------------------------------
    // Import registration
    // ------------------------------------------------------------------

    fn register_import(&mut self, module_path: &str, kind: &ImportKind, span: Span) {
        // For non-wildcard imports, validate that the module path exists in the graph.
        let module_exists = self
            .renamer
            .module_graph
            .modules
            .iter()
            .any(|m| m.name == module_path);
        let exports = self.renamer.exports.get(module_path).cloned();

        match kind {
            ImportKind::Wildcard => {
                // `import Foo.*` — each submodule `Foo.Bar` adds alias `Foo.Bar`.
                for module in &self.renamer.module_graph.modules {
                    if module.name.starts_with(&format!("{}.", module_path)) {
                        self.module_aliases
                            .insert(module.name.clone(), module.name.clone());
                    }
                }
            }
            _ if !module_exists => {
                self.diagnostics.push(
                    Diagnostic::error(format!("Unknown module: '{}'", module_path))
                        .with_label(Label::primary(span, "no such module"))
                        .with_help("check the module name against the source_roots or file layout"),
                );
            }
            ImportKind::All => {
                // `import Foo` — every public name from Foo is available unqualified.
                if let Some(names) = exports {
                    for name in names {
                        self.unqualified_names.insert(name, module_path.to_owned());
                    }
                }
            }
            ImportKind::Selective(names) => {
                // `import Foo (bar, baz)` — only listed names.
                let available = exports.unwrap_or_default();
                for name in names {
                    if !available.contains(name) {
                        self.diagnostics.push(
                            Diagnostic::error(format!(
                                "Module '{}' does not export '{}'",
                                module_path, name
                            ))
                            .with_label(Label::primary(span, "unknown export"))
                            .with_help("check the name against the module's public declarations"),
                        );
                    }
                    self.unqualified_names
                        .insert(name.clone(), module_path.to_owned());
                }
            }
            ImportKind::Qualified(alias) => {
                // `import Foo as F` — module alias only.
                self.module_aliases
                    .insert(alias.clone(), module_path.to_owned());
            }
        }
    }

    // ------------------------------------------------------------------
    // Name collection (for top-level scope)
    // ------------------------------------------------------------------

    fn collect_decl_names(&mut self, decl: &Decl) {
        match decl {
            Decl::FunDecl { name, params, .. } => {
                self.define_local(name);
                for pat in params {
                    self.collect_pat_vars(pat);
                }
            }
            Decl::DataDecl {
                name, constructors, ..
            } => {
                self.define_local(name);
                for con in constructors {
                    self.define_local(&con.name);
                }
            }
            Decl::TypeAlias { name, .. }
            | Decl::BindingDecl { name, .. }
            | Decl::EntryPoint { name, .. }
            | Decl::ExternDecl { name, .. }
            | Decl::BuiltinExternDecl { name, .. }
            | Decl::RenderBlock { name, .. }
            | Decl::ConstDecl { name, .. }
            | Decl::BitfieldDecl { name, .. } => {
                self.define_local(name);
            }
            Decl::TraitDecl { name, methods, .. } => {
                self.define_local(name);
                for m in methods {
                    self.define_local(&m.name);
                }
            }
            Decl::ImplDecl { methods, .. } => {
                for m in methods {
                    self.define_local(&m.name);
                }
            }
            Decl::BuiltinImplDecl { methods, .. } => {
                for m in methods {
                    self.define_local(&m.name);
                }
            }
            Decl::TypeSig { name, .. } => {
                // Type signatures don't define names, but we add them so
                // recursive references work.
                self.define_local(name);
            }
            Decl::CfgDecl {
                then_decls,
                else_decls,
                ..
            } => {
                for d in then_decls {
                    self.collect_decl_names(d);
                }
                for d in else_decls {
                    self.collect_decl_names(d);
                }
            }
            Decl::ModuleDecl { .. } | Decl::ImportDecl { .. } | Decl::BuiltinTypeDecl { .. } => {}
        }
    }

    fn collect_pat_vars(&mut self, pat: &Pat) {
        match pat {
            Pat::Var(name, _) | Pat::As(name, _, _) => {
                self.define_local(name);
            }
            Pat::Con(_, sub_pats, _) => {
                for p in sub_pats {
                    self.collect_pat_vars(p);
                }
            }
            Pat::Tuple(sub_pats, _) => {
                for p in sub_pats {
                    self.collect_pat_vars(p);
                }
            }
            Pat::Record(_, fields, _, _) => {
                for (_, maybe_pat) in fields {
                    if let Some(p) = maybe_pat {
                        self.collect_pat_vars(p);
                    }
                }
            }
            Pat::Or(alts, _) => {
                // All alternatives bind the same variables; just use the first.
                if let Some(first) = alts.first() {
                    self.collect_pat_vars(first);
                }
            }
            Pat::Paren(inner, _) => self.collect_pat_vars(inner),
            Pat::Wild(_) | Pat::Lit(_, _) => {}
        }
    }

    // ------------------------------------------------------------------
    // Declaration renaming
    // ------------------------------------------------------------------

    fn rename_decl(&mut self, decl: &Decl) -> Decl {
        match decl.clone() {
            Decl::TypeSig {
                name,
                constraints,
                ty,
                span,
                comments,
            } => Decl::TypeSig {
                name,
                constraints: constraints
                    .into_iter()
                    .map(|c| self.rename_type_constraint(&c))
                    .collect(),
                ty: self.rename_type(&ty),
                span,
                comments,
            },
            Decl::FunDecl {
                name,
                params,
                body,
                where_binds,
                span,
                comments,
                attributes,
            } => {
                // Parameters enter a new scope for the body + where clause.
                self.push_scope();
                for pat in &params {
                    self.collect_pat_vars(pat);
                }
                // Where bindings are mutually recursive with the body.
                for bind in &where_binds {
                    self.define_local(&bind.name);
                }
                let renamed_body = self.rename_expr(&body);
                let renamed_where: Vec<LocalBind> = where_binds
                    .into_iter()
                    .map(|b| LocalBind {
                        name: b.name,
                        name_span: b.name_span,
                        expr: self.rename_expr(&b.expr),
                        span: b.span,
                    })
                    .collect();
                self.pop_scope();
                Decl::FunDecl {
                    name,
                    params,
                    body: renamed_body,
                    where_binds: renamed_where,
                    span,
                    comments,
                    attributes,
                }
            }
            Decl::DataDecl {
                name,
                type_params,
                constructors,
                span,
                comments,
            } => Decl::DataDecl {
                name,
                type_params,
                constructors: constructors
                    .into_iter()
                    .map(|c| ConDecl {
                        name: c.name,
                        fields: match c.fields {
                            ConFields::Positional(tys) => ConFields::Positional(
                                tys.into_iter().map(|t| self.rename_type(&t)).collect(),
                            ),
                            ConFields::Record(fields) => ConFields::Record(
                                fields
                                    .into_iter()
                                    .map(|f| RecordField {
                                        name: f.name,
                                        ty: self.rename_type(&f.ty),
                                        attributes: f.attributes,
                                        doc: f.doc,
                                    })
                                    .collect(),
                            ),
                            ConFields::Empty => ConFields::Empty,
                        },
                        discriminant: c.discriminant,
                        span: c.span,
                        doc: c.doc,
                    })
                    .collect(),
                span,
                comments,
            },
            Decl::EntryPoint {
                attributes,
                name,
                params,
                body,
                span,
                comments,
            } => {
                self.push_scope();
                for pat in &params {
                    self.collect_pat_vars(pat);
                }
                let renamed_body = self.rename_expr(&body);
                self.pop_scope();
                Decl::EntryPoint {
                    attributes,
                    name,
                    params,
                    body: renamed_body,
                    span,
                    comments,
                }
            }
            Decl::TypeAlias {
                name,
                params,
                ty,
                span,
                comments,
            } => Decl::TypeAlias {
                name,
                params,
                ty: self.rename_type(&ty),
                span,
                comments,
            },
            Decl::BindingDecl {
                name,
                ty,
                address_space,
                group,
                binding,
                span,
                comments,
                attributes,
            } => Decl::BindingDecl {
                name,
                ty: self.rename_type(&ty),
                address_space,
                group,
                binding,
                span,
                comments,
                attributes,
            },
            Decl::BitfieldDecl {
                name,
                base_ty,
                constructor_name,
                fields,
                span,
                comments,
            } => Decl::BitfieldDecl {
                name,
                base_ty: self.rename_type(&base_ty),
                constructor_name,
                fields,
                span,
                comments,
            },
            Decl::ConstDecl {
                name,
                ty,
                value,
                span,
                comments,
            } => Decl::ConstDecl {
                name,
                ty: self.rename_type(&ty),
                value: self.rename_expr(&value),
                span,
                comments,
            },
            Decl::TraitDecl {
                name,
                vars,
                associated_types,
                methods,
                span,
                comments,
            } => Decl::TraitDecl {
                name,
                vars,
                associated_types,
                methods: methods
                    .into_iter()
                    .map(|m| TraitMethod {
                        name: m.name,
                        ty: self.rename_type(&m.ty),
                        span: m.span,
                        doc: m.doc,
                    })
                    .collect(),
                span,
                comments,
            },
            Decl::ImplDecl {
                trait_name,
                tys,
                associated_types,
                methods,
                span,
                comments,
            } => Decl::ImplDecl {
                trait_name,
                tys: tys.into_iter().map(|t| self.rename_type(&t)).collect(),
                associated_types: associated_types
                    .into_iter()
                    .map(|a| AssociatedTypeDef {
                        name: a.name,
                        ty: self.rename_type(&a.ty),
                        span: a.span,
                    })
                    .collect(),
                methods: methods
                    .into_iter()
                    .map(|m| ImplMethod {
                        name: m.name,
                        ty: m.ty.map(|t| self.rename_type(&t)),
                        params: m.params,
                        body: self.rename_expr(&m.body),
                        span: m.span,
                    })
                    .collect(),
                span,
                comments,
            },
            Decl::ExternDecl {
                name,
                ty,
                span,
                comments,
            } => Decl::ExternDecl {
                name,
                ty: self.rename_type(&ty),
                span,
                comments,
            },
            Decl::BuiltinExternDecl {
                name,
                ty,
                lowering,
                span,
                comments,
            } => Decl::BuiltinExternDecl {
                name,
                ty: self.rename_type(&ty),
                lowering,
                span,
                comments,
            },
            Decl::BuiltinImplDecl {
                trait_name,
                tys,
                associated_types,
                methods,
                span,
                comments,
            } => Decl::BuiltinImplDecl {
                trait_name,
                tys: tys.into_iter().map(|t| self.rename_type(&t)).collect(),
                associated_types: associated_types
                    .into_iter()
                    .map(|a| AssociatedTypeDef {
                        name: a.name,
                        ty: self.rename_type(&a.ty),
                        span: a.span,
                    })
                    .collect(),
                methods,
                span,
                comments,
            },
            Decl::RenderBlock {
                name,
                bindings,
                entries,
                span,
                comments,
            } => Decl::RenderBlock {
                name,
                bindings: bindings.into_iter().map(|b| self.rename_decl(&b)).collect(),
                entries: entries.into_iter().map(|e| self.rename_decl(&e)).collect(),
                span,
                comments,
            },
            Decl::CfgDecl {
                condition,
                then_decls,
                else_decls,
                span,
            } => Decl::CfgDecl {
                condition,
                then_decls: then_decls
                    .into_iter()
                    .map(|d| self.rename_decl(&d))
                    .collect(),
                else_decls: else_decls
                    .into_iter()
                    .map(|d| self.rename_decl(&d))
                    .collect(),
                span,
            },
            // Module and import declarations are stripped from output.
            Decl::ModuleDecl { .. } | Decl::ImportDecl { .. } => decl.clone(),
            Decl::BuiltinTypeDecl { .. } => decl.clone(),
        }
    }

    fn rename_type_constraint(&mut self, tc: &TypeConstraint) -> TypeConstraint {
        TypeConstraint {
            trait_name: tc.trait_name.clone(),
            tys: tc.tys.iter().map(|t| self.rename_type(t)).collect(),
            span: tc.span,
        }
    }

    // ------------------------------------------------------------------
    // Expression renaming
    // ------------------------------------------------------------------

    fn rename_expr(&mut self, expr: &Expr) -> Expr {
        match expr {
            // Wildcard import: `Foo.Bar.baz` parses as
            // `FieldAccess(Qualified("Foo", "Bar"), "baz")`.
            // If `Foo.Bar` is a module alias, rewrite the whole thing.
            Expr::FieldAccess(inner, field, span) => {
                if let Expr::Qualified(alias, sub, _) = inner.as_ref() {
                    let full_alias = format!("{}.{}", alias, sub);
                    if let Some(module) = self.module_aliases.get(&full_alias) {
                        return Expr::Resolved(
                            ResolvedName {
                                module: module.clone(),
                                original_name: field.clone(),
                                stamp: 0,
                            },
                            *span,
                        );
                    }
                }
                Expr::FieldAccess(Box::new(self.rename_expr(inner)), field.clone(), *span)
            }
            Expr::Var(name, span) => {
                if self.is_local(name) {
                    Expr::Var(name.clone(), *span)
                } else if let Some(module) = self.unqualified_names.get(name) {
                    Expr::Resolved(
                        ResolvedName {
                            module: module.clone(),
                            original_name: name.clone(),
                            stamp: 0,
                        },
                        *span,
                    )
                } else {
                    // Unresolved — leave as-is for the semantic analyzer to report.
                    Expr::Var(name.clone(), *span)
                }
            }
            Expr::Qualified(alias, name, span) => {
                if let Some(module) = self.module_aliases.get(alias) {
                    Expr::Resolved(
                        ResolvedName {
                            module: module.clone(),
                            original_name: name.clone(),
                            stamp: 0,
                        },
                        *span,
                    )
                } else if let Some(module) = self.module_aliases.get(&format!("{}.{}", alias, name))
                {
                    // Wildcard import: `Foo.Bar` where the alias is the full path.
                    Expr::Resolved(
                        ResolvedName {
                            module: module.clone(),
                            original_name: name.clone(),
                            stamp: 0,
                        },
                        *span,
                    )
                } else {
                    Expr::Qualified(alias.clone(), name.clone(), *span)
                }
            }
            Expr::Resolved(_, _) => expr.clone(),
            Expr::Lit(_, _) | Expr::Con(_, _) | Expr::OpSection(_, _) => expr.clone(),
            Expr::App(func, arg, span) => Expr::App(
                Box::new(self.rename_expr(func)),
                Box::new(self.rename_expr(arg)),
                *span,
            ),
            Expr::Infix(lhs, op, rhs, span) => Expr::Infix(
                Box::new(self.rename_expr(lhs)),
                op.clone(),
                Box::new(self.rename_expr(rhs)),
                *span,
            ),
            Expr::Lambda(params, body, span) => {
                self.push_scope();
                for pat in params {
                    self.collect_pat_vars(pat);
                }
                let renamed = Expr::Lambda(params.clone(), Box::new(self.rename_expr(body)), *span);
                self.pop_scope();
                renamed
            }
            Expr::Let(binds, body, span) => {
                self.push_scope();
                for bind in binds {
                    self.define_local(&bind.name);
                }
                let renamed_binds: Vec<LocalBind> = binds
                    .iter()
                    .map(|b| LocalBind {
                        name: b.name.clone(),
                        name_span: b.name_span,
                        expr: self.rename_expr(&b.expr),
                        span: b.span,
                    })
                    .collect();
                let renamed_body = self.rename_expr(body);
                self.pop_scope();
                Expr::Let(renamed_binds, Box::new(renamed_body), *span)
            }
            Expr::Case(scrut, arms, span) => {
                let renamed_scrut = self.rename_expr(scrut);
                let renamed_arms: Vec<(Pat, Option<Expr>, Expr)> = arms
                    .iter()
                    .map(|(pat, guard, body)| {
                        self.push_scope();
                        self.collect_pat_vars(pat);
                        let renamed_guard = guard.as_ref().map(|g| self.rename_expr(g));
                        let renamed_body = self.rename_expr(body);
                        self.pop_scope();
                        (pat.clone(), renamed_guard, renamed_body)
                    })
                    .collect();
                Expr::Case(Box::new(renamed_scrut), renamed_arms, *span)
            }
            Expr::If(cond, then_, else_, span) => Expr::If(
                Box::new(self.rename_expr(cond)),
                Box::new(self.rename_expr(then_)),
                Box::new(self.rename_expr(else_)),
                *span,
            ),
            Expr::Paren(inner, span) => Expr::Paren(Box::new(self.rename_expr(inner)), *span),
            Expr::Tuple(items, span) => {
                Expr::Tuple(items.iter().map(|e| self.rename_expr(e)).collect(), *span)
            }
            Expr::Record(name, fields, span) => Expr::Record(
                name.clone(),
                fields
                    .iter()
                    .map(|(n, e)| (n.clone(), self.rename_expr(e)))
                    .collect(),
                *span,
            ),
            Expr::Index(base, index, span) => Expr::Index(
                Box::new(self.rename_expr(base)),
                Box::new(self.rename_expr(index)),
                *span,
            ),
            Expr::Neg(inner, span) => Expr::Neg(Box::new(self.rename_expr(inner)), *span),
            Expr::Not(inner, span) => Expr::Not(Box::new(self.rename_expr(inner)), *span),
            Expr::BitNot(inner, span) => Expr::BitNot(Box::new(self.rename_expr(inner)), *span),
            Expr::Do(stmts, span) => {
                self.push_scope();
                let mut renamed = Vec::new();
                for stmt in stmts {
                    match stmt {
                        DoStmt::Bind(bind) | DoStmt::Let(bind) => {
                            self.define_local(&bind.name);
                            renamed.push(DoStmt::Bind(LocalBind {
                                name: bind.name.clone(),
                                name_span: bind.name_span,
                                expr: self.rename_expr(&bind.expr),
                                span: bind.span,
                            }));
                        }
                        DoStmt::Expr(e, s) => renamed.push(DoStmt::Expr(self.rename_expr(e), *s)),
                    }
                }
                self.pop_scope();
                Expr::Do(renamed, *span)
            }
            Expr::VecLit(items, span) => {
                Expr::VecLit(items.iter().map(|e| self.rename_expr(e)).collect(), *span)
            }
            Expr::Loop(name, bindings, body, span) => {
                self.push_scope();
                self.define_local(name);
                for bind in bindings {
                    self.define_local(&bind.name);
                }
                let renamed_bindings: Vec<LocalBind> = bindings
                    .iter()
                    .map(|b| LocalBind {
                        name: b.name.clone(),
                        name_span: b.name_span,
                        expr: self.rename_expr(&b.expr),
                        span: b.span,
                    })
                    .collect();
                let renamed_body = self.rename_expr(body);
                self.pop_scope();
                Expr::Loop(
                    name.clone(),
                    renamed_bindings,
                    Box::new(renamed_body),
                    *span,
                )
            }
            Expr::RecordUpdate(base, fields, span) => Expr::RecordUpdate(
                Box::new(self.rename_expr(base)),
                fields
                    .iter()
                    .map(|(n, e)| (n.clone(), self.rename_expr(e)))
                    .collect(),
                *span,
            ),
        }
    }

    // ------------------------------------------------------------------
    // Type renaming
    // ------------------------------------------------------------------

    fn rename_type(&mut self, ty: &Type) -> Type {
        match ty {
            Type::Con(name, span) => {
                if self.is_local(name) {
                    Type::Con(name.clone(), *span)
                } else if let Some(module) = self.unqualified_names.get(name) {
                    Type::Resolved(
                        ResolvedName {
                            module: module.clone(),
                            original_name: name.clone(),
                            stamp: 0,
                        },
                        *span,
                    )
                } else {
                    Type::Con(name.clone(), *span)
                }
            }
            Type::Qualified(alias, name, span) => {
                if let Some(module) = self.module_aliases.get(alias) {
                    Type::Resolved(
                        ResolvedName {
                            module: module.clone(),
                            original_name: name.clone(),
                            stamp: 0,
                        },
                        *span,
                    )
                } else if let Some(module) = self.module_aliases.get(&format!("{}.{}", alias, name))
                {
                    // Wildcard import: `Foo.Bar` where the alias is the full path.
                    Type::Resolved(
                        ResolvedName {
                            module: module.clone(),
                            original_name: name.clone(),
                            stamp: 0,
                        },
                        *span,
                    )
                } else {
                    Type::Qualified(alias.clone(), name.clone(), *span)
                }
            }
            Type::Resolved(_, _) => ty.clone(),
            Type::Var(_, _) | Type::Nat(_, _) | Type::Unit(_) | Type::Self_(_) => ty.clone(),
            Type::App(f, a, span) => Type::App(
                Box::new(self.rename_type(f)),
                Box::new(self.rename_type(a)),
                *span,
            ),
            Type::Arrow(a, b, span) => Type::Arrow(
                Box::new(self.rename_type(a)),
                Box::new(self.rename_type(b)),
                *span,
            ),
            Type::Paren(inner, span) => Type::Paren(Box::new(self.rename_type(inner)), *span),
            Type::Tuple(elems, span) => {
                Type::Tuple(elems.iter().map(|e| self.rename_type(e)).collect(), *span)
            }
            Type::Proj(base, name, span) => {
                Type::Proj(Box::new(self.rename_type(base)), name.clone(), *span)
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module_resolver::ParsedModule;
    use crate::parser::Parser;
    use std::path::PathBuf;

    fn parse_module(name: &str, source: &str) -> ParsedModule {
        let mut parser = Parser::new(source);
        let program = parser.parse_program();
        let imports = program
            .decls
            .iter()
            .filter_map(|d| {
                if let Decl::ImportDecl {
                    module_path, kind, ..
                } = d
                {
                    Some(crate::module_resolver::ModuleImport {
                        module_path: module_path.clone(),
                        kind: kind.clone(),
                    })
                } else {
                    None
                }
            })
            .collect();
        ParsedModule {
            name: name.to_string(),
            path: PathBuf::from(format!("{}.shadml", name)),
            program,
            imports,
        }
    }

    fn build_graph(modules: Vec<ParsedModule>) -> ModuleGraph {
        ModuleGraph { modules }
    }

    #[test]
    fn selective_import_only_selected_name_available() {
        let foo = parse_module("Foo", "module Foo\nbar x = x\nbaz y = y");
        let main = parse_module("Main", "module Main\nimport Foo (bar)\nmain = bar");
        let graph = build_graph(vec![foo, main]);
        let mut renamer = Renamer::new(&graph);
        let program = renamer.run();
        let main_decl = program.decls.last().unwrap();
        if let Decl::FunDecl { body, .. } = main_decl {
            assert!(
                matches!(body, Expr::Resolved(ResolvedName { module, original_name, .. }, _) if module == "Foo" && original_name == "bar"),
                "expected Resolved(Foo.bar), got {:?}",
                body
            );
        } else {
            panic!("expected FunDecl");
        }
    }

    #[test]
    fn qualified_import_bare_name_unresolved() {
        let foo = parse_module("Foo", "module Foo\nbar x = x");
        let main = parse_module("Main", "module Main\nimport Foo as F\nmain = bar");
        let graph = build_graph(vec![foo, main]);
        let mut renamer = Renamer::new(&graph);
        let program = renamer.run();
        let main_decl = program.decls.last().unwrap();
        if let Decl::FunDecl { body, .. } = main_decl {
            // `bar` is not imported unqualified, so it stays as Var
            assert!(
                matches!(body, Expr::Var(name, _) if name == "bar"),
                "expected Var(bar), got {:?}",
                body
            );
        } else {
            panic!("expected FunDecl");
        }
    }

    #[test]
    fn qualified_import_qualified_name_resolves() {
        let foo = parse_module("Foo", "module Foo\nbar x = x");
        let main = parse_module("Main", "module Main\nimport Foo as F\nmain = F.bar");
        let graph = build_graph(vec![foo, main]);
        let mut renamer = Renamer::new(&graph);
        let program = renamer.run();
        let main_decl = program.decls.last().unwrap();
        if let Decl::FunDecl { body, .. } = main_decl {
            assert!(
                matches!(body, Expr::Resolved(ResolvedName { module, original_name, .. }, _) if module == "Foo" && original_name == "bar"),
                "expected Resolved(Foo.bar), got {:?}",
                body
            );
        } else {
            panic!("expected FunDecl");
        }
    }

    #[test]
    fn wildcard_import_submodule_qualified_resolves() {
        let foo_bar = parse_module("Foo.Bar", "module Foo.Bar\nbaz x = x");
        let main = parse_module("Main", "module Main\nimport Foo.*\nmain = Foo.Bar.baz");
        let graph = build_graph(vec![foo_bar, main]);
        let mut renamer = Renamer::new(&graph);
        let program = renamer.run();
        let main_decl = program.decls.last().unwrap();
        if let Decl::FunDecl { body, .. } = main_decl {
            assert!(
                matches!(body, Expr::Resolved(ResolvedName { module, original_name, .. }, _) if module == "Foo.Bar" && original_name == "baz"),
                "expected Resolved(Foo.Bar.baz), got {:?}",
                body
            );
        } else {
            panic!("expected FunDecl, got {:?}", main_decl);
        }
    }

    #[test]
    fn local_binding_shadows_import() {
        let foo = parse_module("Foo", "module Foo\nbar x = x");
        let main = parse_module(
            "Main",
            "module Main\nimport Foo\nmain = let bar = 42 in bar",
        );
        let graph = build_graph(vec![foo, main]);
        let mut renamer = Renamer::new(&graph);
        let program = renamer.run();
        let main_decl = program.decls.last().unwrap();
        if let Decl::FunDecl { body, .. } = main_decl {
            // The inner `bar` should stay as Var because it's shadowed by the let binding
            if let Expr::Let(_, inner_body, _) = body {
                assert!(
                    matches!(inner_body.as_ref(), Expr::Var(name, _) if name == "bar"),
                    "expected Var(bar) because of shadowing, got {:?}",
                    inner_body
                );
            } else {
                panic!("expected Let, got {:?}", body);
            }
        } else {
            panic!("expected FunDecl");
        }
    }

    #[test]
    fn all_import_every_name_available() {
        let foo = parse_module("Foo", "module Foo\nbar x = x\nbaz y = y");
        let main = parse_module("Main", "module Main\nimport Foo\nmain = baz");
        let graph = build_graph(vec![foo, main]);
        let mut renamer = Renamer::new(&graph);
        let program = renamer.run();
        let main_decl = program.decls.last().unwrap();
        if let Decl::FunDecl { body, .. } = main_decl {
            assert!(
                matches!(body, Expr::Resolved(ResolvedName { module, original_name, .. }, _) if module == "Foo" && original_name == "baz"),
                "expected Resolved(Foo.baz), got {:?}",
                body
            );
        } else {
            panic!("expected FunDecl");
        }
    }
}
