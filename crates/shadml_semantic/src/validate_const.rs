use std::collections::HashSet;

use shadml_diagnostics::{Diagnostic, Label};
use shadml_parser::parser::*;

// use crate::SemanticAnalyzer; // not needed directly

/// Built-in functions that are const-evaluable in WGSL.
fn is_const_builtin(name: &str) -> bool {
    matches!(
        name,
        // Type constructors
        "vec2" | "vec3" | "vec4"
        | "mat2x2" | "mat2x3" | "mat2x4"
        | "mat3x2" | "mat3x3" | "mat3x4"
        | "mat4x2" | "mat4x3" | "mat4x4"
        | "array"
        // Numeric builtins
        | "abs" | "clamp" | "max" | "min" | "sign"
        | "countLeadingZeros" | "countOneBits" | "countTrailingZeros"
        | "extractBits" | "firstLeadingBit" | "firstTrailingBit"
        | "insertBits" | "reverseBits"
        // Logical
        | "all" | "any" | "select"
        // Casts
        | "toF32" | "toI32" | "toU32" | "toBool"
    )
}

/// Collect variable names bound by a pattern.
fn pattern_vars(pat: &Pat, vars: &mut HashSet<String>) {
    match pat {
        Pat::Var(name, _) => {
            vars.insert(name.clone());
        }
        Pat::Con(_, sub_pats, _) => {
            for p in sub_pats {
                pattern_vars(p, vars);
            }
        }
        Pat::Tuple(sub_pats, _) => {
            for p in sub_pats {
                pattern_vars(p, vars);
            }
        }
        Pat::Record(_, fields, _, _) => {
            for (_, maybe_pat) in fields {
                if let Some(p) = maybe_pat {
                    pattern_vars(p, vars);
                }
            }
        }
        Pat::As(name, p, _) => {
            vars.insert(name.clone());
            pattern_vars(p, vars);
        }
        Pat::Or(alts, _) => {
            if let Some(first) = alts.first() {
                pattern_vars(first, vars);
            }
        }
        Pat::Wild(_) | Pat::Lit(_, _) | Pat::Paren(_, _) => {}
    }
}

/// Check whether an AST expression is const-eligible.
///
/// `global_consts` contains top-level binding names that are known to be
/// const-eligible. `local_consts` contains names introduced by `let` bindings
/// or pattern matches inside the current expression.
pub fn is_const_expr(
    expr: &Expr,
    global_consts: &HashSet<String>,
    local_consts: &HashSet<String>,
) -> bool {
    match expr {
        Expr::Lit(_, _) => true,
        Expr::Var(name, _) | Expr::Con(name, _) => {
            local_consts.contains(name) || global_consts.contains(name) || is_const_builtin(name)
        }
        Expr::App(_, _, _) => {
            let (callee, args) = flatten_app(expr);
            let callee_is_const = match callee {
                Expr::Var(name, _) | Expr::Con(name, _) => {
                    local_consts.contains(name)
                        || global_consts.contains(name)
                        || is_const_builtin(name)
                }
                _ => is_const_expr(callee, global_consts, local_consts),
            };
            callee_is_const
                && args
                    .iter()
                    .all(|a| is_const_expr(a, global_consts, local_consts))
        }
        Expr::Infix(lhs, _op, rhs, _) => {
            is_const_expr(lhs, global_consts, local_consts)
                && is_const_expr(rhs, global_consts, local_consts)
        }
        Expr::Neg(inner, _) | Expr::Not(inner, _) | Expr::BitNot(inner, _) => {
            is_const_expr(inner, global_consts, local_consts)
        }
        Expr::If(cond, then_, else_, _) => {
            is_const_expr(cond, global_consts, local_consts)
                && is_const_expr(then_, global_consts, local_consts)
                && is_const_expr(else_, global_consts, local_consts)
        }
        Expr::Let(binds, body, _) => {
            let mut local = local_consts.clone();
            for bind in binds {
                if !is_const_expr(&bind.expr, global_consts, &local) {
                    return false;
                }
                local.insert(bind.name.clone());
            }
            is_const_expr(body, global_consts, &local)
        }
        Expr::Case(scrut, arms, _) => {
            if !is_const_expr(scrut, global_consts, local_consts) {
                return false;
            }
            for (pat, guard, body) in arms {
                let mut local = local_consts.clone();
                pattern_vars(pat, &mut local);
                if let Some(g) = guard {
                    if !is_const_expr(g, global_consts, &local) {
                        return false;
                    }
                }
                if !is_const_expr(body, global_consts, &local) {
                    return false;
                }
            }
            true
        }
        Expr::Tuple(elems, _) => elems
            .iter()
            .all(|e| is_const_expr(e, global_consts, local_consts)),
        Expr::Record(_, fields, _) => fields
            .iter()
            .all(|(_, e)| is_const_expr(e, global_consts, local_consts)),
        Expr::VecLit(elems, _) => elems
            .iter()
            .all(|e| is_const_expr(e, global_consts, local_consts)),
        Expr::Paren(inner, _) => is_const_expr(inner, global_consts, local_consts),
        Expr::Lambda(_, _, _) => false,
        Expr::Loop(_, _, _, _) => false,
        Expr::Do(_, _) => false,
        Expr::FieldAccess(_, _, _) => false,
        Expr::Index(_, _, _) => false,
        Expr::OpSection(_, _) => false,
        Expr::RecordUpdate(_, _, _) => false,
    }
}

/// Flatten a curried application chain into (callee, [args]).
fn flatten_app(expr: &Expr) -> (&Expr, Vec<&Expr>) {
    let mut args = Vec::new();
    let mut current = expr;
    while let Expr::App(func, arg, _) = current {
        args.push(arg.as_ref());
        current = func.as_ref();
    }
    args.reverse();
    (current, args)
}

/// Compute the fixpoint set of const-eligible top-level bindings.
///
/// Seeds are explicit `@const` declarations and `Decl::ConstDecl`.
/// We then iteratively add zero-parameter functions whose bodies are
/// composed entirely of literals, const builtins, and already-known
/// const bindings.
pub fn compute_const_bindings(program: &Program) -> HashSet<String> {
    let all_decls = Decl::flatten_cfg_decls(&program.decls);
    let mut consts: HashSet<String> = HashSet::new();

    // Seed: explicit @const and ConstDecl
    for decl in crate::helpers::all_entries_including_render_blocks(&all_decls) {
        match decl {
            Decl::FunDecl {
                name,
                params,
                attributes,
                ..
            } if attributes.iter().any(|a| a.name == "const") && params.is_empty() => {
                consts.insert(name.clone());
            }
            Decl::ConstDecl { name, .. } => {
                consts.insert(name.clone());
            }
            _ => {}
        }
    }

    // Fixpoint: auto-promotable zero-param functions
    loop {
        let mut changed = false;
        for decl in crate::helpers::all_entries_including_render_blocks(&all_decls) {
            if let Decl::FunDecl {
                name, params, body, ..
            } = decl
            {
                if params.is_empty() && !consts.contains(name) {
                    if is_const_expr(body, &consts, &HashSet::new()) {
                        consts.insert(name.clone());
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }

    consts
}

/// Validate that every `@const` binding is actually const-eligible.
///
/// Emits diagnostics for:
/// - `@const` on functions with parameters
/// - `@const` referencing non-const values or using side-effecting operations
pub fn validate_const_attributes(
    program: &Program,
    const_bindings: &HashSet<String>,
    diagnostics: &mut shadml_diagnostics::DiagnosticSink,
) {
    let all_decls = Decl::flatten_cfg_decls(&program.decls);

    for decl in crate::helpers::all_entries_including_render_blocks(&all_decls) {
        if let Decl::FunDecl {
            name,
            params,
            body,
            span,
            attributes,
            ..
        } = decl
        {
            if !attributes.iter().any(|a| a.name == "const") {
                continue;
            }

            if !params.is_empty() {
                diagnostics.push(
                    Diagnostic::error(format!(
                        "`@const` cannot be used on functions with parameters"
                    ))
                    .with_label(Label::primary(*span, "function has parameters here"))
                    .with_help("remove parameters or remove `@const`"),
                );
                continue;
            }

            if !is_const_expr(body, const_bindings, &HashSet::new()) {
                diagnostics.push(
                    Diagnostic::error(format!(
                        "`@const` binding `{}` is not compile-time evaluable",
                        name
                    ))
                    .with_label(Label::primary(*span, "not a const expression"))
                    .with_help(
                        "`@const` requires all referenced values to be literals or other `@const` bindings",
                    ),
                );
            }
        }
    }
}
