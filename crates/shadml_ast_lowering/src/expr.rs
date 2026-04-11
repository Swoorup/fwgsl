use std::collections::HashMap;

use shadml_hir::*;
use shadml_parser::parser::*;
use shadml_span::Span;
use shadml_typechecker::*;

use super::*;
use crate::helpers::*;
use crate::monomorphize::*;

impl AstLowering {
    /// Lower an expression, returning the HIR expression and its inferred type.
    pub(crate) fn lower_expr(&mut self, expr: &Expr, env: &mut TypeEnv) -> (HirExpr, Ty) {
        self.lower_expr_with_hint(expr, env, None)
    }

    /// Lower an expression with an optional expected-type hint.
    ///
    /// For lambdas, the expected type is peeled to provide concrete parameter
    /// types, avoiding the fresh-variable problem that causes unresolved trait
    /// constraints in higher-order functions (foldRange, map, etc.).
    pub(crate) fn lower_expr_with_hint(
        &mut self,
        expr: &Expr,
        env: &mut TypeEnv,
        expected_ty: Option<&Ty>,
    ) -> (HirExpr, Ty) {
        match expr {
            Expr::Lit(lit, span) => {
                let (hir_lit, ty) = self.lower_lit(lit);
                (HirExpr::Lit(hir_lit, ty.clone(), *span), ty)
            }

            Expr::Var(name, span) => {
                let ty = if let Some(scheme) = env.lookup(name) {
                    self.instantiate_scheme(scheme)
                } else {
                    Ty::Error
                };
                (HirExpr::Var(name.clone(), ty.clone(), *span), ty)
            }

            Expr::Con(name, span) => {
                // A constructor used as a value. If it takes no arguments, it's
                // a nullary constructor call; otherwise it's just a variable
                // reference (will be applied later).
                let ty = if let Some(scheme) = env.lookup(name) {
                    self.instantiate_scheme(scheme)
                } else {
                    Ty::Error
                };

                if let Some(con_info) = self.constructors.get(name).cloned() {
                    match &con_info.fields {
                        ConstructorFields::Empty => (
                            HirExpr::ConstructorCall(
                                name.clone(),
                                con_info.tag,
                                vec![],
                                ty.clone(),
                                *span,
                            ),
                            ty,
                        ),
                        _ => (HirExpr::Var(name.clone(), ty.clone(), *span), ty),
                    }
                } else {
                    (HirExpr::Var(name.clone(), ty.clone(), *span), ty)
                }
            }

            Expr::App(func, arg, span) => {
                // Desugar foldRange: foldRange start end init (\acc i -> body)
                // → loop _fr (i = start) (acc = init) in if i >= end then acc else let acc = body in _fr (i + 1) acc
                if let Some(result) = self.try_lower_fold_range(func, arg, *span, env) {
                    return result;
                }

                // Beta-reduce: App(Lambda([p], body), arg) → Let([(p, arg)], body)
                // Unwrap Paren wrappers to find the underlying Lambda
                let unwrapped_func = {
                    let mut f = func.as_ref();
                    while let Expr::Paren(inner, _) = f {
                        f = inner.as_ref();
                    }
                    f
                };
                if let Expr::Lambda(pats, body, lam_span) = unwrapped_func {
                    if let Some((first_pat, rest_pats)) = pats.split_first() {
                        let mut local_env = env.clone();
                        let param_ty = self.engine.fresh_var();
                        let (hir_arg, arg_ty) = self.lower_expr(arg, &mut local_env);
                        self.engine.unify(&param_ty, &arg_ty, *span);

                        // Extract variable name from pattern for Let binding
                        let param_name = match first_pat {
                            Pat::Var(name, _) => name.clone(),
                            Pat::Wild(_) => "_lambda_param".to_string(),
                            _ => "_lambda_param".to_string(),
                        };
                        self.bind_pattern(first_pat, &param_ty, &mut local_env);

                        if rest_pats.is_empty() {
                            // Single-param lambda: Let([(name, arg)], body)
                            let (hir_body, body_ty) = self.lower_expr(body, &mut local_env);
                            (
                                HirExpr::Let(
                                    vec![(param_name, hir_arg)],
                                    Box::new(hir_body),
                                    body_ty.clone(),
                                    *span,
                                ),
                                body_ty,
                            )
                        } else {
                            // Multi-param lambda: reduce first param, recurse on remaining
                            let inner_lambda =
                                Expr::Lambda(rest_pats.to_vec(), body.clone(), *lam_span);
                            let (hir_body, body_ty) =
                                self.lower_expr(&inner_lambda, &mut local_env);
                            (
                                HirExpr::Let(
                                    vec![(param_name, hir_arg)],
                                    Box::new(hir_body),
                                    body_ty.clone(),
                                    *span,
                                ),
                                body_ty,
                            )
                        }
                    } else {
                        // Empty lambda params — shouldn't happen, fall through to normal App
                        self.lower_app_algorithm_m(func, arg, *span, env)
                    }
                } else {
                    self.lower_app_algorithm_m(func, arg, *span, env)
                }
            }

            Expr::Infix(lhs, op, rhs, span) => {
                // Desugar infix to BinOp if it's a known operator
                if let Some(binop) = BinOp::parse(op) {
                    let (hir_lhs, lhs_ty) = self.lower_expr(lhs, env);
                    let (hir_rhs, rhs_ty) = self.lower_expr(rhs, env);

                    // Get operator type and unify
                    let op_ty = if let Some(scheme) = env.lookup(op) {
                        self.instantiate_scheme(scheme)
                    } else if op == ">>" {
                        let a = self.engine.fresh_var();
                        Ty::arrow(a.clone(), Ty::arrow(a.clone(), a))
                    } else {
                        Ty::Error
                    };
                    let ret_ty = self.engine.fresh_var();
                    self.engine.unify(
                        &op_ty,
                        &Ty::arrow(lhs_ty, Ty::arrow(rhs_ty, ret_ty.clone())),
                        *span,
                    );
                    self.eagerly_resolve_assoc_proj_in_ret(&ret_ty, *span);
                    (
                        HirExpr::BinOp(
                            binop,
                            Box::new(hir_lhs),
                            Box::new(hir_rhs),
                            ret_ty.clone(),
                            *span,
                        ),
                        ret_ty,
                    )
                } else {
                    // Desugar as function application: (op lhs) rhs
                    let op_ty = if let Some(scheme) = env.lookup(op) {
                        self.instantiate_scheme(scheme)
                    } else if op == ">>" {
                        let a = self.engine.fresh_var();
                        Ty::arrow(a.clone(), Ty::arrow(a.clone(), a))
                    } else {
                        Ty::Error
                    };
                    let (hir_lhs, lhs_ty) = self.lower_expr(lhs, env);
                    let (hir_rhs, rhs_ty) = self.lower_expr(rhs, env);
                    let ret_ty = self.engine.fresh_var();
                    self.engine.unify(
                        &op_ty,
                        &Ty::arrow(lhs_ty, Ty::arrow(rhs_ty, ret_ty.clone())),
                        *span,
                    );
                    self.eagerly_resolve_assoc_proj_in_ret(&ret_ty, *span);
                    let op_expr = HirExpr::Var(op.clone(), op_ty, *span);
                    let app1_ty = self.engine.fresh_var();
                    let app1 = HirExpr::App(Box::new(op_expr), Box::new(hir_lhs), app1_ty, *span);
                    (
                        HirExpr::App(Box::new(app1), Box::new(hir_rhs), ret_ty.clone(), *span),
                        ret_ty,
                    )
                }
            }

            Expr::Lambda(pats, body, _span) => {
                let mut local_env = env.clone();
                let mut param_types = Vec::new();
                // Walk the expected function type, peeling off arrows to get
                // concrete parameter types for lambdas passed to known HOFs.
                let mut expected_cursor = expected_ty;
                for pat in pats {
                    let param_ty = if let Some(Ty::Arrow(from, to)) = expected_cursor {
                        expected_cursor = Some(to.as_ref());
                        from.apply_subst(&self.engine.subst)
                    } else {
                        self.engine.fresh_var()
                    };
                    self.bind_pattern(pat, &param_ty, &mut local_env);
                    param_types.push(param_ty);
                }
                let (hir_body, body_ty) =
                    self.lower_expr_with_hint(body, &mut local_env, expected_cursor);
                let mut result_ty = body_ty;
                for pt in param_types.into_iter().rev() {
                    result_ty = Ty::arrow(pt, result_ty);
                }
                // Lambda is desugared: for now, emit as the body directly
                // (lambdas in shadml source get applied away or become function params)
                (hir_body, result_ty)
            }

            Expr::Let(binds, body, span) => {
                let mut local_env = env.clone();
                let mut hir_binds = Vec::new();
                for bind in binds {
                    let predicate_start = self.inferred_predicates.len();
                    let (hir_expr, ty) = self.lower_expr(&bind.expr, &mut local_env);
                    let active_constraints = self.active_constraints().to_vec();
                    let inferred_constraints = self.resolve_inferred_predicates(
                        predicate_start,
                        &active_constraints,
                        *span,
                    );
                    let scheme = self.engine.generalize_with_constraints(
                        &local_env,
                        &ty,
                        &inferred_constraints,
                    );
                    local_env.insert(bind.name.clone(), scheme);
                    hir_binds.push((bind.name.clone(), hir_expr));
                    // Resolve AssocProj in the substitution after each binding so
                    // that subsequent expressions can use the resolved types.
                    shadml_semantic::resolve_assoc_projections_in_subst(
                        &mut self.engine.subst,
                        &self.impls,
                        &self.builtin_impls,
                    );
                }
                let (hir_body, body_ty) = self.lower_expr(body, &mut local_env);
                (
                    HirExpr::Let(hir_binds, Box::new(hir_body), body_ty.clone(), *span),
                    body_ty,
                )
            }

            Expr::Case(scrutinee, arms, span) => {
                let (hir_scrut, scrut_ty) = self.lower_expr(scrutinee, env);
                let result_ty = self.engine.fresh_var();

                let mut hir_arms = Vec::new();
                for (pat, guard, body) in arms {
                    let mut arm_env = env.clone();
                    self.bind_pattern(pat, &scrut_ty, &mut arm_env);
                    let hir_pattern = self.lower_pattern(pat, &scrut_ty);
                    let hir_guard = guard.as_ref().map(|g| {
                        let (hir_g, guard_ty) = self.lower_expr(g, &mut arm_env);
                        self.engine.unify(&guard_ty, &Ty::bool(), *span);
                        hir_g
                    });
                    let (hir_body, body_ty) = self.lower_expr(body, &mut arm_env);
                    self.engine.unify(&result_ty, &body_ty, *span);
                    hir_arms.push(HirCaseArm {
                        pattern: hir_pattern,
                        guard: hir_guard,
                        body: hir_body,
                    });
                }

                (
                    HirExpr::Case(Box::new(hir_scrut), hir_arms, result_ty.clone(), *span),
                    result_ty,
                )
            }

            Expr::If(cond, then_expr, else_expr, span) => {
                let (hir_cond, cond_ty) = self.lower_expr(cond, env);
                self.engine.unify(&cond_ty, &Ty::bool(), *span);
                let (hir_then, then_ty) = self.lower_expr(then_expr, env);
                let (hir_else, else_ty) = self.lower_expr(else_expr, env);
                self.engine.unify(&then_ty, &else_ty, *span);
                (
                    HirExpr::If(
                        Box::new(hir_cond),
                        Box::new(hir_then),
                        Box::new(hir_else),
                        then_ty.clone(),
                        *span,
                    ),
                    then_ty,
                )
            }

            Expr::Paren(inner, _) => self.lower_expr(inner, env),

            Expr::Tuple(elems, span) => {
                if elems.is_empty() {
                    (HirExpr::Tuple(vec![], Ty::unit(), *span), Ty::unit())
                } else {
                    let mut hir_elems = Vec::new();
                    let mut tys = Vec::new();
                    for e in elems {
                        let (hir_elem, ty) = self.lower_expr(e, env);
                        hir_elems.push(hir_elem);
                        tys.push(ty);
                    }
                    let tuple_ty = Ty::Tuple(tys);
                    (HirExpr::Tuple(hir_elems, tuple_ty.clone(), *span), tuple_ty)
                }
            }

            Expr::Record(name, fields, span) => {
                if let Some(bf_name) = name {
                    if let Some(bf_fields) = self.bitfield_fields.get(bf_name).cloned() {
                        // Bitfield construction: lower to shift+OR chain
                        return self
                            .lower_bitfield_construct(bf_name, &bf_fields, fields, env, *span);
                    }
                }
                // Regular record construction: Name { f1 = e1, f2 = e2 }
                if let Some(con_name) = name {
                    if let Some(con_info) = self.constructors.get(con_name).cloned() {
                        let con_info = con_info.instantiate(&mut self.engine);
                        if let ConstructorFields::Record(con_fields) = &con_info.fields {
                            let field_map: HashMap<&str, &Expr> =
                                fields.iter().map(|(n, e)| (n.as_str(), e)).collect();
                            let mut args = Vec::new();
                            for (field_name, field_ty) in con_fields {
                                if let Some(val_expr) = field_map.get(field_name.as_str()) {
                                    let (hir_val, val_ty) = self.lower_expr(val_expr, env);
                                    self.engine.unify(&val_ty, field_ty, *span);
                                    args.push(hir_val);
                                } else {
                                    self.engine.diagnostics.push(
                                        shadml_diagnostics::Diagnostic::error(format!(
                                            "missing field `{}` in record construction of `{}`",
                                            field_name, con_name
                                        ))
                                        .with_label(
                                            shadml_diagnostics::Label::primary(
                                                *span,
                                                "missing field",
                                            ),
                                        )
                                        .with_help("all record fields must be provided in construction; add the missing field"),
                                    );
                                    args.push(HirExpr::Lit(
                                        HirLit::Int(0),
                                        field_ty.clone(),
                                        *span,
                                    ));
                                }
                            }
                            let result_ty = con_info.result_ty.clone();
                            return (
                                HirExpr::ConstructorCall(
                                    con_name.clone(),
                                    con_info.tag,
                                    args,
                                    result_ty.clone(),
                                    *span,
                                ),
                                result_ty,
                            );
                        }
                    }
                }
                // Fallback for anonymous records or unknown constructors
                if let Some((_, expr)) = fields.first() {
                    self.lower_expr(expr, env)
                } else {
                    (HirExpr::Lit(HirLit::Int(0), Ty::unit(), *span), Ty::unit())
                }
            }

            Expr::FieldAccess(expr, field, span) => {
                let (hir_expr, expr_ty) = self.lower_expr(expr, env);
                let expr_ty_final = self.finalize_resolve(&expr_ty);

                // 1. Check for Vec swizzle patterns
                if shadml_semantic::is_swizzle(field) {
                    if let Some((n, scalar)) = shadml_semantic::extract_vec_type(&expr_ty_final) {
                        if shadml_semantic::validate_swizzle(field, n) {
                            let result_ty = if field.len() == 1 {
                                scalar
                            } else {
                                Ty::app(
                                    Ty::app(
                                        Ty::Con(ty_name::VEC.into()),
                                        Ty::Nat(field.len() as u64),
                                    ),
                                    scalar,
                                )
                            };
                            return (
                                HirExpr::FieldAccess(
                                    Box::new(hir_expr),
                                    field.clone(),
                                    result_ty.clone(),
                                    *span,
                                ),
                                result_ty,
                            );
                        }
                    }

                    // Matrix column access: mat.x -> Vec<rows, scalar>
                    if field.len() == 1 {
                        if let Some((rows, cols, scalar)) =
                            shadml_typechecker::extract_mat_type(&expr_ty_final)
                        {
                            let col_index =
                                shadml_semantic::swizzle_index(field.chars().next().unwrap());
                            if col_index < cols as usize {
                                let result_ty = Ty::app(
                                    Ty::app(Ty::Con(ty_name::VEC.into()), Ty::Nat(rows as u64)),
                                    scalar,
                                );
                                return (
                                    HirExpr::FieldAccess(
                                        Box::new(hir_expr),
                                        field.clone(),
                                        result_ty.clone(),
                                        *span,
                                    ),
                                    result_ty,
                                );
                            }
                        }
                    }
                }

                // 2. Method-call syntax sugar: `x.method` → `method x`
                if let Some(name) = shadml_semantic::resolve_dot_call_target(
                    env,
                    &self.impls,
                    field,
                    &expr_ty_final,
                ) {
                    if let Some(scheme) = env.lookup(&name) {
                        let func_ty = self.instantiate_scheme(scheme);
                        let ret_ty = self.engine.fresh_var();
                        let expected = Ty::arrow(expr_ty, ret_ty.clone());
                        self.engine.unify(&func_ty, &expected, *span);
                        return (
                            HirExpr::App(
                                Box::new(HirExpr::Var(name, func_ty, *span)),
                                Box::new(hir_expr),
                                ret_ty.clone(),
                                *span,
                            ),
                            ret_ty,
                        );
                    }
                }

                // 3. Regular field access (struct fields, bitfield fields)
                let result_ty =
                    if let Some(ty) = self.resolve_record_field_type(&expr_ty_final, field) {
                        ty
                    } else if let Ty::Con(ref type_name) = expr_ty_final {
                        // Check if it's a valid bitfield field
                        let bf_valid = self
                            .bitfield_fields
                            .get(type_name.as_str())
                            .is_some_and(|fields| fields.iter().any(|(n, _)| n == field));
                        if bf_valid {
                            // Bitfield field access — type inferred from context
                            self.engine.fresh_var()
                        } else {
                            // Check if the type is a known struct or bitfield with no matching field
                            let is_known_record = self
                                .constructors
                                .get(type_name.as_str())
                                .is_some_and(|c| matches!(&c.fields, ConstructorFields::Record(_)));
                            let is_known_bitfield =
                                self.bitfield_fields.contains_key(type_name.as_str());
                            if is_known_record || is_known_bitfield {
                                self.engine.diagnostics.push(
                                    shadml_diagnostics::Diagnostic::error(format!(
                                        "no method or field `{}` on type `{}`",
                                        field, type_name
                                    ))
                                    .with_label(
                                        shadml_diagnostics::Label::primary(*span, "unknown member"),
                                    ),
                                );
                            }
                            self.engine.fresh_var()
                        }
                    } else {
                        self.engine.fresh_var()
                    };
                (
                    HirExpr::FieldAccess(
                        Box::new(hir_expr),
                        field.clone(),
                        result_ty.clone(),
                        *span,
                    ),
                    result_ty,
                )
            }

            Expr::Index(base, index, span) => {
                let (hir_base, base_ty) = self.lower_expr(base, env);
                let (hir_index, _idx_ty) = self.lower_expr(index, env);
                let base_ty_final = self.finalize_resolve(&base_ty);

                // Matrix column indexing: mat[i] -> Vec<rows, scalar>
                if let Some((rows, _cols, scalar)) =
                    shadml_typechecker::extract_mat_type(&base_ty_final)
                {
                    let result_ty = Ty::app(
                        Ty::app(Ty::Con(ty_name::VEC.into()), Ty::Nat(rows as u64)),
                        scalar,
                    );
                    return (
                        HirExpr::Index(
                            Box::new(hir_base),
                            Box::new(hir_index),
                            result_ty.clone(),
                            *span,
                        ),
                        result_ty,
                    );
                }

                // Vector element indexing: vec[i] -> scalar
                if let Some((_, elem_ty)) = extract_vec_type(&base_ty_final) {
                    return (
                        HirExpr::Index(
                            Box::new(hir_base),
                            Box::new(hir_index),
                            elem_ty.clone(),
                            *span,
                        ),
                        elem_ty,
                    );
                }

                // Array/tensor element indexing
                if let Some((_, elem_ty)) = shadml_typechecker::extract_tensor_type(&base_ty_final)
                {
                    return (
                        HirExpr::Index(
                            Box::new(hir_base),
                            Box::new(hir_index),
                            elem_ty.clone(),
                            *span,
                        ),
                        elem_ty,
                    );
                }

                let result_ty = self.engine.fresh_var();
                (
                    HirExpr::Index(
                        Box::new(hir_base),
                        Box::new(hir_index),
                        result_ty.clone(),
                        *span,
                    ),
                    result_ty,
                )
            }

            Expr::VecLit(elems, span) => {
                // Lower each element, compute total components, emit as vecN call.
                let scalar_ty = self.engine.fresh_var();
                let mut total_components: u64 = 0;
                let mut hir_args = Vec::new();

                for elem in elems {
                    let (hir_elem, elem_ty) = self.lower_expr(elem, env);
                    let elem_ty_final = self.finalize_resolve(&elem_ty);

                    if let Some((n, inner_scalar)) =
                        shadml_semantic::extract_vec_type(&elem_ty_final)
                    {
                        total_components += n as u64;
                        self.engine.unify(&scalar_ty, &inner_scalar, *span);
                    } else {
                        total_components += 1;
                        self.engine.unify(&scalar_ty, &elem_ty, *span);
                    }

                    hir_args.push(hir_elem);
                }

                // Construct vecN call: App(App(...(Var("vecN"), arg1), arg2), argN)
                let n = total_components.clamp(2, 4);
                let vec_name = format!("vec{}", n);
                let result_ty = Ty::app(
                    Ty::app(Ty::Con(ty_name::VEC.into()), Ty::Nat(n)),
                    scalar_ty.clone(),
                );

                // Build curried application chain
                let mut expr = HirExpr::Var(vec_name, result_ty.clone(), *span);
                for arg in hir_args {
                    let app_ty = result_ty.clone();
                    expr = HirExpr::App(Box::new(expr), Box::new(arg), app_ty, *span);
                }

                (expr, result_ty)
            }

            Expr::OpSection(op, span) => {
                let ty = if let Some(scheme) = env.lookup(op) {
                    self.instantiate_scheme(scheme)
                } else {
                    Ty::Error
                };
                (HirExpr::Var(op.clone(), ty.clone(), *span), ty)
            }

            Expr::Neg(inner, span) => {
                let (hir_inner, inner_ty) = self.lower_expr(inner, env);
                (
                    HirExpr::UnaryNeg(Box::new(hir_inner), inner_ty.clone(), *span),
                    inner_ty,
                )
            }

            Expr::Not(inner, span) => {
                let (hir_inner, _inner_ty) = self.lower_expr(inner, env);
                let bool_ty = Ty::bool();
                (
                    HirExpr::UnaryNot(Box::new(hir_inner), bool_ty.clone(), *span),
                    bool_ty,
                )
            }

            Expr::BitNot(inner, span) => {
                let (hir_inner, inner_ty) = self.lower_expr(inner, env);
                (
                    HirExpr::UnaryBitNot(Box::new(hir_inner), inner_ty.clone(), *span),
                    inner_ty,
                )
            }

            Expr::Do(stmts, span) => {
                // Lower do-notation as sequential let bindings
                let mut local_env = env.clone();
                let mut hir_binds = Vec::new();
                let mut last_expr = None;

                for stmt in stmts {
                    match stmt {
                        DoStmt::Expr(expr, _) => {
                            let (hir_expr, _ty) = self.lower_expr(expr, &mut local_env);
                            last_expr = Some(hir_expr);
                        }
                        DoStmt::Bind(bind) => {
                            let (hir_expr, ty) = self.lower_expr(&bind.expr, &mut local_env);
                            let inner_ty = self.engine.fresh_var();
                            local_env.insert(bind.name.clone(), Scheme::mono(inner_ty));
                            hir_binds.push((bind.name.clone(), hir_expr));
                            last_expr = None;
                            let _ = ty;
                        }
                        DoStmt::Let(bind) => {
                            let (hir_expr, ty) = self.lower_expr(&bind.expr, &mut local_env);
                            local_env.insert(bind.name.clone(), Scheme::mono(ty));
                            hir_binds.push((bind.name.clone(), hir_expr));
                            last_expr = None;
                        }
                    }
                }

                let body = last_expr.unwrap_or(HirExpr::Lit(HirLit::Int(0), Ty::unit(), *span));
                let body_ty = body.ty().clone();
                if hir_binds.is_empty() {
                    (body, body_ty)
                } else {
                    (
                        HirExpr::Let(hir_binds, Box::new(body), body_ty.clone(), *span),
                        body_ty,
                    )
                }
            }

            Expr::RecordUpdate(base, fields, span) => {
                let (hir_base, base_ty) = self.lower_expr(base, env);
                let base_ty_final = self.finalize_resolve(&base_ty);

                // Try to resolve bitfield type from the finalized base type
                let bf_match = if let Ty::Con(ref type_name) = base_ty_final {
                    self.bitfield_fields
                        .get(type_name)
                        .cloned()
                        .map(|bf| (type_name.clone(), bf))
                } else {
                    None
                };

                // Fallback: infer bitfield type from field names
                let bf_match = bf_match.or_else(|| {
                    if let Some((first_field, _)) = fields.first() {
                        for (bf_name, bf_fields) in &self.bitfield_fields {
                            if bf_fields.iter().any(|(n, _)| n == first_field) {
                                return Some((bf_name.clone(), bf_fields.clone()));
                            }
                        }
                    }
                    None
                });

                if let Some((type_name, bf_fields)) = bf_match {
                    let ty = Ty::Con(type_name.clone());
                    self.engine.unify(&base_ty, &ty, *span);
                    return self.lower_bitfield_update(
                        &type_name, hir_base, &bf_fields, fields, env, *span,
                    );
                }

                // Non-bitfield record update: expr { field1 = val1, field2 = val2 }
                // Look up the constructor for this record type.
                let base_ty_final = self.finalize_resolve(&base_ty);
                let type_name = if let Ty::Con(ref name) = base_ty_final {
                    Some(name.clone())
                } else {
                    None
                };

                // Find a record constructor for this type
                let con_match = type_name.as_ref().and_then(|tn| {
                    let dt = self.data_types.get(tn)?;
                    for con_name in &dt.constructors {
                        if let Some(con_info) = self.constructors.get(con_name) {
                            if matches!(con_info.fields, ConstructorFields::Record(_)) {
                                return Some(con_info.clone());
                            }
                        }
                    }
                    None
                });

                if let Some(con_info) = con_match {
                    let con_info = con_info.instantiate(&mut self.engine);
                    if let ConstructorFields::Record(con_fields) = &con_info.fields {
                        self.engine.unify(&base_ty, &con_info.result_ty, *span);

                        // Bind base expression to a temp variable to avoid re-evaluation
                        let base_var = format!("_rec_base_{}", span.start);
                        let base_var_ty = base_ty.clone();

                        // Lower the updated field values
                        let update_map: HashMap<&str, &Expr> =
                            fields.iter().map(|(n, e)| (n.as_str(), e)).collect();

                        // Build constructor args: updated fields use new values,
                        // unchanged fields use FieldAccess on the base variable
                        let mut args = Vec::new();
                        for (field_name, field_ty) in con_fields {
                            if let Some(val_expr) = update_map.get(field_name.as_str()) {
                                let (hir_val, val_ty) = self.lower_expr(val_expr, env);
                                self.engine.unify(&val_ty, field_ty, *span);
                                args.push(hir_val);
                            } else {
                                // Copy from base: base_var.field_name
                                args.push(HirExpr::FieldAccess(
                                    Box::new(HirExpr::Var(
                                        base_var.clone(),
                                        base_var_ty.clone(),
                                        *span,
                                    )),
                                    field_name.clone(),
                                    field_ty.clone(),
                                    *span,
                                ));
                            }
                        }

                        let result_ty = con_info.result_ty.clone();
                        let constructor_call = HirExpr::ConstructorCall(
                            con_info.type_name.clone(),
                            con_info.tag,
                            args,
                            result_ty.clone(),
                            *span,
                        );

                        // Wrap in let to bind the base: let _rec_base = <base> in Constructor(...)
                        return (
                            HirExpr::Let(
                                vec![(base_var, hir_base)],
                                Box::new(constructor_call),
                                result_ty.clone(),
                                *span,
                            ),
                            result_ty,
                        );
                    }
                }

                // Fallback: not a record type
                self.engine.diagnostics.push(
                    shadml_diagnostics::Diagnostic::error(
                        "Record update syntax requires a record type (data type with named fields)",
                    )
                    .with_label(shadml_diagnostics::Label::primary(
                        *span,
                        "not a record type",
                    ))
                    .with_help("use `expr { field = value }` only on records defined with `data Name = Name { field : Type }`"),
                );
                (hir_base, base_ty)
            }

            Expr::Loop(loop_name, bindings, body, span) => {
                // Lower each binding's initial value
                let mut hir_bindings = Vec::new();
                let mut loop_env = env.clone();
                // Build the type of the loop result from the first binding
                // (for single-binding loops), or a tuple of all binding types.
                let mut binding_tys = Vec::new();
                for bind in bindings {
                    let (hir_init, init_ty) = self.lower_expr(&bind.expr, env);
                    loop_env.insert(bind.name.clone(), Scheme::mono(init_ty.clone()));
                    binding_tys.push(init_ty);
                    hir_bindings.push((bind.name.clone(), hir_init));
                }

                // The result type of the loop is inferred from the body's
                // non-recursive branches (not the tuple of bindings).
                let result_ty = self.engine.fresh_var();
                // The loop name is a function: binding_ty1 -> ... -> result_ty
                let loop_fn_ty = binding_tys.iter().rev().fold(result_ty.clone(), |acc, ty| {
                    Ty::Arrow(Box::new(ty.clone()), Box::new(acc))
                });
                loop_env.insert(loop_name.clone(), Scheme::mono(loop_fn_ty));

                let (hir_body, body_ty) = self.lower_expr(body, &mut loop_env);
                self.engine.unify(&result_ty, &body_ty, *span);
                (
                    HirExpr::Loop(
                        loop_name.clone(),
                        hir_bindings,
                        Box::new(hir_body),
                        result_ty.clone(),
                        *span,
                    ),
                    result_ty,
                )
            }
        }
    }

    /// Algorithm M application lowering: infer func first, extract expected
    /// arg type, then lower arg with that hint. This propagates concrete
    /// parameter types into lambdas passed to higher-order functions.
    fn lower_app_algorithm_m(
        &mut self,
        func: &Expr,
        arg: &Expr,
        span: Span,
        env: &mut TypeEnv,
    ) -> (HirExpr, Ty) {
        let (hir_func, func_ty) = self.lower_expr(func, env);
        let fresh_arg_ty = self.engine.fresh_var();
        let fresh_ret_ty = self.engine.fresh_var();
        let expected = Ty::arrow(fresh_arg_ty.clone(), fresh_ret_ty.clone());
        self.engine.unify(&func_ty, &expected, span);

        let arg_hint = fresh_arg_ty.apply_subst(&self.engine.subst);
        let (hir_arg, arg_ty) = self.lower_expr_with_hint(arg, env, Some(&arg_hint));
        self.engine.unify(&arg_ty, &fresh_arg_ty, span);
        (
            HirExpr::App(
                Box::new(hir_func),
                Box::new(hir_arg),
                fresh_ret_ty.clone(),
                span,
            ),
            fresh_ret_ty,
        )
    }

    pub(crate) fn lower_lit(&self, lit: &Lit) -> (HirLit, Ty) {
        match lit {
            Lit::Int(v) => (HirLit::Int(*v), Ty::i32()),
            Lit::UInt(v) => (HirLit::UInt(*v), Ty::u32()),
            Lit::Float(v) => (HirLit::Float(*v), Ty::f32()),
            Lit::String(_) => (HirLit::Int(0), Ty::Con(ty_name::STRING.into())),
            Lit::Char(_) => (HirLit::Int(0), Ty::Con("Char".into())),
        }
    }

    /// Lower bitfield construction `Name { f1 = e1, f2 = e2 }`.
    /// Produces `HirExpr::BitfieldConstruct` which the MIR lowering converts
    /// to actual shift+OR bit manipulation.
    pub(crate) fn lower_bitfield_construct(
        &mut self,
        bf_name: &str,
        bf_fields: &[(String, BitfieldFieldMeta)],
        user_fields: &[(String, Expr)],
        env: &mut TypeEnv,
        span: shadml_span::Span,
    ) -> (HirExpr, Ty) {
        let ty = Ty::Con(bf_name.to_string());
        let mut hir_fields = Vec::new();

        for (field_name, field_expr) in user_fields {
            // Find the field metadata
            let meta = bf_fields.iter().find(|(n, _)| n == field_name);
            if meta.is_none() {
                self.engine.diagnostics.push(
                    shadml_diagnostics::Diagnostic::error(format!(
                        "unknown bitfield field '{}' in '{}'",
                        field_name, bf_name
                    ))
                    .with_label(shadml_diagnostics::Label::primary(span, "unknown field"))
                    .with_help("check the bitfield definition for available field names"),
                );
                continue;
            }

            let (hir_val, _val_ty) = self.lower_expr(field_expr, env);
            hir_fields.push((field_name.clone(), hir_val));
        }

        (
            HirExpr::BitfieldConstruct(bf_name.to_string(), hir_fields, ty.clone(), span),
            ty,
        )
    }

    /// Lower bitfield functional update `base { f1 = e1, f2 = e2 }`.
    /// Produces `HirExpr::BitfieldUpdate` which the MIR lowering converts to
    /// `(base & ~mask1 & ~mask2) | ((val1 & mask1) << offset1) | ...`
    pub(crate) fn lower_bitfield_update(
        &mut self,
        type_name: &str,
        hir_base: HirExpr,
        bf_fields: &[(String, BitfieldFieldMeta)],
        user_fields: &[(String, Expr)],
        env: &mut TypeEnv,
        span: shadml_span::Span,
    ) -> (HirExpr, Ty) {
        let ty = Ty::Con(type_name.to_string());
        let mut hir_fields = Vec::new();

        for (field_name, field_expr) in user_fields {
            let meta = bf_fields.iter().find(|(n, _)| n == field_name);
            if meta.is_none() {
                self.engine.diagnostics.push(
                    shadml_diagnostics::Diagnostic::error(format!(
                        "unknown bitfield field '{}' in '{}'",
                        field_name, type_name
                    ))
                    .with_label(shadml_diagnostics::Label::primary(span, "unknown field"))
                    .with_help("check the bitfield definition for available field names"),
                );
                continue;
            }

            let (hir_val, _val_ty) = self.lower_expr(field_expr, env);
            hir_fields.push((field_name.clone(), hir_val));
        }

        (
            HirExpr::BitfieldUpdate(
                type_name.to_string(),
                Box::new(hir_base),
                hir_fields,
                ty.clone(),
                span,
            ),
            ty,
        )
    }

    /// Try to desugar `foldRange start end init (\acc i -> body)` into a Loop.
    /// Returns Some((hir_expr, ty)) if the pattern matches, None otherwise.
    pub(crate) fn try_lower_fold_range(
        &mut self,
        func: &Expr,
        arg: &Expr,
        span: Span,
        env: &mut TypeEnv,
    ) -> Option<(HirExpr, Ty)> {
        // Flatten: App(App(App(App(Var("foldRange"), start), end), init), lambda)
        // The outermost App has func=App(App(App(Var("foldRange"), start), end), init), arg=lambda
        // We need to peel 3 layers of App to find Var("foldRange") at the core.
        let (f3, init) = match func {
            Expr::App(f, a, _) => (f.as_ref(), a.as_ref()),
            _ => return None,
        };
        let (f2, end_expr) = match f3 {
            Expr::App(f, a, _) => (f.as_ref(), a.as_ref()),
            _ => return None,
        };
        let (f1, start_expr) = match f2 {
            Expr::App(f, a, _) => (f.as_ref(), a.as_ref()),
            _ => return None,
        };
        // Unwrap parens around the function name
        let mut head = f1;
        while let Expr::Paren(inner, _) = head {
            head = inner.as_ref();
        }
        match head {
            Expr::Var(name, _) if name == "foldRange" => {}
            _ => return None,
        }

        // arg must be a Lambda with exactly 2 params, or a named function
        let mut lambda = arg;
        while let Expr::Paren(inner, _) = lambda {
            lambda = inner.as_ref();
        }

        // Extract acc/idx names from lambda, or use defaults for named function case
        let (acc_name, idx_name, is_lambda) = match lambda {
            Expr::Lambda(pats, _, _) if pats.len() == 2 => {
                let an = match &pats[0] {
                    Pat::Var(name, _) => name.clone(),
                    _ => "_fold_acc".to_string(),
                };
                let in_ = match &pats[1] {
                    Pat::Var(name, _) => name.clone(),
                    _ => "_fold_i".to_string(),
                };
                (an, in_, true)
            }
            // Named function or other expression — we'll build App(App(f, acc), i)
            _ => ("_fold_acc".to_string(), "_fold_i".to_string(), false),
        };

        // Lower start, end, init
        let (hir_start, start_ty) = self.lower_expr(start_expr, env);
        let (hir_end, end_ty) = self.lower_expr(end_expr, env);
        let (hir_init, init_ty) = self.lower_expr(init, env);

        // Unify start and end with I32
        self.engine.unify(&start_ty, &Ty::i32(), span);
        self.engine.unify(&end_ty, &Ty::i32(), span);

        // Set up loop environment with acc and i bound
        let mut loop_env = env.clone();
        loop_env.insert(idx_name.clone(), Scheme::mono(Ty::i32()));
        loop_env.insert(acc_name.clone(), Scheme::mono(init_ty.clone()));

        // The result type is the accumulator type
        let result_ty = init_ty.clone();

        // Build the loop name function type: I32 -> acc_ty -> result_ty
        let loop_name = "_foldRange".to_string();
        let loop_fn_ty = Ty::arrow(Ty::i32(), Ty::arrow(init_ty.clone(), result_ty.clone()));
        loop_env.insert(loop_name.clone(), Scheme::mono(loop_fn_ty));

        // Lower the fold body in the loop environment
        let (hir_body, body_ty) = if is_lambda {
            // Lambda case: lower the lambda body directly (params already bound in loop_env)
            let lambda_body = match lambda {
                Expr::Lambda(_, body, _) => body.as_ref(),
                _ => unreachable!(),
            };
            self.lower_expr(lambda_body, &mut loop_env)
        } else {
            // Named function case: lower as App(App(f, acc), i)
            let (hir_f, f_ty) = self.lower_expr(lambda, &mut loop_env);
            let ret1_ty = self.engine.fresh_var();
            let expected_f = Ty::arrow(init_ty.clone(), Ty::arrow(Ty::i32(), ret1_ty.clone()));
            self.engine.unify(&f_ty, &expected_f, span);
            let app1 = HirExpr::App(
                Box::new(hir_f),
                Box::new(HirExpr::Var(acc_name.clone(), init_ty.clone(), span)),
                Ty::arrow(Ty::i32(), ret1_ty.clone()),
                span,
            );
            let app2 = HirExpr::App(
                Box::new(app1),
                Box::new(HirExpr::Var(idx_name.clone(), Ty::i32(), span)),
                ret1_ty.clone(),
                span,
            );
            (app2, ret1_ty)
        };
        self.engine.unify(&result_ty, &body_ty, span);

        // Build: if i >= end then acc else _foldRange (i + 1) (body)
        // Condition: i >= end
        let cond = HirExpr::BinOp(
            BinOp::Ge,
            Box::new(HirExpr::Var(idx_name.clone(), Ty::i32(), span)),
            Box::new(hir_end),
            Ty::bool(),
            span,
        );

        // Then branch: acc (return the accumulator)
        let then_branch = HirExpr::Var(acc_name.clone(), init_ty.clone(), span);

        // Else branch: _foldRange (i + 1) (body)
        let i_plus_1 = HirExpr::BinOp(
            BinOp::Add,
            Box::new(HirExpr::Var(idx_name.clone(), Ty::i32(), span)),
            Box::new(HirExpr::Lit(HirLit::Int(1), Ty::i32(), span)),
            Ty::i32(),
            span,
        );
        let loop_var = HirExpr::Var(
            loop_name.clone(),
            Ty::arrow(Ty::i32(), Ty::arrow(init_ty.clone(), result_ty.clone())),
            span,
        );
        let app1 = HirExpr::App(
            Box::new(loop_var),
            Box::new(i_plus_1),
            Ty::arrow(init_ty.clone(), result_ty.clone()),
            span,
        );
        let else_branch = HirExpr::App(Box::new(app1), Box::new(hir_body), result_ty.clone(), span);

        let loop_body = HirExpr::If(
            Box::new(cond),
            Box::new(then_branch),
            Box::new(else_branch),
            result_ty.clone(),
            span,
        );

        // Build HirExpr::Loop
        let hir_loop = HirExpr::Loop(
            loop_name,
            vec![(idx_name, hir_start), (acc_name, hir_init)],
            Box::new(loop_body),
            result_ty.clone(),
            span,
        );

        Some((hir_loop, result_ty))
    }

    pub(crate) fn finalize_expr(&mut self, expr: HirExpr) -> HirExpr {
        match expr {
            HirExpr::Lit(lit, ty, span) => HirExpr::Lit(lit, self.finalize_resolve(&ty), span),
            HirExpr::Var(name, ty, span) => {
                let final_ty = self.finalize_resolve(&ty);
                // Trait method dispatch: if this var is a trait method and the
                // type resolves to a concrete type, rewrite to the mangled impl function.
                let resolved_name = self.resolve_trait_method_or_diag(&name, &final_ty, span);
                HirExpr::Var(resolved_name, final_ty, span)
            }
            HirExpr::Tuple(items, ty, span) => HirExpr::Tuple(
                items
                    .into_iter()
                    .map(|item| self.finalize_expr(item))
                    .collect(),
                self.finalize_resolve(&ty),
                span,
            ),
            HirExpr::TupleIndex(base, index, ty, span) => HirExpr::TupleIndex(
                Box::new(self.finalize_expr(*base)),
                index,
                self.finalize_resolve(&ty),
                span,
            ),
            HirExpr::App(func, arg, ty, span) => {
                let final_func = self.finalize_expr(*func);
                let final_arg = self.finalize_expr(*arg);
                let final_ty = self.finalize_resolve(&ty);
                let app = HirExpr::App(
                    Box::new(final_func),
                    Box::new(final_arg),
                    final_ty.clone(),
                    span,
                );
                self.finalize_builtin_extern_call(app, final_ty, span)
            }
            HirExpr::Let(binds, body, ty, span) => HirExpr::Let(
                binds
                    .into_iter()
                    .map(|(name, expr)| (name, self.finalize_expr(expr)))
                    .collect(),
                Box::new(self.finalize_expr(*body)),
                self.finalize_resolve(&ty),
                span,
            ),
            HirExpr::Case(scrutinee, arms, ty, span) => HirExpr::Case(
                Box::new(self.finalize_expr(*scrutinee)),
                arms.into_iter()
                    .map(|arm| HirCaseArm {
                        pattern: self.finalize_pattern(arm.pattern),
                        guard: arm.guard.map(|g| self.finalize_expr(g)),
                        body: self.finalize_expr(arm.body),
                    })
                    .collect(),
                self.finalize_resolve(&ty),
                span,
            ),
            HirExpr::If(cond, then_expr, else_expr, ty, span) => HirExpr::If(
                Box::new(self.finalize_expr(*cond)),
                Box::new(self.finalize_expr(*then_expr)),
                Box::new(self.finalize_expr(*else_expr)),
                self.finalize_resolve(&ty),
                span,
            ),
            HirExpr::BinOp(op, lhs, rhs, ty, span) => {
                let final_lhs = self.finalize_expr(*lhs);
                let final_rhs = self.finalize_expr(*rhs);
                let final_ty = self.finalize_resolve(&ty);
                let lhs_ty = final_lhs.ty().clone();

                // Check if there's a trait instance for this operator on the lhs type.
                // Built-in operator-to-trait mapping: + → Add, - → Sub, * → Mul, / → Div, etc.
                let op_str = op.to_str();
                if let Some(mangled) =
                    self.resolve_binary_operator_trait(op_str, &lhs_ty, final_rhs.ty(), &final_ty)
                {
                    // Rewrite BinOp → App(App(Var(mangled), lhs), rhs)
                    let method_ty =
                        Ty::arrow(lhs_ty, Ty::arrow(final_rhs.ty().clone(), final_ty.clone()));
                    let var_expr = HirExpr::Var(mangled, method_ty, span);
                    let partial_ty = Ty::arrow(final_rhs.ty().clone(), final_ty.clone());
                    let app1 =
                        HirExpr::App(Box::new(var_expr), Box::new(final_lhs), partial_ty, span);
                    HirExpr::App(Box::new(app1), Box::new(final_rhs), final_ty, span)
                } else {
                    HirExpr::BinOp(op, Box::new(final_lhs), Box::new(final_rhs), final_ty, span)
                }
            }
            HirExpr::ConstructorCall(name, tag, args, ty, span) => HirExpr::ConstructorCall(
                name,
                tag,
                args.into_iter()
                    .map(|arg| self.finalize_expr(arg))
                    .collect(),
                self.finalize_resolve(&ty),
                span,
            ),
            HirExpr::FieldAccess(expr, field, ty, span) => HirExpr::FieldAccess(
                Box::new(self.finalize_expr(*expr)),
                field,
                self.finalize_resolve(&ty),
                span,
            ),
            HirExpr::Index(base, index, ty, span) => HirExpr::Index(
                Box::new(self.finalize_expr(*base)),
                Box::new(self.finalize_expr(*index)),
                self.finalize_resolve(&ty),
                span,
            ),
            HirExpr::UnaryNeg(inner, ty, span) => {
                let final_inner = self.finalize_expr(*inner);
                let final_ty = self.finalize_resolve(&ty);
                let inner_ty = final_inner.ty().clone();
                if let Some(mangled) = self.resolve_unary_operator_trait("negate", &inner_ty) {
                    let method_ty = Ty::arrow(inner_ty, final_ty.clone());
                    let var_expr = HirExpr::Var(mangled, method_ty, span);
                    HirExpr::App(Box::new(var_expr), Box::new(final_inner), final_ty, span)
                } else {
                    HirExpr::UnaryNeg(Box::new(final_inner), final_ty, span)
                }
            }
            HirExpr::UnaryNot(inner, ty, span) => HirExpr::UnaryNot(
                Box::new(self.finalize_expr(*inner)),
                self.finalize_resolve(&ty),
                span,
            ),
            HirExpr::UnaryBitNot(inner, ty, span) => {
                let final_inner = self.finalize_expr(*inner);
                let final_ty = self.finalize_resolve(&ty);
                let inner_ty = final_inner.ty().clone();
                if let Some(mangled) = self.resolve_unary_operator_trait("bitnot", &inner_ty) {
                    let method_ty = Ty::arrow(inner_ty, final_ty.clone());
                    let var_expr = HirExpr::Var(mangled, method_ty, span);
                    HirExpr::App(Box::new(var_expr), Box::new(final_inner), final_ty, span)
                } else {
                    HirExpr::UnaryBitNot(Box::new(final_inner), final_ty, span)
                }
            }
            HirExpr::Loop(loop_name, bindings, body, ty, span) => HirExpr::Loop(
                loop_name,
                bindings
                    .into_iter()
                    .map(|(n, e)| (n, self.finalize_expr(e)))
                    .collect(),
                Box::new(self.finalize_expr(*body)),
                self.finalize_resolve(&ty),
                span,
            ),
            HirExpr::BitfieldConstruct(name, fields, ty, span) => HirExpr::BitfieldConstruct(
                name,
                fields
                    .into_iter()
                    .map(|(n, e)| (n, self.finalize_expr(e)))
                    .collect(),
                self.finalize_resolve(&ty),
                span,
            ),
            HirExpr::BitfieldUpdate(name, base, fields, ty, span) => HirExpr::BitfieldUpdate(
                name,
                Box::new(self.finalize_expr(*base)),
                fields
                    .into_iter()
                    .map(|(n, e)| (n, self.finalize_expr(e)))
                    .collect(),
                self.finalize_resolve(&ty),
                span,
            ),
        }
    }

    /// Check if an operator (e.g. "+") has a trait impl for the given operand type.
    /// Returns the mangled function name if found (e.g. "add_Fp64"), None otherwise.
    /// Resolve a record field type from the constructor info.
    /// Given a base type like `Fp64` and a field name like `high`, returns `Some(F32)`.
    pub(crate) fn resolve_record_field_type(&self, base_ty: &Ty, field: &str) -> Option<Ty> {
        let type_name = match base_ty {
            Ty::Con(name) => name.as_str(),
            _ => return None,
        };
        if let Some(con_info) = self.constructors.get(type_name) {
            if let ConstructorFields::Record(fields) = &con_info.fields {
                for (fname, fty) in fields {
                    if fname == field {
                        return Some(fty.clone());
                    }
                }
            }
        }
        None
    }

    pub(crate) fn resolve_binary_operator_trait(
        &self,
        op: &str,
        lhs_ty: &Ty,
        rhs_ty: &Ty,
        result_ty: &Ty,
    ) -> Option<String> {
        let logical_op = match op {
            ">>" => "shr",
            _ => op,
        };
        for trait_info in self.traits.values() {
            // Get the name of the trait's output associated type (typically "Output").
            let output_name = trait_info.associated_types.first();
            for (method_name, _) in &trait_info.methods {
                if method_name == logical_op {
                    for inst in &self.impls {
                        if inst.trait_name.as_deref() == Some(trait_info.name.as_str())
                            && inst.tys.len() == 2
                            && inst.tys[0] == *lhs_ty
                            && inst.tys[1] == *rhs_ty
                            && output_name.is_some_and(|name| {
                                inst.associated_type_bindings
                                    .get(name)
                                    .map(|t| t == result_ty)
                                    .unwrap_or(false)
                            })
                        {
                            if let Some(mangled) = inst.methods.get(logical_op) {
                                return Some(mangled.clone());
                            }
                        }
                    }
                    for inst in &self.builtin_impls {
                        if inst.trait_name == trait_info.name
                            && inst.tys.len() == 2
                            && inst.tys[0] == *lhs_ty
                            && inst.tys[1] == *rhs_ty
                            && output_name.is_some_and(|name| {
                                inst.associated_type_bindings
                                    .get(name)
                                    .map(|t| t == result_ty)
                                    .unwrap_or(false)
                            })
                        {
                            if let Some(lowering) = inst.methods.get(logical_op) {
                                return match lowering {
                                    BuiltinLowering::Intrinsic(name) => Some(name.clone()),
                                    BuiltinLowering::NativeBinOp(_) => None,
                                    BuiltinLowering::NativeUnary(_) => None,
                                };
                            }
                        }
                    }
                }
            }
        }
        None
    }

    pub(crate) fn resolve_unary_operator_trait(&self, op: &str, operand_ty: &Ty) -> Option<String> {
        for trait_info in self.traits.values() {
            for (method_name, _) in &trait_info.methods {
                if method_name == op {
                    for inst in &self.impls {
                        if inst.trait_name.as_deref() == Some(trait_info.name.as_str())
                            && inst.tys.len() == 1
                            && inst.tys[0] == *operand_ty
                        {
                            if let Some(mangled) = inst.methods.get(op) {
                                return Some(mangled.clone());
                            }
                        }
                    }
                    for inst in &self.builtin_impls {
                        if inst.trait_name == trait_info.name
                            && inst.tys.len() == 1
                            && inst.tys[0] == *operand_ty
                        {
                            if let Some(lowering) = inst.methods.get(op) {
                                return match lowering {
                                    BuiltinLowering::Intrinsic(name) => Some(name.clone()),
                                    BuiltinLowering::NativeUnary(_) => None,
                                    BuiltinLowering::NativeBinOp(_) => None,
                                };
                            }
                        }
                    }
                }
            }
        }
        None
    }

    pub(crate) fn finalize_builtin_extern_call(
        &self,
        expr: HirExpr,
        result_ty: Ty,
        span: Span,
    ) -> HirExpr {
        let (head, args) = collect_hir_app_args(&expr);
        let HirExpr::Var(name, _, _) = head else {
            return expr;
        };
        let Some(overloads) = self.builtin_externs.get(name) else {
            return expr;
        };

        let mut full_ty = result_ty.clone();
        for arg in args.iter().rev() {
            full_ty = Ty::arrow(arg.ty().clone(), full_ty);
        }
        let final_full_ty = self.finalize_resolve(&full_ty);

        for overload in overloads {
            if self.finalize_resolve(&overload.ty) == final_full_ty {
                if let BuiltinLowering::Intrinsic(target) = &overload.lowering {
                    return rename_hir_app_head(expr, target, final_full_ty, span);
                }
            }
        }

        expr
    }

    /// Resolve a trait or standalone impl method name to a concrete mangled name,
    /// if the resolved type is concrete and a matching impl exists.
    pub(crate) fn resolve_trait_method_or_diag(
        &mut self,
        name: &str,
        ty: &Ty,
        span: Span,
    ) -> String {
        // Check trait methods
        for trait_info in self.traits.values() {
            for (method_name, _) in &trait_info.methods {
                if method_name == name {
                    let concrete = extract_first_arg_type(ty);
                    if let Some(concrete_ty) = concrete {
                        let mut matches = self.impls.iter().filter(|inst| {
                            inst.trait_name.as_deref() == Some(trait_info.name.as_str())
                                && !inst.tys.is_empty()
                                && inst.tys[0] == concrete_ty
                                && inst.methods.contains_key(name)
                        });
                        if let Some(inst) = matches.next() {
                            if matches.next().is_none() {
                                if let Some(mangled) = inst.methods.get(name) {
                                    return mangled.clone();
                                }
                            }
                        }
                        for inst in &self.impls {
                            if inst.trait_name.as_deref() == Some(trait_info.name.as_str())
                                && inst.tys.len() == 1
                                && inst.tys[0] == concrete_ty
                            {
                                if let Some(mangled) = inst.methods.get(name) {
                                    return mangled.clone();
                                }
                            }
                        }
                        if concrete_ty.free_vars().is_empty()
                            && should_emit_missing_trait_impl_diag(&trait_info.name)
                        {
                            self.engine.diagnostics.push(
                                shadml_diagnostics::Diagnostic::error(format!(
                                    "type `{}` does not implement trait `{}`",
                                    concrete_ty, trait_info.name
                                ))
                                .with_label(shadml_diagnostics::Label::primary(
                                    span,
                                    "missing trait implementation",
                                ))
                                .with_help(format!(
                                    "define `impl {} {} where ...`",
                                    trait_info.name, concrete_ty
                                )),
                            );
                        }
                    }
                }
            }
        }
        // Check standalone impl methods
        let concrete = extract_first_arg_type(ty);
        if let Some(concrete_ty) = concrete {
            for inst in &self.impls {
                if inst.trait_name.is_none() && inst.tys.len() == 1 && inst.tys[0] == concrete_ty {
                    if let Some(mangled) = inst.methods.get(name) {
                        return mangled.clone();
                    }
                }
            }
        }
        name.to_string()
    }
}
