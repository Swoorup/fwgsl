use std::collections::HashMap;

use shadml_hir::*;
use shadml_typechecker::*;

use super::*;

/// Extract the first argument type from a curried function type.
/// `(A -> B -> C)` → `Some(A)`
pub(crate) fn extract_first_arg_type(ty: &Ty) -> Option<Ty> {
    match ty {
        Ty::Arrow(arg, _) => Some((**arg).clone()),
        _ => None,
    }
}

pub(crate) fn format_predicate_local(predicate: &Predicate) -> String {
    format!(
        "{} {}",
        predicate.trait_name,
        shadml_semantic::format_impl_head(&predicate.tys)
    )
}

/// Extract a name from a pattern (for parameter names).
pub(crate) fn pat_name(pat: &Pat) -> String {
    match pat {
        Pat::Var(name, _) => name.clone(),
        Pat::Wild(_) => "_".to_string(),
        Pat::Paren(inner, _) => pat_name(inner),
        Pat::As(name, _, _) => name.clone(),
        _ => "_".to_string(),
    }
}

pub(crate) fn param_binding_name(pat: &Pat, index: usize) -> String {
    match pat {
        Pat::Var(..) | Pat::Wild(..) | Pat::Paren(..) | Pat::As(..) => pat_name(pat),
        _ => format!("_arg{}", index),
    }
}

pub(crate) fn is_generic_hir_function(function: &HirFunction) -> bool {
    !hir_function_type(function).free_vars().is_empty()
}

pub(crate) fn hir_function_type(function: &HirFunction) -> Ty {
    function
        .params
        .iter()
        .rev()
        .fold(function.return_ty.clone(), |acc, (_, ty)| {
            Ty::arrow(ty.clone(), acc)
        })
}

pub(crate) fn substitute_ty_vars(ty: &Ty, subst: &HashMap<TyVarId, Ty>) -> Ty {
    match ty {
        Ty::Var(id) => subst.get(id).cloned().unwrap_or_else(|| ty.clone()),
        Ty::Con(_) | Ty::Nat(_) | Ty::Error => ty.clone(),
        Ty::App(f, a) => Ty::App(
            Box::new(substitute_ty_vars(f, subst)),
            Box::new(substitute_ty_vars(a, subst)),
        ),
        Ty::Arrow(a, b) => Ty::Arrow(
            Box::new(substitute_ty_vars(a, subst)),
            Box::new(substitute_ty_vars(b, subst)),
        ),
        Ty::Tuple(elems) => Ty::Tuple(
            elems
                .iter()
                .map(|elem| substitute_ty_vars(elem, subst))
                .collect(),
        ),
        Ty::Forall(vars, body) => {
            Ty::Forall(vars.clone(), Box::new(substitute_ty_vars(body, subst)))
        }
        Ty::AssocProj {
            trait_params,
            name,
            trait_name,
        } => Ty::AssocProj {
            trait_params: trait_params
                .iter()
                .map(|t| substitute_ty_vars(t, subst))
                .collect(),
            name: name.clone(),
            trait_name: trait_name.clone(),
        },
    }
}

/// Apply a type transformation to every type annotation in an `HirExpr`,
/// recursively processing sub-expressions. Preserves expression structure.
///
/// Both this function and `finalize_expr` traverse `HirExpr` variants.
/// This function only applies the type mapping; `finalize_expr` additionally
/// does method dispatch rewrites. If a new `HirExpr` variant is added,
/// Rust's exhaustiveness check ensures BOTH functions are updated.
pub(crate) fn map_hir_expr_types<F>(expr: HirExpr, f: &F) -> HirExpr
where
    F: Fn(&Ty) -> Ty,
{
    match expr {
        HirExpr::Lit(lit, ty, span) => HirExpr::Lit(lit, f(&ty), span),
        HirExpr::Var(name, ty, span) => HirExpr::Var(name, f(&ty), span),
        HirExpr::Tuple(items, ty, span) => HirExpr::Tuple(
            items
                .into_iter()
                .map(|item| map_hir_expr_types(item, f))
                .collect(),
            f(&ty),
            span,
        ),
        HirExpr::TupleIndex(base, index, ty, span) => {
            HirExpr::TupleIndex(Box::new(map_hir_expr_types(*base, f)), index, f(&ty), span)
        }
        HirExpr::App(func, arg, ty, span) => HirExpr::App(
            Box::new(map_hir_expr_types(*func, f)),
            Box::new(map_hir_expr_types(*arg, f)),
            f(&ty),
            span,
        ),
        HirExpr::Let(binds, body, ty, span) => HirExpr::Let(
            binds
                .into_iter()
                .map(|(name, expr)| (name, map_hir_expr_types(expr, f)))
                .collect(),
            Box::new(map_hir_expr_types(*body, f)),
            f(&ty),
            span,
        ),
        HirExpr::Case(scrutinee, arms, ty, span) => HirExpr::Case(
            Box::new(map_hir_expr_types(*scrutinee, f)),
            arms.into_iter()
                .map(|arm| HirCaseArm {
                    pattern: arm.pattern,
                    guard: arm.guard.map(|guard| map_hir_expr_types(guard, f)),
                    body: map_hir_expr_types(arm.body, f),
                })
                .collect(),
            f(&ty),
            span,
        ),
        HirExpr::If(cond, then_expr, else_expr, ty, span) => HirExpr::If(
            Box::new(map_hir_expr_types(*cond, f)),
            Box::new(map_hir_expr_types(*then_expr, f)),
            Box::new(map_hir_expr_types(*else_expr, f)),
            f(&ty),
            span,
        ),
        HirExpr::BinOp(op, lhs, rhs, ty, span) => HirExpr::BinOp(
            op,
            Box::new(map_hir_expr_types(*lhs, f)),
            Box::new(map_hir_expr_types(*rhs, f)),
            f(&ty),
            span,
        ),
        HirExpr::UnaryNeg(inner, ty, span) => {
            HirExpr::UnaryNeg(Box::new(map_hir_expr_types(*inner, f)), f(&ty), span)
        }
        HirExpr::UnaryNot(inner, ty, span) => {
            HirExpr::UnaryNot(Box::new(map_hir_expr_types(*inner, f)), f(&ty), span)
        }
        HirExpr::UnaryBitNot(inner, ty, span) => {
            HirExpr::UnaryBitNot(Box::new(map_hir_expr_types(*inner, f)), f(&ty), span)
        }
        HirExpr::ConstructorCall(name, tag, args, ty, span) => HirExpr::ConstructorCall(
            name,
            tag,
            args.into_iter()
                .map(|arg| map_hir_expr_types(arg, f))
                .collect(),
            f(&ty),
            span,
        ),
        HirExpr::FieldAccess(base, field, ty, span) => {
            HirExpr::FieldAccess(Box::new(map_hir_expr_types(*base, f)), field, f(&ty), span)
        }
        HirExpr::Index(base, index, ty, span) => HirExpr::Index(
            Box::new(map_hir_expr_types(*base, f)),
            Box::new(map_hir_expr_types(*index, f)),
            f(&ty),
            span,
        ),
        HirExpr::Loop(loop_name, bindings, body, ty, span) => HirExpr::Loop(
            loop_name,
            bindings
                .into_iter()
                .map(|(name, expr)| (name, map_hir_expr_types(expr, f)))
                .collect(),
            Box::new(map_hir_expr_types(*body, f)),
            f(&ty),
            span,
        ),
        HirExpr::BitfieldConstruct(name, fields, ty, span) => HirExpr::BitfieldConstruct(
            name,
            fields
                .into_iter()
                .map(|(field_name, expr)| (field_name, map_hir_expr_types(expr, f)))
                .collect(),
            f(&ty),
            span,
        ),
        HirExpr::BitfieldUpdate(name, base, fields, ty, span) => HirExpr::BitfieldUpdate(
            name,
            Box::new(map_hir_expr_types(*base, f)),
            fields
                .into_iter()
                .map(|(field_name, expr)| (field_name, map_hir_expr_types(expr, f)))
                .collect(),
            f(&ty),
            span,
        ),
    }
}

/// Resolve `AssocProj` types with concrete `trait_params` in a HIR expression.
pub(crate) fn resolve_hir_expr_assoc_projections(
    expr: HirExpr,
    impls: &[shadml_semantic::ImplInfo],
    builtin_impls: &[shadml_semantic::BuiltinImplInfo],
) -> HirExpr {
    map_hir_expr_types(expr, &|ty| {
        shadml_semantic::resolve_assoc_projections_with_impls(ty, impls, builtin_impls)
    })
}

pub(crate) fn substitute_pattern_ty_vars(
    pattern: HirPattern,
    subst: &HashMap<TyVarId, Ty>,
) -> HirPattern {
    match pattern {
        HirPattern::Wild => HirPattern::Wild,
        HirPattern::Var(name, ty) => HirPattern::Var(name, substitute_ty_vars(&ty, subst)),
        HirPattern::Constructor(name, tag, sub_patterns) => HirPattern::Constructor(
            name,
            tag,
            sub_patterns
                .into_iter()
                .map(|pattern| substitute_pattern_ty_vars(pattern, subst))
                .collect(),
        ),
        HirPattern::Lit(lit) => HirPattern::Lit(lit),
        HirPattern::Or(patterns) => HirPattern::Or(
            patterns
                .into_iter()
                .map(|pattern| substitute_pattern_ty_vars(pattern, subst))
                .collect(),
        ),
    }
}

pub(crate) fn collect_hir_app_args(expr: &HirExpr) -> (&HirExpr, Vec<&HirExpr>) {
    let mut args = Vec::new();
    let mut current = expr;
    while let HirExpr::App(func, arg, _, _) = current {
        args.push(arg.as_ref());
        current = func.as_ref();
    }
    args.reverse();
    (current, args)
}

pub(crate) fn collect_hir_app_chain(expr: HirExpr) -> (HirExpr, Vec<HirExpr>) {
    let mut args = Vec::new();
    let mut current = expr;
    while let HirExpr::App(func, arg, _, _) = current {
        args.push(*arg);
        current = *func;
    }
    args.reverse();
    (current, args)
}
