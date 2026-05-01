//! AST-level reachability analysis for inter-module tree-shaking.
//!
//! After the Renamer resolves all cross-module references, this module
//! determines which declarations are actually reachable from entry points.
//! Only live declarations are included in the final flattened program.

use std::collections::{HashMap, HashSet};

use crate::parser::*;

/// Build a reference graph and return the set of live declaration indices.
///
/// Roots are `EntryPoint` and `RenderBlock` declarations.  If the program
/// contains no entry points, **all** top-level declarations are treated as
/// roots so that library-like files without shader entry points still compile.
pub fn find_live_decl_indices(program: &Program) -> HashSet<usize> {
    let graph = ReferenceGraph::build(program);
    let mut live = HashSet::new();
    let mut worklist: Vec<usize> = Vec::new();

    // Roots: entry points and render blocks.
    let mut has_roots = false;
    for (i, decl) in program.decls.iter().enumerate() {
        if matches!(decl, Decl::EntryPoint { .. } | Decl::RenderBlock { .. }) {
            has_roots = true;
            if live.insert(i) {
                worklist.push(i);
            }
        }
    }

    // No roots → everything is live (library file or intermediate module).
    if !has_roots {
        return (0..program.decls.len()).collect();
    }

    // Worklist algorithm.
    while let Some(idx) = worklist.pop() {
        for &target in &graph.refs[idx] {
            if live.insert(target) {
                worklist.push(target);
            }
        }
    }

    // Type-signature preservation: a TypeSig for a live declaration must
    // stay alive (it carries type information needed by lowering, e.g.
    // entry-point parameter types).  Mutually, if a TypeSig is alive,
    // the declaration it types should stay alive (handled by name_map).
    for (i, decl) in program.decls.iter().enumerate() {
        if let Decl::TypeSig { name, .. } = decl {
            let value_indices = graph.name_map.get(name).cloned().unwrap_or_default();
            if value_indices.iter().any(|&idx| live.contains(&idx)) {
                if live.insert(i) {
                    // The TypeSig itself may reference data types (e.g.
                    // parameter types) — push to worklist so those
                    // transitive dependencies are followed.
                    worklist.push(i);
                }
            }
        }
    }

    while let Some(idx) = worklist.pop() {
        for &target in &graph.refs[idx] {
            if live.insert(target) {
                worklist.push(target);
            }
        }
    }

    live
}

/// Return a new program containing only live declarations.
pub fn filter_live_program(program: &Program) -> Program {
    let live = find_live_decl_indices(program);
    Program {
        decls: program
            .decls
            .iter()
            .enumerate()
            .filter(|(i, _)| live.contains(i))
            .map(|(_, d)| d.clone())
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Reference graph
// ---------------------------------------------------------------------------

struct ReferenceGraph {
    /// refs[i] = set of declaration indices referenced by decl i.
    refs: Vec<HashSet<usize>>,
    /// name → declaration indices (for name resolution and TypeSig preservation).
    name_map: HashMap<String, Vec<usize>>,
}

impl ReferenceGraph {
    fn build(program: &Program) -> Self {
        let mut name_map: HashMap<String, Vec<usize>> = HashMap::new();

        // 1. Map every exported name to ALL declarations that define it.
        //    This ensures TypeSig / FunDecl pairs stay together.
        for (i, decl) in program.decls.iter().enumerate() {
            for name in decl_exported_names(decl) {
                name_map.entry(name).or_default().push(i);
            }
        }

        // 2. For each declaration, find which names it references.
        let mut refs: Vec<HashSet<usize>> = Vec::with_capacity(program.decls.len());
        for decl in &program.decls {
            let mut referenced_names = HashSet::new();
            collect_decl_refs(decl, &mut referenced_names);
            let mut ref_indices = HashSet::new();
            for name in referenced_names {
                if let Some(indices) = name_map.get(&name) {
                    for &idx in indices {
                        ref_indices.insert(idx);
                    }
                }
            }
            refs.push(ref_indices);
        }

        Self { refs, name_map }
    }
}

// ---------------------------------------------------------------------------
// Exported names from a declaration
// ---------------------------------------------------------------------------

fn decl_exported_names(decl: &Decl) -> Vec<String> {
    match decl {
        Decl::FunDecl { name, .. }
        | Decl::TypeAlias { name, .. }
        | Decl::BindingDecl { name, .. }
        | Decl::EntryPoint { name, .. }
        | Decl::ExternDecl { name, .. }
        | Decl::BuiltinExternDecl { name, .. }
        | Decl::RenderBlock { name, .. }
        | Decl::ConstDecl { name, .. }
        | Decl::BitfieldDecl { name, .. }
        | Decl::BuiltinTypeDecl { name, .. } => vec![name.clone()],
        Decl::DataDecl {
            name, constructors, ..
        } => {
            let mut names = vec![name.clone()];
            names.extend(constructors.iter().map(|c| c.name.clone()));
            names
        }
        Decl::TraitDecl { name, methods, .. } => {
            let mut names = vec![name.clone()];
            names.extend(methods.iter().map(|m| m.name.clone()));
            names
        }
        Decl::ImplDecl { methods, .. } => methods.iter().map(|m| m.name.clone()).collect(),
        Decl::BuiltinImplDecl { methods, .. } => methods.iter().map(|m| m.name.clone()).collect(),
        Decl::TypeSig { name, .. } => vec![name.clone()],
        Decl::CfgDecl { then_decls, .. } => {
            // CfgDecl doesn't export names itself; nested decls do.
            // But for safety, we also include nested names here so
            // that references to nested names can find this container.
            then_decls.iter().flat_map(decl_exported_names).collect()
        }
        Decl::ModuleDecl { .. } | Decl::ImportDecl { .. } => vec![],
    }
}

// ---------------------------------------------------------------------------
// Reference collection
// ---------------------------------------------------------------------------

fn collect_decl_refs(decl: &Decl, out: &mut HashSet<String>) {
    match decl {
        Decl::FunDecl {
            params,
            body,
            where_binds,
            ..
        } => {
            for pat in params {
                collect_pat_refs(pat, out);
            }
            collect_expr_refs(body, out);
            for bind in where_binds {
                collect_expr_refs(&bind.expr, out);
            }
        }
        Decl::DataDecl { constructors, .. } => {
            for con in constructors {
                match &con.fields {
                    ConFields::Positional(tys) => {
                        for t in tys {
                            collect_type_refs(t, out);
                        }
                    }
                    ConFields::Record(fields) => {
                        for f in fields {
                            collect_type_refs(&f.ty, out);
                        }
                    }
                    ConFields::Empty => {}
                }
            }
        }
        Decl::TypeAlias { ty, .. } => {
            collect_type_refs(ty, out);
        }
        Decl::BindingDecl { ty, .. } => {
            collect_type_refs(ty, out);
        }
        Decl::BitfieldDecl { base_ty, .. } => {
            collect_type_refs(base_ty, out);
        }
        Decl::ConstDecl { ty, value, .. } => {
            collect_type_refs(ty, out);
            collect_expr_refs(value, out);
        }
        Decl::EntryPoint { params, body, .. } => {
            for pat in params {
                collect_pat_refs(pat, out);
            }
            collect_expr_refs(body, out);
        }
        Decl::TraitDecl { methods, .. } => {
            for m in methods {
                collect_type_refs(&m.ty, out);
            }
        }
        Decl::ImplDecl {
            trait_name,
            tys,
            methods,
            ..
        } => {
            if let Some(tn) = trait_name {
                out.insert(tn.clone());
            }
            for t in tys {
                collect_type_refs(t, out);
            }
            for m in methods {
                if let Some(t) = &m.ty {
                    collect_type_refs(t, out);
                }
                for pat in &m.params {
                    collect_pat_refs(pat, out);
                }
                collect_expr_refs(&m.body, out);
            }
        }
        Decl::BuiltinImplDecl {
            trait_name, tys, ..
        } => {
            out.insert(trait_name.clone());
            for t in tys {
                collect_type_refs(t, out);
            }
        }
        Decl::ExternDecl { ty, .. } | Decl::BuiltinExternDecl { ty, .. } => {
            collect_type_refs(ty, out);
        }
        Decl::RenderBlock {
            bindings, entries, ..
        } => {
            for b in bindings {
                collect_decl_refs(b, out);
            }
            for e in entries {
                collect_decl_refs(e, out);
            }
        }
        Decl::CfgDecl {
            then_decls,
            else_decls,
            ..
        } => {
            for d in then_decls {
                collect_decl_refs(d, out);
            }
            for d in else_decls {
                collect_decl_refs(d, out);
            }
        }
        Decl::TypeSig {
            ty, constraints, ..
        } => {
            for c in constraints {
                out.insert(c.trait_name.clone());
                for t in &c.tys {
                    collect_type_refs(t, out);
                }
            }
            collect_type_refs(ty, out);
        }
        Decl::ModuleDecl { .. } | Decl::ImportDecl { .. } | Decl::BuiltinTypeDecl { .. } => {}
    }
}

fn collect_expr_refs(expr: &Expr, out: &mut HashSet<String>) {
    match expr {
        Expr::Var(name, _) | Expr::Con(name, _) | Expr::OpSection(name, _) => {
            out.insert(name.clone());
        }
        Expr::Resolved(resolved, _) => {
            out.insert(resolved.original_name.clone());
        }
        Expr::Qualified(_, _, _) => {
            // Should not appear after Renamer, but be defensive.
        }
        Expr::Lit(_, _) => {}
        Expr::App(func, arg, _) => {
            collect_expr_refs(func, out);
            collect_expr_refs(arg, out);
        }
        Expr::Infix(lhs, op, rhs, _) => {
            out.insert(op.clone());
            collect_expr_refs(lhs, out);
            collect_expr_refs(rhs, out);
        }
        Expr::Lambda(params, body, _) => {
            for pat in params {
                collect_pat_refs(pat, out);
            }
            collect_expr_refs(body, out);
        }
        Expr::Let(binds, body, _) => {
            for bind in binds {
                collect_expr_refs(&bind.expr, out);
            }
            collect_expr_refs(body, out);
        }
        Expr::If(cond, then_branch, else_branch, _) => {
            collect_expr_refs(cond, out);
            collect_expr_refs(then_branch, out);
            collect_expr_refs(else_branch, out);
        }
        Expr::Case(scrut, arms, _) => {
            collect_expr_refs(scrut, out);
            for (pat, guard, body) in arms {
                collect_pat_refs(pat, out);
                if let Some(guard_expr) = guard {
                    collect_expr_refs(guard_expr, out);
                }
                collect_expr_refs(body, out);
            }
        }
        Expr::Tuple(elems, _) => {
            for e in elems {
                collect_expr_refs(e, out);
            }
        }
        Expr::VecLit(elems, _) => {
            for e in elems {
                collect_expr_refs(e, out);
            }
        }
        Expr::Record(name, fields, _) => {
            if let Some(n) = name {
                out.insert(n.clone());
            }
            for (_, e) in fields {
                collect_expr_refs(e, out);
            }
        }
        Expr::RecordUpdate(base, fields, _) => {
            collect_expr_refs(base, out);
            for (_, e) in fields {
                collect_expr_refs(e, out);
            }
        }
        Expr::FieldAccess(base, _, _) => {
            collect_expr_refs(base, out);
        }
        Expr::Index(base, idx, _) => {
            collect_expr_refs(base, out);
            collect_expr_refs(idx, out);
        }
        Expr::Paren(inner, _) => collect_expr_refs(inner, out),
        Expr::Neg(inner, _) | Expr::Not(inner, _) | Expr::BitNot(inner, _) => {
            collect_expr_refs(inner, out);
        }
        Expr::Do(stmts, _) => {
            for stmt in stmts {
                match stmt {
                    DoStmt::Expr(e, _) => collect_expr_refs(e, out),
                    DoStmt::Bind(bind) | DoStmt::Let(bind) => {
                        collect_expr_refs(&bind.expr, out);
                    }
                }
            }
        }
        Expr::Loop(_, binds, body, _) => {
            for bind in binds {
                collect_expr_refs(&bind.expr, out);
            }
            collect_expr_refs(body, out);
        }
    }
}

fn collect_type_refs(ty: &Type, out: &mut HashSet<String>) {
    match ty {
        Type::Con(name, _) | Type::Var(name, _) => {
            out.insert(name.clone());
        }
        Type::Resolved(resolved, _) => {
            out.insert(resolved.original_name.clone());
        }
        Type::Qualified(_, _, _) => {
            // Should not appear after Renamer.
        }
        Type::Arrow(a, b, _) => {
            collect_type_refs(a, out);
            collect_type_refs(b, out);
        }
        Type::App(func, arg, _) => {
            collect_type_refs(func, out);
            collect_type_refs(arg, out);
        }
        Type::Tuple(tys, _) => {
            for t in tys {
                collect_type_refs(t, out);
            }
        }
        Type::Paren(inner, _) => collect_type_refs(inner, out),
        Type::Proj(base, _, _) => collect_type_refs(base, out),
        Type::Unit(_) => {}
        Type::Self_(_) => {}
        Type::Nat(_, _) => {}
    }
}

fn collect_pat_refs(pat: &Pat, out: &mut HashSet<String>) {
    match pat {
        Pat::Con(name, sub_pats, _) => {
            // Data constructors are references to the data declaration.
            out.insert(name.clone());
            for p in sub_pats {
                collect_pat_refs(p, out);
            }
        }
        Pat::Var(_, _) | Pat::As(_, _, _) | Pat::Wild(_) | Pat::Lit(_, _) => {}
        Pat::Tuple(sub_pats, _) | Pat::Or(sub_pats, _) => {
            for p in sub_pats {
                collect_pat_refs(p, out);
            }
        }
        Pat::Record(name, fields, _, _) => {
            out.insert(name.clone());
            for (_, maybe_pat) in fields {
                if let Some(p) = maybe_pat {
                    collect_pat_refs(p, out);
                }
            }
        }
        Pat::Paren(inner, _) => collect_pat_refs(inner, out),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    fn parse(source: &str) -> Program {
        let mut parser = Parser::new(source);
        parser.parse_program()
    }

    #[test]
    fn live_from_entry_point() {
        let prog = parse(
            r#"
add : I32 -> I32 -> I32
add x y = x + y

main : () -> ()
@compute @workgroup_size(64, 1, 1)
main _ = ()
"#,
        );
        let live = find_live_decl_indices(&prog);
        // main TypeSig + main EntryPoint are live; add is dead.
        assert_eq!(live.len(), 2);
        let names: Vec<&str> = live
            .iter()
            .map(|&i| match &prog.decls[i] {
                Decl::TypeSig { name, .. } => name.as_str(),
                Decl::EntryPoint { name, .. } => name.as_str(),
                _ => panic!("unexpected decl"),
            })
            .collect();
        assert!(names.contains(&"main"), "main should be live");
        assert!(!names.contains(&"add"), "add should not be live");
    }

    #[test]
    fn transitive_reachability() {
        let prog = parse(
            r#"
helper : I32 -> I32
helper x = x + 1

main : () -> ()
@compute @workgroup_size(64, 1, 1)
main _ = helper 42
"#,
        );
        let live = find_live_decl_indices(&prog);
        // helper TypeSig + helper FunDecl + main TypeSig + main EntryPoint = 4
        assert_eq!(live.len(), 4);
        let names: Vec<&str> = live
            .iter()
            .map(|&i| match &prog.decls[i] {
                Decl::FunDecl { name, .. } => name.as_str(),
                Decl::EntryPoint { name, .. } => name.as_str(),
                Decl::TypeSig { name, .. } => name.as_str(),
                _ => panic!("unexpected decl"),
            })
            .collect();
        assert!(names.contains(&"helper"));
        assert!(names.contains(&"main"));
    }

    #[test]
    fn no_roots_everything_live() {
        let prog = parse(
            r#"
add : I32 -> I32 -> I32
add x y = x + y

sub : I32 -> I32 -> I32
sub x y = x - y
"#,
        );
        let live = find_live_decl_indices(&prog);
        // No entry points → everything stays (4 decls: 2 type sigs + 2 fun decls).
        assert_eq!(live.len(), 4);
    }

    #[test]
    fn data_decl_referenced_by_type() {
        let prog = parse(
            r#"
data Point = Point F32 F32

origin : Point
origin = Point 0.0 0.0

main : () -> ()
@compute @workgroup_size(64, 1, 1)
main _ = ()
"#,
        );
        let live = find_live_decl_indices(&prog);
        // main TypeSig + main EntryPoint are live.
        // origin is unreferenced, Point is unreferenced.
        assert_eq!(live.len(), 2);
    }

    #[test]
    fn data_decl_referenced_by_live_expr() {
        let prog = parse(
            r#"
data Point = Point F32 F32

origin : Point
origin = Point 0.0 0.0

main : () -> ()
@compute @workgroup_size(64, 1, 1)
main _ = origin
"#,
        );
        let live = find_live_decl_indices(&prog);
        // main TypeSig + main EntryPoint + origin TypeSig + origin FunDecl + Point DataDecl = 5
        assert_eq!(live.len(), 5);
    }

    #[test]
    fn filter_live_program_keeps_only_live() {
        let prog = parse(
            r#"
unused : I32 -> I32
unused x = x

main : () -> ()
@compute @workgroup_size(64, 1, 1)
main _ = ()
"#,
        );
        let filtered = filter_live_program(&prog);
        // main TypeSig + main EntryPoint stay
        assert_eq!(filtered.decls.len(), 2);
    }
}
