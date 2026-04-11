//! Shared helpers used by both semantic analysis and AST lowering.

use std::collections::HashMap;

use shadml_diagnostics::{Diagnostic, Label};
use shadml_parser::parser::*;
use shadml_span::Span;
use shadml_typechecker::*;

use super::*;

/// Wrap a body expression with `let` bindings from a `where` clause.
///
/// If there are no where-bindings, returns the body unchanged.
/// Otherwise produces `let <bindings> in <body>`.
pub fn desugar_where(body: &Expr, where_binds: &[LocalBind], span: Span) -> Expr {
    if where_binds.is_empty() {
        body.clone()
    } else {
        Expr::Let(where_binds.to_vec(), Box::new(body.clone()), span)
    }
}

/// Allocate a fresh type variable in the inference engine.
pub fn fresh_var_id(engine: &mut InferEngine) -> TyVarId {
    match engine.fresh_var() {
        Ty::Var(id) => id,
        _ => unreachable!(),
    }
}

/// Collect all type-variable IDs present in a scope, sorted and deduplicated.
pub fn scope_vars(scope: &HashMap<String, TyVarId>) -> Vec<TyVarId> {
    let mut vars: Vec<_> = scope.values().copied().collect();
    vars.sort_unstable();
    vars.dedup();
    vars
}

/// Apply a list of type parameters to a base type constructor.
///
/// Looks up each parameter name in `scope` and builds a curried application:
/// `Con(name) param1 param2 ...`.
pub fn apply_type_params(
    name: &str,
    type_params: &[String],
    scope: &HashMap<String, TyVarId>,
) -> Ty {
    type_params
        .iter()
        .fold(Ty::Con(name.to_string()), |ty, param| {
            let var = scope
                .get(param)
                .copied()
                .expect("type parameter should exist in scope");
            Ty::app(ty, Ty::Var(var))
        })
}

pub(crate) fn all_entries_including_render_blocks<'a>(
    decls: &'a [&'a Decl],
) -> impl Iterator<Item = &'a Decl> {
    decls.iter().copied().flat_map(|decl| {
        let mut v = vec![decl];
        if let Decl::RenderBlock { entries, .. } = decl {
            v.extend(entries.iter());
        }
        v.into_iter()
    })
}

/// Iterate over module-scope declarations plus render-block bindings and entries.
pub(crate) fn all_decls_and_render_block_contents<'a>(
    decls: &'a [&'a Decl],
) -> impl Iterator<Item = &'a Decl> {
    decls.iter().copied().flat_map(|decl| {
        let mut v = vec![decl];
        if let Decl::RenderBlock {
            bindings, entries, ..
        } = decl
        {
            v.extend(bindings.iter());
            v.extend(entries.iter());
        }
        v.into_iter()
    })
}

/// Produce a short suffix string from a Ty for name-mangling.
pub fn format_type_suffix(ty: &Ty) -> String {
    match ty {
        Ty::Con(name) => name.clone(),
        Ty::App(f, a) => format!("{}_{}", format_type_suffix(f), format_type_suffix(a)),
        Ty::Nat(n) => format!("{}", n),
        Ty::Tuple(elems) => elems
            .iter()
            .map(format_type_suffix)
            .collect::<Vec<_>>()
            .join("_"),
        Ty::AssocProj {
            trait_name, name, ..
        } => {
            debug_assert!(
                !trait_name.is_empty(),
                "AssocProj with empty trait_name should not reach mangling"
            );
            format!("{}_{}", trait_name.to_lowercase(), name.to_lowercase())
        }
        Ty::Arrow(_, _) => "fn".to_string(),
        // Type variables can appear in impl type parameters before monomorphization.
        // Format them as `t{id}` consistent with ty_to_mono_suffix_local.
        Ty::Var(id) => format!("t{}", id),
        // Forall types should be monomorphized away before mangling, but produce
        // a readable suffix rather than panicking to support partial compilation.
        Ty::Forall(_, body) => format_type_suffix(body),
        Ty::Error => "error".to_string(),
    }
}

/// Map operator symbols to readable names for mangling.
pub fn sanitise_operator_name(name: &str) -> String {
    match name {
        "+" => "add".to_string(),
        "-" => "sub".to_string(),
        "*" => "mul".to_string(),
        "/" => "div".to_string(),
        "%" => "mod".to_string(),
        "==" => "eq".to_string(),
        "/=" => "ne".to_string(),
        "<" => "lt".to_string(),
        ">" => "gt".to_string(),
        "<=" => "le".to_string(),
        ">=" => "ge".to_string(),
        "&&" => "and".to_string(),
        "||" => "or".to_string(),
        "&" => "bitand".to_string(),
        "^" => "bitxor".to_string(),
        "<<" => "shl".to_string(),
        ">>" => "shr".to_string(),
        _ => name.replace(|c: char| !c.is_alphanumeric() && c != '_', "_"),
    }
}

/// Mangle an instance method name: `(+)` for F32 → `add_F32`, `scale` for F32 → `scale_F32`.
pub fn mangle_instance_method(method_name: &str, type_suffix: &str) -> String {
    let sanitised = sanitise_operator_name(method_name);
    format!("{}_{}", sanitised, type_suffix)
}

pub(crate) fn canonical_trait_method_name(trait_name: &str, method_name: &str) -> String {
    match (trait_name, method_name) {
        ("Neg", "-") => "negate".to_owned(),
        ("BitNot", "~") => "bitnot".to_owned(),
        ("Shr", ">>") => "shr".to_owned(),
        _ => method_name.to_owned(),
    }
}

pub(crate) fn resolve_impl_method_name(
    impls: &[ImplInfo],
    name: &str,
    receiver_ty: &Ty,
) -> Option<String> {
    impls
        .iter()
        .filter(|inst| inst.tys.len() == 1 && inst.tys[0] == *receiver_ty)
        .find_map(|inst| inst.methods.get(name).cloned())
}

pub(crate) fn resolve_unique_standalone_impl_method_name(
    impls: &[ImplInfo],
    name: &str,
) -> Option<String> {
    let mut matches = impls
        .iter()
        .filter(|inst| inst.trait_name.is_none())
        .filter_map(|inst| inst.methods.get(name).cloned());
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

/// Resolve the callable target for dot-call syntax.
///
/// Matching impl methods win when the receiver type is already concrete.
/// Otherwise, any in-scope function remains callable with dot syntax so the
/// WGSL-oriented prelude keeps working naturally.
pub fn resolve_dot_call_target(
    env: &TypeEnv,
    impls: &[ImplInfo],
    name: &str,
    receiver_ty: &Ty,
) -> Option<String> {
    resolve_impl_method_name(impls, name, receiver_ty)
        .or_else(|| resolve_unique_standalone_impl_method_name(impls, name))
        .or_else(|| env.lookup(name).map(|_| name.to_string()))
}

pub(crate) fn format_predicate(predicate: &Predicate) -> String {
    format!(
        "{} {}",
        predicate.trait_name,
        format_impl_head(&predicate.tys)
    )
}

pub(crate) fn format_constraints(predicates: &[Predicate]) -> String {
    predicates
        .iter()
        .map(format_predicate)
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn format_impl_head(tys: &[Ty]) -> String {
    tys.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn predicate_matches_head(predicate: &Predicate, head: &[Ty]) -> bool {
    predicate.tys.len() == head.len()
        && predicate
            .tys
            .iter()
            .zip(head.iter())
            .all(|(actual, expected)| {
                let actual = normalize_type_aliases(actual);
                let expected = normalize_type_aliases(expected);
                actual == expected || !actual.free_vars().is_empty()
            })
}

pub fn builtin_head_for_predicate(predicate: &Predicate) -> Option<Vec<Ty>> {
    use shadml_typechecker::ty_name;

    fn scalar_numeric_name(ty: &Ty) -> Option<&str> {
        match ty {
            Ty::Con(name)
                if matches!(name.as_str(), ty_name::F32 | ty_name::I32 | ty_name::U32) =>
            {
                Some(name.as_str())
            }
            _ => None,
        }
    }

    fn same_or_var(a: &Ty, b: &Ty) -> bool {
        normalize_type_aliases(a) == normalize_type_aliases(b)
            || !a.free_vars().is_empty()
            || !b.free_vars().is_empty()
    }

    fn first_concrete(tys: &[&Ty]) -> Option<Ty> {
        tys.iter()
            .find(|ty| ty.free_vars().is_empty())
            .map(|ty| normalize_type_aliases(ty))
    }

    if !predicate.tys.iter().any(|ty| ty.free_vars().is_empty()) {
        return None;
    }

    match (predicate.trait_name.as_str(), predicate.tys.as_slice()) {
        ("Add", [a, b])
        | ("Sub", [a, b])
        | ("Div", [a, b])
        | ("Mod", [a, b])
        | ("BitAnd", [a, b])
        | ("BitXor", [a, b]) => {
            for candidate in [Ty::f32(), Ty::i32(), Ty::u32()] {
                if same_or_var(a, &candidate) && same_or_var(b, &candidate) {
                    return Some(vec![candidate.clone(), candidate]);
                }
            }
            if same_or_var(a, b) {
                if let Some(vec_ty) = a
                    .free_vars()
                    .is_empty()
                    .then(|| normalize_type_aliases(a))
                    .or_else(|| b.free_vars().is_empty().then(|| normalize_type_aliases(b)))
                {
                    if extract_vec_type(&vec_ty).is_some() || extract_mat_type(&vec_ty).is_some() {
                        return Some(vec![vec_ty.clone(), vec_ty]);
                    }
                }
            }
            None
        }
        ("Mul", [a, b]) => {
            for candidate in [Ty::f32(), Ty::i32(), Ty::u32()] {
                if same_or_var(a, &candidate) && same_or_var(b, &candidate) {
                    return Some(vec![candidate.clone(), candidate]);
                }
            }
            if let Some(lhs) = first_concrete(&[a, b]) {
                if same_or_var(&lhs, a)
                    && same_or_var(&lhs, b)
                    && (extract_vec_type(&lhs).is_some() || extract_mat_type(&lhs).is_some())
                {
                    return Some(vec![lhs.clone(), lhs]);
                }
            }
            // Vec * Scalar -> Vec
            if let Some((_, elem)) = extract_vec_type(a) {
                if scalar_numeric_name(b) == scalar_numeric_name(&elem) {
                    return Some(vec![normalize_type_aliases(a), elem.clone()]);
                }
            }
            // Scalar * Vec -> Vec
            if let Some((_, elem)) = extract_vec_type(b) {
                if scalar_numeric_name(a) == scalar_numeric_name(&elem) {
                    return Some(vec![elem.clone(), normalize_type_aliases(b)]);
                }
            }
            // Mat * Scalar -> Mat
            if let Some((_, _, elem)) = extract_mat_type(a) {
                if scalar_numeric_name(b) == scalar_numeric_name(&elem) {
                    return Some(vec![normalize_type_aliases(a), elem.clone()]);
                }
            }
            // Scalar * Mat -> Mat
            if let Some((_, _, elem)) = extract_mat_type(b) {
                if scalar_numeric_name(a) == scalar_numeric_name(&elem) {
                    return Some(vec![elem.clone(), normalize_type_aliases(b)]);
                }
            }
            // Mat * Vec -> Vec
            if let (Some((rows, cols, elem_a)), Some((cols_b, elem_b))) =
                (extract_mat_type(a), extract_vec_type(b))
            {
                if cols == cols_b && elem_a == elem_b {
                    // Result type is a vector with rows elements
                    return Some(vec![
                        normalize_type_aliases(a),
                        vector_ty(rows as u64, elem_a),
                    ]);
                }
            }
            None
        }
        ("Shl", [a, b]) | ("Shr", [a, b]) => {
            for lhs in [Ty::i32(), Ty::u32()] {
                for rhs in [Ty::i32(), Ty::u32()] {
                    if same_or_var(a, &lhs) && same_or_var(b, &rhs) {
                        return Some(vec![lhs.clone(), rhs]);
                    }
                }
            }
            None
        }
        ("Neg", [a]) | ("BitNot", [a]) => {
            for candidate in [Ty::f32(), Ty::i32(), Ty::u32()] {
                if same_or_var(a, &candidate) {
                    return Some(vec![candidate]);
                }
            }
            None
        }
        _ => None,
    }
}

pub fn replace_trait_vars(ty: &Ty, trait_vars: &[TyVarId], replacements: &[Ty]) -> Ty {
    match ty {
        Ty::Var(var) => trait_vars
            .iter()
            .position(|trait_var| trait_var == var)
            .and_then(|idx| replacements.get(idx))
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        Ty::Con(_) | Ty::Nat(_) | Ty::Error => ty.clone(),
        Ty::App(f, a) => Ty::App(
            Box::new(replace_trait_vars(f, trait_vars, replacements)),
            Box::new(replace_trait_vars(a, trait_vars, replacements)),
        ),
        Ty::Arrow(a, b) => Ty::Arrow(
            Box::new(replace_trait_vars(a, trait_vars, replacements)),
            Box::new(replace_trait_vars(b, trait_vars, replacements)),
        ),
        Ty::Tuple(elems) => Ty::Tuple(
            elems
                .iter()
                .map(|e| replace_trait_vars(e, trait_vars, replacements))
                .collect(),
        ),
        Ty::Forall(vars, body) => Ty::Forall(
            vars.clone(),
            Box::new(replace_trait_vars(body, trait_vars, replacements)),
        ),
        Ty::AssocProj {
            trait_params,
            name,
            trait_name,
        } => Ty::AssocProj {
            trait_params: trait_params
                .iter()
                .map(|t| replace_trait_vars(t, trait_vars, replacements))
                .collect(),
            name: name.clone(),
            trait_name: trait_name.clone(),
        },
    }
}

/// Resolve associated type projections by replacing `AssocProj` nodes
/// whose trait_params are fully concrete with the corresponding binding.
/// The `lookup` closure is called to resolve an `AssocProj` when its
/// trait_params are all concrete. It receives the trait_name, resolved
/// params, and assoc type name, and returns `Some(concrete_type)` if found.
pub(crate) fn resolve_assoc_projections_with<F>(ty: &Ty, lookup: &F) -> Ty
where
    F: Fn(&str, &[Ty], &str) -> Option<Ty>,
{
    match ty {
        Ty::AssocProj {
            trait_params,
            name,
            trait_name,
        } => {
            debug_assert!(
                !trait_name.is_empty(),
                "AssocProj with empty trait_name should not reach resolution"
            );
            let resolved_params: Vec<Ty> = trait_params
                .iter()
                .map(|t| resolve_assoc_projections_with(t, lookup))
                .collect();
            if resolved_params.iter().all(|t| t.free_vars().is_empty()) {
                if let Some(resolved) = lookup(trait_name, &resolved_params, name) {
                    resolve_assoc_projections_with(&resolved, lookup)
                } else {
                    Ty::AssocProj {
                        trait_params: resolved_params,
                        name: name.clone(),
                        trait_name: trait_name.clone(),
                    }
                }
            } else {
                Ty::AssocProj {
                    trait_params: resolved_params,
                    name: name.clone(),
                    trait_name: trait_name.clone(),
                }
            }
        }
        Ty::App(f, a) => Ty::App(
            Box::new(resolve_assoc_projections_with(f, lookup)),
            Box::new(resolve_assoc_projections_with(a, lookup)),
        ),
        Ty::Arrow(a, b) => Ty::Arrow(
            Box::new(resolve_assoc_projections_with(a, lookup)),
            Box::new(resolve_assoc_projections_with(b, lookup)),
        ),
        Ty::Tuple(elems) => Ty::Tuple(
            elems
                .iter()
                .map(|e| resolve_assoc_projections_with(e, lookup))
                .collect(),
        ),
        Ty::Forall(vars, body) => Ty::Forall(
            vars.clone(),
            Box::new(resolve_assoc_projections_with(body, lookup)),
        ),
        // Leaf types that cannot contain AssocProj — pass through unchanged.
        // Exhaustive match ensures new Ty variants cause a compile error here.
        Ty::Var(_) | Ty::Con(_) | Ty::Nat(_) | Ty::Error => ty.clone(),
    }
}

/// Resolve associated type projections using a direct binding map.
/// Used during impl analysis where the bindings are already known.
pub fn resolve_assoc_projections(ty: &Ty, bindings: &HashMap<String, Ty>) -> Ty {
    resolve_assoc_projections_with(ty, &|_trait_name, _params, name| {
        bindings.get(name).cloned()
    })
}

/// Resolve `AssocProj` nodes in a type by looking up matching impls.
/// When an `AssocProj` has fully-concrete trait_params, find the matching
/// impl and use its associated type bindings to resolve the projection.
pub fn resolve_assoc_projections_with_impls(
    ty: &Ty,
    impls: &[ImplInfo],
    builtin_impls: &[BuiltinImplInfo],
) -> Ty {
    resolve_assoc_projections_with(ty, &|trait_name, params, name| {
        lookup_assoc_type_binding(trait_name, params, impls, builtin_impls, name)
    })
}

/// Look up an associated type binding from a matching impl.
/// For `Mul F32 F32` with `Output = F32`, calling this with
/// trait_name="Mul", trait_params=[F32, F32], assoc_name="Output"
/// returns Some(F32).
///
/// All trait params must match the impl's `tys` (not just the first one),
/// so that `Mul F32 F32`, `Mul F32 Vec2f`, etc. are distinguished correctly.
pub(crate) fn lookup_assoc_type_binding(
    trait_name: &str,
    trait_params: &[Ty],
    impls: &[ImplInfo],
    builtin_impls: &[BuiltinImplInfo],
    assoc_name: &str,
) -> Option<Ty> {
    let normalized_params: Vec<Ty> = trait_params.iter().map(normalize_type_aliases).collect();
    // Collect all matching bindings from builtin impls
    let mut found_binding: Option<Ty> = None;

    for inst in builtin_impls {
        if inst.trait_name == trait_name {
            let matches = inst.tys.len() == normalized_params.len()
                && inst
                    .tys
                    .iter()
                    .zip(normalized_params.iter())
                    .all(|(inst_ty, param)| normalize_type_aliases(inst_ty) == *param);
            if matches {
                if let Some(binding) = inst.associated_type_bindings.get(assoc_name) {
                    let resolved =
                        resolve_assoc_projections_with_impls(binding, impls, builtin_impls);
                    match &found_binding {
                        Some(existing) if *existing != resolved => return None, // ambiguous
                        _ => found_binding = Some(resolved),
                    }
                }
            }
        }
    }

    // Also check user impls
    for inst in impls {
        if inst.trait_name.as_deref() == Some(trait_name)
            && inst.tys.len() == normalized_params.len()
            && inst
                .tys
                .iter()
                .zip(normalized_params.iter())
                .all(|(inst_ty, param)| normalize_type_aliases(inst_ty) == *param)
        {
            if let Some(binding) = inst.associated_type_bindings.get(assoc_name) {
                let resolved = resolve_assoc_projections_with_impls(binding, impls, builtin_impls);
                match &found_binding {
                    Some(existing) if *existing != resolved => return None, // ambiguous
                    _ => found_binding = Some(resolved),
                }
            }
        }
    }

    found_binding
}

// ── Swizzle / Vec helpers ─────────────────────────────────────────────

/// Check if a field name is a valid swizzle pattern (xyzw or rgba, 1-4 chars).
pub fn is_swizzle(field: &str) -> bool {
    if field.is_empty() || field.len() > 4 {
        return false;
    }
    let all_xyzw = field.chars().all(|c| matches!(c, 'x' | 'y' | 'z' | 'w'));
    let all_rgba = field.chars().all(|c| matches!(c, 'r' | 'g' | 'b' | 'a'));
    all_xyzw || all_rgba
}

/// Get the component index for a swizzle character.
///
/// Panics on invalid swizzle characters — callers should validate with [`is_swizzle`] first.
pub fn swizzle_index(c: char) -> usize {
    shadml_typechecker::swizzle_char_index(c).unwrap_or(0)
}

/// Validate that all swizzle components are within bounds for a Vec of size n.
pub fn validate_swizzle(field: &str, n: u8) -> bool {
    field.chars().all(|c| swizzle_index(c) < n as usize)
}

/// Extract Vec type info: Vec n T → Some((n, T))
pub fn extract_vec_type(ty: &Ty) -> Option<(u8, Ty)> {
    let ty = normalize_type_aliases(ty);

    // Vec n T = App(App(Con("Vec"), Nat(n)), T)
    if let Ty::App(f, scalar) = &ty {
        if let Ty::App(con, nat) = f.as_ref() {
            if let (Ty::Con(name), Ty::Nat(n)) = (con.as_ref(), nat.as_ref()) {
                if name == ty_name::VEC {
                    return Some((*n as u8, scalar.as_ref().clone()));
                }
            }
        }
    }
    None
}

/// Extract the element type from any indexed/container type.
///
/// Handles `Vec`, `Mat`, and `Tensor`/`Array`.
pub fn extract_element_type(ty: &Ty) -> Option<Ty> {
    if let Some((_, elem)) = extract_vec_type(ty) {
        return Some(elem);
    }
    if let Some((_, _, elem)) = shadml_typechecker::extract_mat_type(ty) {
        return Some(elem);
    }
    shadml_typechecker::extract_tensor_type(ty).map(|(_, elem)| elem)
}

pub fn predicate_has_impl(
    predicate: &Predicate,
    impls: &[ImplInfo],
    builtin_impls: &[BuiltinImplInfo],
) -> bool {
    impls.iter().any(|inst| {
        inst.trait_name.as_deref() == Some(predicate.trait_name.as_str())
            && inst.tys == predicate.tys
    }) || builtin_impls
        .iter()
        .any(|inst| inst.trait_name == predicate.trait_name && inst.tys == predicate.tys)
}

/// When a single impl candidate matches a predicate, propagate its associated
/// type bindings into the substitution. Finds any type variable mapped to an
/// `AssocProj` referencing the matching impl's trait and with matching params,
/// and updates the substitution to the concrete binding value.
///
/// This accelerates convergence of the fixpoint loop by resolving `AssocProj`
/// eagerly, rather than waiting for `resolve_assoc_projections_in_subst` on
/// the next iteration.
pub(crate) fn apply_assoc_type_bindings(
    engine: &mut InferEngine,
    trait_name: &str,
    impl_tys: &[Ty],
    assoc_bindings: &HashMap<String, Ty>,
) {
    if assoc_bindings.is_empty() {
        return;
    }
    let entries: Vec<(TyVarId, Ty)> = engine
        .subst
        .keys()
        .filter_map(|key| {
            let ty = engine.subst.lookup(key)?;
            match ty {
                Ty::AssocProj {
                    trait_params,
                    name,
                    trait_name: proj_trait,
                } if proj_trait == trait_name => {
                    // Check if the trait params match the impl's tys after
                    // applying the current substitution.
                    let resolved_params: Vec<Ty> = trait_params
                        .iter()
                        .map(|p| p.apply_subst(&engine.subst))
                        .collect();
                    if resolved_params.len() == impl_tys.len()
                        && resolved_params
                            .iter()
                            .zip(impl_tys.iter())
                            .all(|(actual, expected)| {
                                let actual = normalize_type_aliases(actual);
                                let expected = normalize_type_aliases(expected);
                                actual == expected
                            })
                    {
                        if let Some(binding) = assoc_bindings.get(name) {
                            return Some((key, binding.clone()));
                        }
                    }
                    None
                }
                _ => None,
            }
        })
        .collect();
    for (key, resolved) in entries {
        engine.subst.insert(key, resolved);
    }
}

pub fn try_improve_predicate_with_impls(
    engine: &mut InferEngine,
    predicate: &Predicate,
    span: Span,
    impls: &[ImplInfo],
    builtin_impls: &[BuiltinImplInfo],
) {
    let predicate = predicate.apply_subst(&engine.subst);
    // Resolve associated type projections in predicate types.
    // This converts `Add F32 (F32.Output)` to `Add F32 F32` so that
    // the predicate can match impls correctly.
    let resolved_tys: Vec<Ty> = predicate
        .tys
        .iter()
        .map(|ty| resolve_assoc_projections_with_impls(ty, impls, builtin_impls))
        .collect();
    let predicate = Predicate {
        trait_name: predicate.trait_name,
        tys: resolved_tys,
    };

    if predicate.tys.iter().any(|ty| ty.free_vars().is_empty()) {
        // Candidate: (impl_tys, associated_type_bindings)
        let candidates: Vec<(Vec<Ty>, HashMap<String, Ty>)> = impls
            .iter()
            .filter(|inst| inst.trait_name.as_deref() == Some(predicate.trait_name.as_str()))
            .filter(|inst| inst.tys.len() == predicate.tys.len())
            .filter(|inst| predicate_matches_head(&predicate, &inst.tys))
            .map(|inst| (inst.tys.clone(), inst.associated_type_bindings.clone()))
            .collect();
        if candidates.len() == 1 {
            let (tys, assoc_bindings) = &candidates[0];
            for (actual, expected) in predicate.tys.iter().zip(tys.iter()) {
                engine.unify(actual, expected, span);
            }
            // Propagate associated type bindings: find any type variable in the
            // substitution currently mapped to an AssocProj referencing this impl's
            // trait, and update it to the concrete binding. This accelerates
            // convergence by resolving AssocProj eagerly rather than waiting for
            // the next iteration of the fixpoint loop.
            apply_assoc_type_bindings(engine, &predicate.trait_name, tys, assoc_bindings);
            return;
        }
        let builtin_candidates: Vec<(Vec<Ty>, HashMap<String, Ty>)> = builtin_impls
            .iter()
            .filter(|inst| inst.trait_name == predicate.trait_name)
            .filter(|inst| inst.tys.len() == predicate.tys.len())
            .filter(|inst| predicate_matches_head(&predicate, &inst.tys))
            .map(|inst| (inst.tys.clone(), inst.associated_type_bindings.clone()))
            .collect();
        if builtin_candidates.len() == 1 {
            let (tys, assoc_bindings) = &builtin_candidates[0];
            for (actual, expected) in predicate.tys.iter().zip(tys.iter()) {
                engine.unify(actual, expected, span);
            }
            apply_assoc_type_bindings(engine, &predicate.trait_name, tys, assoc_bindings);
        } else if let Some(preferred) = builtin_head_for_predicate(&predicate) {
            let preferred = preferred
                .into_iter()
                .map(|ty| normalize_type_aliases(&ty))
                .collect::<Vec<_>>();
            if builtin_impls.iter().any(|inst| {
                inst.trait_name == predicate.trait_name
                    && inst.tys.len() == preferred.len()
                    && inst
                        .tys
                        .iter()
                        .map(normalize_type_aliases)
                        .eq(preferred.iter().cloned())
            }) {
                for (actual, expected) in predicate.tys.iter().zip(preferred.iter()) {
                    engine.unify(actual, expected, span);
                }
            }
        }
    }
}

/// Resolve associated type projections in the substitution.
///
/// After type variables are unified with concrete types, `AssocProj` nodes
/// like `AssocProj { trait_params: [F32, F32], name: "Output", trait_name: "Add" }`
/// need to be resolved to their concrete types (e.g., `F32`) by looking
/// up the matching impl's associated type bindings.
pub fn resolve_assoc_projections_in_subst(
    subst: &mut Substitution,
    impls: &[ImplInfo],
    builtin_impls: &[BuiltinImplInfo],
) {
    let entries: Vec<(TyVarId, Ty)> = subst
        .keys()
        .filter_map(|key| {
            let ty = subst.lookup(key)?;
            let substituted = ty.apply_subst(subst);
            let resolved = resolve_assoc_projections_with_impls(&substituted, impls, builtin_impls);
            if resolved != *ty {
                Some((key, resolved))
            } else {
                None
            }
        })
        .collect();
    for (key, resolved) in entries {
        subst.insert(key, resolved);
    }
}

/// Maximum number of fixpoint iterations for predicate resolution
/// before we declare convergence failure.
const MAX_PREDICATE_ITERATIONS: usize = 16;

/// Resolve inferred predicates using a fixpoint loop.
///
/// This is the canonical predicate resolution algorithm shared between
/// the semantic analyzer and the AST lowering pass. It:
///
/// 1. Runs a fixpoint loop that resolves `AssocProj` nodes, applies the
///    substitution, improves predicates against known impls, and repeats
///    until nothing changes.
///
/// 2. Retains predicates that are still ambiguous (have free type variables),
///    deduplicates them, and checks concrete predicates against available impls
///    (emitting "missing trait implementation" errors for unsatisfied ones).
pub fn resolve_predicates_fixpoint(
    engine: &mut InferEngine,
    impls: &[ImplInfo],
    builtin_impls: &[BuiltinImplInfo],
    mut pending: Vec<Predicate>,
    active_constraints: &[Predicate],
    span: Span,
) -> Vec<Predicate> {
    for _ in 0..MAX_PREDICATE_ITERATIONS {
        let mut changed = false;

        // Resolve AssocProj in predicate types before trying to improve.
        // This ensures predicates like `Add F32 (F32.Output)` become `Add F32 F32`
        // before we try to match them against impls.
        for predicate in &mut pending {
            let resolved_tys: Vec<Ty> = predicate
                .tys
                .iter()
                .map(|ty| resolve_assoc_projections_with_impls(ty, impls, builtin_impls))
                .collect();
            if resolved_tys != predicate.tys {
                predicate.tys = resolved_tys;
                changed = true;
            }
        }

        // Resolve AssocProj in the substitution before predicate improvement.
        resolve_assoc_projections_in_subst(&mut engine.subst, impls, builtin_impls);

        // Re-apply substitution after resolving AssocProj in subst values.
        for predicate in &mut pending {
            let improved = predicate.apply_subst(&engine.subst);
            if improved != *predicate {
                *predicate = improved;
                changed = true;
            }
        }

        for predicate in &mut pending {
            let current = predicate.apply_subst(&engine.subst);
            try_improve_predicate_with_impls(engine, &current, span, impls, builtin_impls);
            let improved = current.apply_subst(&engine.subst);
            changed |= improved != current;
            *predicate = improved;
        }

        // After improvement, resolve AssocProj again (improvement may have
        // unified type vars that allow further resolution).
        resolve_assoc_projections_in_subst(&mut engine.subst, impls, builtin_impls);

        for predicate in &mut pending {
            let improved = predicate.apply_subst(&engine.subst);
            if improved != *predicate {
                *predicate = improved;
                changed = true;
            }
            let resolved_tys: Vec<Ty> = predicate
                .tys
                .iter()
                .map(|ty| resolve_assoc_projections_with_impls(ty, impls, builtin_impls))
                .collect();
            if resolved_tys != predicate.tys {
                predicate.tys = resolved_tys;
                changed = true;
            }
        }

        if !changed {
            break;
        }
    }

    // Retention phase: deduplicate, check against active constraints,
    // and verify concrete predicates have impls.
    let mut retained = Vec::new();
    for predicate in pending.into_iter().map(|predicate| {
        let substituted = predicate.apply_subst(&engine.subst);
        let resolved_tys: Vec<Ty> = substituted
            .tys
            .iter()
            .map(|ty| resolve_assoc_projections_with_impls(ty, impls, builtin_impls))
            .collect();
        Predicate {
            trait_name: substituted.trait_name,
            tys: resolved_tys,
        }
    }) {
        if retained.iter().any(|existing| existing == &predicate) {
            continue;
        }
        if active_constraints.iter().any(|active| {
            let active_sub = active.apply_subst(&engine.subst);
            let active_resolved = Predicate {
                trait_name: active_sub.trait_name,
                tys: active_sub
                    .tys
                    .into_iter()
                    .map(|ty| resolve_assoc_projections_with_impls(&ty, impls, builtin_impls))
                    .collect(),
            };
            active_resolved == predicate
        }) {
            continue;
        }
        if predicate.tys.iter().all(|ty| ty.free_vars().is_empty()) {
            let resolved_tys: Vec<Ty> = predicate
                .tys
                .iter()
                .map(|ty| resolve_assoc_projections_with_impls(ty, impls, builtin_impls))
                .collect();
            let resolved_pred = Predicate {
                trait_name: predicate.trait_name.clone(),
                tys: resolved_tys,
            };
            if predicate_has_impl(&resolved_pred, impls, builtin_impls) {
                continue;
            }
            engine.diagnostics.push(
                Diagnostic::error(format!(
                    "type `{}` does not implement trait `{}`",
                    format_impl_head(&resolved_pred.tys),
                    resolved_pred.trait_name
                ))
                .with_label(Label::primary(span, "missing trait implementation"))
                .with_help(format!(
                    "define `impl {} {} where ...`",
                    resolved_pred.trait_name,
                    format_impl_head(&resolved_pred.tys)
                )),
            );
            continue;
        }
        retained.push(predicate);
    }
    retained
}

pub(crate) fn function_arity(ty: &Ty) -> usize {
    let mut arity = 0;
    let mut cursor = ty;
    while let Ty::Arrow(_, to) = cursor {
        arity += 1;
        cursor = to;
    }
    arity
}
