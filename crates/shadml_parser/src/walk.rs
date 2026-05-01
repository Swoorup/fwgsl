//! Efficient AST-walker helpers.
//!
//! Each AST node exposes iterator-like methods over its children so downstream
//! passes (Renamer, reachability, linter, formatter, LSP) can traverse the AST
//! without duplicating boilerplate match arms.

use crate::parser::*;

// ═══════════════════════════════════════════════════════════════════════════
// Decl child iterators
// ═══════════════════════════════════════════════════════════════════════════

impl Decl {
    /// All names that this declaration **defines** (for resolution and import
    /// purposes). Unlike `module_merge::exported_names`, this includes
    /// `TypeSig`, `ConstDecl` and `BitfieldDecl` so the reachability graph can
    /// track all declarations.
    pub fn names_defined(&self) -> Vec<&str> {
        match self {
            Decl::FunDecl { name, .. }
            | Decl::TypeAlias { name, .. }
            | Decl::BindingDecl { name, .. }
            | Decl::EntryPoint { name, .. }
            | Decl::ExternDecl { name, .. }
            | Decl::BuiltinExternDecl { name, .. }
            | Decl::RenderBlock { name, .. }
            | Decl::ConstDecl { name, .. }
            | Decl::BitfieldDecl { name, .. }
            | Decl::BuiltinTypeDecl { name, .. }
            | Decl::TypeSig { name, .. } => {
                vec![name.as_str()]
            }
            Decl::DataDecl {
                name, constructors, ..
            } => {
                let mut names: Vec<&str> = vec![name.as_str()];
                names.extend(constructors.iter().map(|c| c.name.as_str()));
                names
            }
            Decl::TraitDecl { name, methods, .. } => {
                let mut names: Vec<&str> = vec![name.as_str()];
                names.extend(methods.iter().map(|m| m.name.as_str()));
                names
            }
            Decl::ImplDecl { methods, .. } => {
                methods.iter().map(|m| m.name.as_str()).collect()
            }
            Decl::BuiltinImplDecl { methods, .. } => {
                methods.iter().map(|m| m.name.as_str()).collect()
            }
            Decl::CfgDecl { then_decls, .. } => {
                then_decls.iter().flat_map(|d| d.names_defined()).collect()
            }
            Decl::ModuleDecl { .. } | Decl::ImportDecl { .. } => vec![],
        }
    }

    /// Call `f` for every `&Expr` child of this declaration.
    pub fn for_each_expr(&self, f: &mut impl FnMut(&Expr)) {
        match self {
            Decl::FunDecl {
                params,
                body,
                where_binds,
                ..
            } => {
                for pat in params {
                    pat.for_each_expr(f);
                }
                f(body);
                for bind in where_binds {
                    f(&bind.expr);
                }
            }
            Decl::EntryPoint {
                params, body, ..
            } => {
                for pat in params {
                    pat.for_each_expr(f);
                }
                f(body);
            }
            Decl::ConstDecl { value, .. } => {
                f(value);
            }
            Decl::ImplDecl {
                methods, ..
            } => {
                for m in methods {
                    for pat in &m.params {
                        pat.for_each_expr(f);
                    }
                    f(&m.body);
                }
            }
            Decl::RenderBlock {
                bindings, entries, ..
            } => {
                for b in bindings {
                    b.for_each_expr(f);
                }
                for e in entries {
                    e.for_each_expr(f);
                }
            }
            Decl::CfgDecl {
                then_decls,
                else_decls,
                ..
            } => {
                for d in then_decls {
                    d.for_each_expr(f);
                }
                for d in else_decls {
                    d.for_each_expr(f);
                }
            }
            Decl::DataDecl { constructors, .. } => {
                for con in constructors {
                    match &con.fields {
                        ConFields::Positional(tys) => {
                            for t in tys {
                                t.for_each_expr(f);
                            }
                        }
                        ConFields::Record(fields) => {
                            for rf in fields {
                                rf.ty.for_each_expr(f);
                            }
                        }
                        ConFields::Empty => {}
                    }
                }
            }
            Decl::TypeAlias { ty, .. }
            | Decl::BindingDecl { ty, .. }
            | Decl::ExternDecl { ty, .. }
            | Decl::BuiltinExternDecl { ty, .. } => {
                ty.for_each_expr(f);
            }
            Decl::BitfieldDecl { base_ty, .. } => {
                base_ty.for_each_expr(f);
            }
            Decl::TypeSig { ty, constraints, .. } => {
                ty.for_each_expr(f);
                for c in constraints {
                    for t in &c.tys {
                        t.for_each_expr(f);
                    }
                }
            }
            Decl::TraitDecl { methods, .. } => {
                for m in methods {
                    m.ty.for_each_expr(f);
                }
            }
            Decl::BuiltinImplDecl { tys, .. } => {
                for t in tys {
                    t.for_each_expr(f);
                }
            }
            Decl::ModuleDecl { .. }
            | Decl::ImportDecl { .. }
            | Decl::BuiltinTypeDecl { .. } => {}
        }
    }

    /// Call `f` for every `&Type` child of this declaration.
    pub fn for_each_type(&self, f: &mut impl FnMut(&Type)) {
        match self {
            Decl::TypeAlias { ty, .. }
            | Decl::BindingDecl { ty, .. }
            | Decl::ExternDecl { ty, .. }
            | Decl::BuiltinExternDecl { ty, .. }
            | Decl::ConstDecl { ty, .. } => {
                f(ty);
            }
            Decl::TypeSig {
                ty, constraints, ..
            } => {
                f(ty);
                for c in constraints {
                    for t in &c.tys {
                        f(t);
                    }
                }
            }
            Decl::BitfieldDecl { base_ty, .. } => {
                f(base_ty);
            }
            Decl::DataDecl { constructors, .. } => {
                for con in constructors {
                    match &con.fields {
                        ConFields::Positional(tys) => {
                            for t in tys {
                                f(t);
                            }
                        }
                        ConFields::Record(fields) => {
                            for rf in fields {
                                f(&rf.ty);
                            }
                        }
                        ConFields::Empty => {}
                    }
                }
            }
            Decl::ImplDecl { tys, methods, .. } => {
                for t in tys {
                    f(t);
                }
                for m in methods {
                    if let Some(ty) = &m.ty {
                        f(ty);
                    }
                }
            }
            Decl::BuiltinImplDecl { tys, .. } => {
                for t in tys {
                    f(t);
                }
            }
            Decl::TraitDecl { methods, .. } => {
                for m in methods {
                    f(&m.ty);
                }
            }
            Decl::CfgDecl {
                then_decls,
                else_decls,
                ..
            } => {
                for d in then_decls {
                    d.for_each_type(f);
                }
                for d in else_decls {
                    d.for_each_type(f);
                }
            }
            Decl::RenderBlock {
                bindings, entries, ..
            } => {
                for b in bindings {
                    b.for_each_type(f);
                }
                for e in entries {
                    e.for_each_type(f);
                }
            }
            Decl::FunDecl { params, .. } | Decl::EntryPoint { params, .. } => {
                for pat in params {
                    pat.for_each_type(f);
                }
            }
            Decl::ModuleDecl { .. }
            | Decl::ImportDecl { .. }
            | Decl::BuiltinTypeDecl { .. } => {}
        }
    }

    /// Call `f` for every `&Pat` child of this declaration.
    pub fn for_each_pat(&self, f: &mut impl FnMut(&Pat)) {
        match self {
            Decl::FunDecl { params, .. } | Decl::EntryPoint { params, .. } => {
                for pat in params {
                    f(pat);
                }
            }
            Decl::ImplDecl { methods, .. } => {
                for m in methods {
                    for pat in &m.params {
                        f(pat);
                    }
                }
            }
            Decl::CfgDecl {
                then_decls,
                else_decls,
                ..
            } => {
                for d in then_decls {
                    d.for_each_pat(f);
                }
                for d in else_decls {
                    d.for_each_pat(f);
                }
            }
            Decl::RenderBlock {
                bindings, entries, ..
            } => {
                for b in bindings {
                    b.for_each_pat(f);
                }
                for e in entries {
                    e.for_each_pat(f);
                }
            }
            _ => {}
        }
    }

    /// Collect all locally-bound variables visible within a declaration (params
    /// for FunDecl/EntryPoint, method params for ImplDecl, etc.).
    pub fn for_each_bound_var(&self, f: &mut impl FnMut(&str)) {
        match self {
            Decl::FunDecl { name, params, where_binds, .. } => {
                f(name);
                for pat in params {
                    pat.bound_vars(f);
                }
                for bind in where_binds {
                    f(&bind.name);
                }
            }
            Decl::EntryPoint { name, params, .. } => {
                f(name);
                for pat in params {
                    pat.bound_vars(f);
                }
            }
            Decl::DataDecl {
                name, constructors, ..
            } => {
                f(name);
                for con in constructors {
                    f(&con.name);
                }
            }
            Decl::TypeAlias { name, .. }
            | Decl::BindingDecl { name, .. }
            | Decl::ExternDecl { name, .. }
            | Decl::BuiltinExternDecl { name, .. }
            | Decl::RenderBlock { name, .. }
            | Decl::ConstDecl { name, .. }
            | Decl::BitfieldDecl { name, .. } => {
                f(name);
            }
            Decl::TraitDecl { name, methods, .. } => {
                f(name);
                for m in methods {
                    f(&m.name);
                }
            }
            Decl::ImplDecl { methods, .. } => {
                for m in methods {
                    f(&m.name);
                }
            }
            Decl::BuiltinImplDecl { methods, .. } => {
                for m in methods {
                    f(&m.name);
                }
            }
            Decl::TypeSig { name, .. } => {
                // TypeSig entries need to be visible for mutual recursion.
                f(name);
            }
            Decl::CfgDecl {
                then_decls,
                else_decls,
                ..
            } => {
                for d in then_decls {
                    d.for_each_bound_var(f);
                }
                for d in else_decls {
                    d.for_each_bound_var(f);
                }
            }
            Decl::ModuleDecl { .. }
            | Decl::ImportDecl { .. }
            | Decl::BuiltinTypeDecl { .. } => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Expr child iterators
// ═══════════════════════════════════════════════════════════════════════════

impl Expr {
    /// Call `f` for each direct `&Expr` child (non-recursive into nested
    /// exprs). To recursively walk, the caller must explicitly recurse from
    /// within `f`.
    pub fn for_each_child_expr(&self, f: &mut impl FnMut(&Expr)) {
        match self {
            Expr::App(func, arg, _) => {
                f(func);
                f(arg);
            }
            Expr::Infix(lhs, _op, rhs, _) => {
                f(lhs);
                f(rhs);
            }
            Expr::Lambda(_params, body, _) => {
                f(body);
            }
            Expr::Let(binds, body, _) => {
                for bind in binds {
                    f(&bind.expr);
                }
                f(body);
            }
            Expr::If(cond, then_, else_, _) => {
                f(cond);
                f(then_);
                f(else_);
            }
            Expr::Case(scrut, arms, _) => {
                f(scrut);
                for (_, guard, body) in arms {
                    if let Some(g) = guard {
                        f(g);
                    }
                    f(body);
                }
            }
            Expr::Tuple(elems, _) | Expr::VecLit(elems, _) => {
                for e in elems {
                    f(e);
                }
            }
            Expr::Record(_, fields, _) => {
                for (_, e) in fields {
                    f(e);
                }
            }
            Expr::RecordUpdate(base, fields, _) => {
                f(base);
                for (_, e) in fields {
                    f(e);
                }
            }
            Expr::FieldAccess(base, _, _) => f(base),
            Expr::Index(base, idx, _) => {
                f(base);
                f(idx);
            }
            Expr::Neg(inner, _) | Expr::Not(inner, _) | Expr::BitNot(inner, _) => {
                f(inner);
            }
            Expr::Do(stmts, _) => {
                for stmt in stmts {
                    match stmt {
                        DoStmt::Expr(e, _) => f(e),
                        DoStmt::Bind(b) | DoStmt::Let(b) => f(&b.expr),
                    }
                }
            }
            Expr::Loop(_name, binds, body, _) => {
                for bind in binds {
                    f(&bind.expr);
                }
                f(body);
            }
            Expr::Lit(_, _)
            | Expr::Var(_, _)
            | Expr::Con(_, _)
            | Expr::OpSection(_, _)
            | Expr::Qualified(_, _, _)
            | Expr::Resolved(_, _)
            | Expr::Paren(_, _) => {}
        }
    }

    /// All names referenced by this expression node (non-recursive into
    /// children).  Handles `Var`, `Con`, `OpSection`, `Resolved`, and `Infix`.
    pub fn names_referenced(&self) -> Vec<&str> {
        match self {
            Expr::Var(name, _)
            | Expr::Con(name, _)
            | Expr::OpSection(name, _) => {
                vec![name.as_str()]
            }
            Expr::Resolved(resolved, _) => {
                vec![resolved.original_name.as_str()]
            }
            Expr::Infix(_lhs, op, _rhs, _) => {
                vec![op.as_str()]
            }
            Expr::Qualified(_, _, _) => {
                // Should not appear after Renamer; handled by the caller.
                vec![]
            }
            _ => vec![],
        }
    }

    /// Local variables bound by this expression node (non-recursive into
    /// children).  Lambda params, Let bindings, Case bindings, Loop variables,
    /// Do-statement bindings.
    pub fn for_each_bound_var(&self, f: &mut impl FnMut(&str)) {
        match self {
            Expr::Lambda(params, _, _) => {
                for pat in params {
                    pat.bound_vars(f);
                }
            }
            Expr::Let(binds, _, _) => {
                for bind in binds {
                    f(&bind.name);
                }
            }
            Expr::Case(_scrut, arms, _) => {
                for (pat, _, _) in arms {
                    pat.bound_vars(f);
                }
            }
            Expr::Do(stmts, _) => {
                for stmt in stmts {
                    match stmt {
                        DoStmt::Bind(b) | DoStmt::Let(b) => f(&b.name),
                        DoStmt::Expr(_, _) => {}
                    }
                }
            }
            Expr::Loop(name, binds, _, _) => {
                f(name);
                for bind in binds {
                    f(&bind.name);
                }
            }
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Type child iterators
// ═══════════════════════════════════════════════════════════════════════════

impl Type {
    /// Call `f` for each direct `&Type` child (non-recursive).
    pub fn for_each_child_type(&self, f: &mut impl FnMut(&Type)) {
        match self {
            Type::App(func, arg, _) => {
                f(func);
                f(arg);
            }
            Type::Arrow(a, b, _) => {
                f(a);
                f(b);
            }
            Type::Paren(inner, _) => f(inner),
            Type::Tuple(elems, _) => {
                for e in elems {
                    f(e);
                }
            }
            Type::Proj(base, _, _) => f(base),
            Type::Con(_, _)
            | Type::Var(_, _)
            | Type::Nat(_, _)
            | Type::Unit(_)
            | Type::Qualified(_, _, _)
            | Type::Resolved(_, _)
            | Type::Self_(_) => {}
        }
    }

    /// Call `f` for every `&Expr` child embedded in this type (e.g., in
    /// dependent type arguments).  Currently no `Type` variant holds an
    /// `Expr`, but `Nat` carries a `u64` so we skip it.
    pub fn for_each_expr(&self, _f: &mut impl FnMut(&Expr)) {
        match self {
            Type::Nat(_, _) => {}
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Pat child iterators
// ═══════════════════════════════════════════════════════════════════════════

impl Pat {
    /// Variables bound by this pattern node (non-recursive into sub-patterns).
    pub fn bound_vars(&self, f: &mut impl FnMut(&str)) {
        match self {
            Pat::Var(name, _) | Pat::As(name, _, _) => f(name),
            Pat::Con(_, sub_pats, _) => {
                for p in sub_pats {
                    p.bound_vars(f);
                }
            }
            Pat::Tuple(sub_pats, _) | Pat::Or(sub_pats, _) => {
                // Or-patterns bind the same variables; just use the first alt.
                if let Some(first) = sub_pats.first() {
                    first.bound_vars(f);
                }
            }
            Pat::Record(_, fields, _, _) => {
                for (_, maybe_pat) in fields {
                    if let Some(p) = maybe_pat {
                        p.bound_vars(f);
                    }
                }
            }
            Pat::Paren(inner, _) => inner.bound_vars(f),
            Pat::Wild(_) | Pat::Lit(_, _) => {}
        }
    }

    /// Call `f` for every constructor/data-type name referenced by this
    /// pattern (useful for reachability analysis).
    pub fn names_referenced(&self, f: &mut impl FnMut(&str)) {
        match self {
            Pat::Con(name, sub_pats, _) => {
                f(name);
                for p in sub_pats {
                    p.names_referenced(f);
                }
            }
            Pat::Record(name, fields, _, _) => {
                f(name);
                for (_, maybe_pat) in fields {
                    if let Some(p) = maybe_pat {
                        p.names_referenced(f);
                    }
                }
            }
            Pat::Tuple(sub_pats, _) | Pat::Or(sub_pats, _) => {
                for p in sub_pats {
                    p.names_referenced(f);
                }
            }
            Pat::Paren(inner, _) => inner.names_referenced(f),
            Pat::Var(_, _) | Pat::As(_, _, _) | Pat::Wild(_) | Pat::Lit(_, _) => {}
        }
    }

    /// Call `f` for every `&Expr` child of this pattern.
    pub fn for_each_expr(&self, _f: &mut impl FnMut(&Expr)) {
        match self {
            // Currently no Pat variants contain Expr children directly.
            _ => {}
        }
    }

    /// Call `f` for every `&Type` child of this pattern.
    pub fn for_each_type(&self, _f: &mut impl FnMut(&Type)) {
        match self {
            // Currently no Pat variants contain Type children directly.
            _ => {}
        }
    }
}
