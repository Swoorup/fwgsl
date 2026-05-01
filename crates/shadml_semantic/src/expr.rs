use super::*;
use shadml_diagnostics::{Diagnostic, Label};

impl SemanticAnalyzer {
    /// Convenience wrapper: infer without an expected-type hint.
    pub(crate) fn infer_expr(
        &mut self,
        expr: &Expr,
        env: &mut TypeEnv,
        active_constraints: &[Predicate],
    ) -> Ty {
        self.infer_expr_with_hint(expr, env, active_constraints, None)
    }

    /// Infer an expression, optionally guided by an expected type from context.
    ///
    /// When `expected_ty` is `Some(ty)`, the expression is checked against `ty`.
    /// For lambdas, this propagates the expected function type into parameter
    /// types, eliminating the fresh-variable problem that causes unresolved
    /// vector-arithmetic trait constraints inside lambdas passed to known
    /// higher-order functions.
    pub(crate) fn infer_expr_with_hint(
        &mut self,
        expr: &Expr,
        env: &mut TypeEnv,
        active_constraints: &[Predicate],
        expected_ty: Option<&Ty>,
    ) -> Ty {
        let ty = match expr {
            Expr::Lit(lit, _) => self.lit_type(lit),

            Expr::Var(name, span)
            | Expr::Resolved(
                ResolvedName {
                    original_name: name,
                    ..
                },
                span,
            ) => {
                if self.is_internal_impl_method_name(name) {
                    self.engine.diagnostics.push(
                        Diagnostic::error(format!(
                            "Internal impl method `{}` is not accessible from source",
                            name
                        ))
                        .with_label(Label::primary(*span, "compiler-internal symbol"))
                        .with_help("call the source method name or use dot syntax instead"),
                    );
                    return Ty::Error;
                }
                if let Some(scheme) = env.lookup(name) {
                    let qualified = self.engine.instantiate_qualified(scheme);
                    self.inferred_predicates
                        .extend(qualified.constraints.iter().cloned());
                    qualified.ty
                } else {
                    self.engine.diagnostics.push(
                        Diagnostic::error(format!("Unbound variable: {}", name))
                            .with_label(Label::primary(*span, "not in scope"))
                            .with_help(
                                "bind the name in a parameter, let, where, or import declaration",
                            ),
                    );
                    Ty::Error
                }
            }

            Expr::Con(name, span) => {
                if let Some(scheme) = env.lookup(name) {
                    self.engine.instantiate(scheme)
                } else {
                    self.engine.diagnostics.push(
                        Diagnostic::error(format!("Unknown constructor: {}", name))
                            .with_label(Label::primary(*span, "not in scope"))
                            .with_help(
                                "declare it in a data definition before constructing it here",
                            ),
                    );
                    Ty::Error
                }
            }

            Expr::App(func, arg, span) => {
                // Algorithm M: infer the function first, then unify it with
                // (expected_arg -> expected_ret) BEFORE inferring the argument.
                // This lets the expected argument type flow into the argument
                // expression (crucial for lambdas, records, tuples, etc.).
                let func_ty = self.infer_expr(func, env, active_constraints);
                let fresh_arg_ty = self.engine.fresh_var();
                let fresh_ret_ty = self.engine.fresh_var();
                let expected = Ty::arrow(fresh_arg_ty.clone(), fresh_ret_ty.clone());
                self.engine.unify(&func_ty, &expected, *span);

                // The substitution may have already resolved fresh_arg_ty to a
                // concrete type (e.g. when func_ty was a known arrow type).
                // Apply the substitution before using it as a hint so that
                // lambdas receive their *concrete* expected parameter types.
                let arg_hint = fresh_arg_ty.apply_subst(&self.engine.subst);

                let arg_ty =
                    self.infer_expr_with_hint(arg, env, active_constraints, Some(&arg_hint));
                self.engine.unify(&arg_ty, &fresh_arg_ty, *span);
                fresh_ret_ty
            }

            Expr::Infix(lhs, op, rhs, span) => {
                let op_ty = if let Some(scheme) = env.lookup(op) {
                    let qualified = self.engine.instantiate_qualified(scheme);
                    self.inferred_predicates
                        .extend(qualified.constraints.iter().cloned());
                    qualified.ty
                } else if op == ">>" {
                    let a = self.engine.fresh_var();
                    Ty::arrow(a.clone(), Ty::arrow(a.clone(), a))
                } else {
                    self.engine.diagnostics.push(
                        Diagnostic::error(format!("Unknown operator: {}", op))
                            .with_label(Label::primary(*span, "not in scope"))
                            .with_help("operators are regular functions; define one or import it into scope"),
                    );
                    return Ty::Error;
                };
                let lhs_ty = self.infer_expr(lhs, env, active_constraints);
                let rhs_ty = self.infer_expr(rhs, env, active_constraints);
                let ret_ty = self.engine.fresh_var();
                self.engine.unify(
                    &op_ty,
                    &Ty::arrow(lhs_ty, Ty::arrow(rhs_ty, ret_ty.clone())),
                    *span,
                );
                // Eagerly resolve associated type projections in the return type.
                let ret_substituted = ret_ty.apply_subst(&self.engine.subst);
                let ret_resolved = resolve_assoc_projections_with_impls(
                    &ret_substituted,
                    &self.impls,
                    &self.builtin_impls,
                );
                if ret_resolved != ret_substituted {
                    if let Ty::Var(v) = ret_ty {
                        self.engine.subst.insert(v, ret_resolved);
                    } else {
                        self.engine.unify(&ret_ty, &ret_resolved, *span);
                    }
                }
                ret_ty
            }

            Expr::Lambda(pats, body, _span) => {
                let mut local_env = env.clone();
                let mut param_types = Vec::new();
                // Walk the expected function type, peeling off arrows to get
                // concrete parameter types. This avoids fresh vars for params
                // when the lambda is passed to a known higher-order function.
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
                let body_ty = self.infer_expr_with_hint(
                    body,
                    &mut local_env,
                    active_constraints,
                    expected_cursor,
                );
                let mut result = body_ty;
                for pt in param_types.into_iter().rev() {
                    result = Ty::arrow(pt, result);
                }
                result
            }

            Expr::Let(binds, body, _span) => {
                let mut local_env = env.clone();
                for bind in binds {
                    let predicate_start = self.inferred_predicates.len();
                    let ty = self.infer_expr(&bind.expr, &mut local_env, active_constraints);
                    let inferred_constraints = self.resolve_inferred_predicates(
                        predicate_start,
                        active_constraints,
                        bind.expr.span(),
                    );
                    let ty = self.apply_subst_resolve(&ty);
                    let scheme = self.engine.generalize_with_constraints(
                        &local_env,
                        &ty,
                        &inferred_constraints,
                    );
                    self.local_binding_schemes
                        .insert(bind.name_span, scheme.clone());
                    local_env.insert(bind.name.clone(), scheme);
                    resolve_assoc_projections_in_subst(
                        &mut self.engine.subst,
                        &self.impls,
                        &self.builtin_impls,
                    );
                }
                self.infer_expr_with_hint(body, &mut local_env, active_constraints, expected_ty)
            }

            Expr::Case(scrutinee, arms, span) => {
                let scrut_ty = self.infer_expr(scrutinee, env, active_constraints);
                let result_ty = self.engine.fresh_var();
                for (pat, guard, body) in arms {
                    let mut arm_env = env.clone();
                    self.bind_pattern(pat, &scrut_ty, &mut arm_env);
                    if let Some(guard_expr) = guard {
                        let guard_ty =
                            self.infer_expr(guard_expr, &mut arm_env, active_constraints);
                        self.engine.unify(&guard_ty, &Ty::bool(), *span);
                    }
                    let body_ty = self.infer_expr_with_hint(
                        body,
                        &mut arm_env,
                        active_constraints,
                        Some(&result_ty),
                    );
                    self.engine.unify(&result_ty, &body_ty, *span);
                }
                result_ty
            }

            Expr::If(cond, then_expr, else_expr, span) => {
                let cond_ty = self.infer_expr(cond, env, active_constraints);
                self.engine.unify(&cond_ty, &Ty::bool(), *span);
                let result_ty = expected_ty
                    .cloned()
                    .unwrap_or_else(|| self.engine.fresh_var());
                let then_ty =
                    self.infer_expr_with_hint(then_expr, env, active_constraints, Some(&result_ty));
                let else_ty =
                    self.infer_expr_with_hint(else_expr, env, active_constraints, Some(&result_ty));
                self.engine.unify(&then_ty, &else_ty, *span);
                result_ty
            }

            Expr::Paren(inner, _) => {
                self.infer_expr_with_hint(inner, env, active_constraints, expected_ty)
            }

            Expr::Tuple(elems, _span) => {
                let tys: Vec<Ty> = elems
                    .iter()
                    .map(|e| self.infer_expr(e, env, active_constraints))
                    .collect();
                if tys.is_empty() {
                    Ty::unit()
                } else {
                    Ty::Tuple(tys)
                }
            }

            Expr::Record(name, fields, span) => {
                if let Some(con_name) = name {
                    if let Some(con_info) = self
                        .constructors
                        .get(con_name)
                        .cloned()
                        .map(|info| info.instantiate(&mut self.engine))
                    {
                        if let ConstructorFields::Record(con_fields) = &con_info.fields {
                            for (field_name, field_expr) in fields {
                                let val_ty = self.infer_expr(field_expr, env, active_constraints);
                                if let Some((_, expected_ty)) =
                                    con_fields.iter().find(|(n, _)| n == field_name)
                                {
                                    self.engine.unify(&val_ty, expected_ty, *span);
                                }
                            }
                        } else {
                            for (_, expr) in fields {
                                self.infer_expr(expr, env, active_constraints);
                            }
                        }
                        con_info.result_ty.clone()
                    } else {
                        for (_, expr) in fields {
                            self.infer_expr(expr, env, active_constraints);
                        }
                        Ty::Con(con_name.clone())
                    }
                } else {
                    for (_, expr) in fields {
                        self.infer_expr(expr, env, active_constraints);
                    }
                    self.engine.fresh_var()
                }
            }

            Expr::FieldAccess(expr, field, span) => {
                let base_ty = self.infer_expr(expr, env, active_constraints);
                let base_ty = self.engine.finalize(&base_ty);

                if is_swizzle(field) {
                    if let Some((n, scalar)) = extract_vec_type(&base_ty) {
                        let swizzle_len = field.len();
                        if validate_swizzle(field, n) {
                            if swizzle_len == 1 {
                                return scalar;
                            } else {
                                return Ty::app(
                                    Ty::app(
                                        Ty::Con(ty_name::VEC.into()),
                                        Ty::Nat(swizzle_len as u64),
                                    ),
                                    scalar,
                                );
                            }
                        }
                    }

                    if field.len() == 1 {
                        if let Some((rows, cols, scalar)) = extract_mat_type(&base_ty) {
                            let col_index = swizzle_index(field.chars().next().unwrap());
                            if col_index < cols as usize {
                                return Ty::app(
                                    Ty::app(Ty::Con(ty_name::VEC.into()), Ty::Nat(rows as u64)),
                                    scalar,
                                );
                            } else {
                                self.engine.diagnostics.push(
                                    Diagnostic::error(format!(
                                        "column index '{}' out of bounds for mat{}{}",
                                        field, rows, cols
                                    ))
                                    .with_label(
                                        Label::primary(*span, "out of bounds column access"),
                                    ),
                                );
                                return Ty::Error;
                            }
                        }
                    }
                }

                if let Some(name) = resolve_dot_call_target(env, &self.impls, field, &base_ty) {
                    if let Some(scheme) = env.lookup(&name) {
                        let qualified = self.engine.instantiate_qualified(scheme);
                        self.inferred_predicates
                            .extend(qualified.constraints.iter().cloned());
                        let func_ty = qualified.ty;
                        let ret_ty = self.engine.fresh_var();
                        let expected = Ty::arrow(base_ty.clone(), ret_ty.clone());
                        self.engine.unify(&func_ty, &expected, *span);
                        return ret_ty;
                    }
                }

                if let Ty::Con(ref type_name) = base_ty {
                    let is_known_record = self
                        .constructors
                        .get(type_name.as_str())
                        .is_some_and(|c| matches!(&c.fields, ConstructorFields::Record(_)));
                    let is_known_bitfield =
                        self.bitfield_field_names.contains_key(type_name.as_str());

                    if is_known_record {
                        if let Some(c) = self.constructors.get(type_name.as_str()) {
                            if let ConstructorFields::Record(fields) = &c.fields {
                                if fields.iter().any(|(n, _)| n == field) {
                                    return fields
                                        .iter()
                                        .find(|(n, _)| n == field)
                                        .map(|(_, ty)| ty.clone())
                                        .unwrap();
                                }
                            }
                        }
                        self.engine.diagnostics.push(
                            Diagnostic::error(format!(
                                "no method or field `{}` on type `{}`",
                                field, type_name
                            ))
                            .with_label(Label::primary(*span, "unknown member")),
                        );
                    } else if is_known_bitfield {
                        let bf_fields = self.bitfield_field_names.get(type_name.as_str()).unwrap();
                        if !bf_fields.iter().any(|n| n == field) {
                            self.engine.diagnostics.push(
                                Diagnostic::error(format!(
                                    "no method or field `{}` on type `{}`",
                                    field, type_name
                                ))
                                .with_label(Label::primary(*span, "unknown member")),
                            );
                        }
                    } else {
                        self.engine.diagnostics.push(
                            Diagnostic::error(format!(
                                "no method or field `{}` on type `{}`",
                                field, type_name
                            ))
                            .with_label(Label::primary(*span, "unknown member")),
                        );
                    }
                }

                self.engine.fresh_var()
            }

            Expr::Index(base, index, _span) => {
                let base_ty = self.infer_expr(base, env, active_constraints);
                let base_ty = self.engine.finalize(&base_ty);
                let _ = self.infer_expr(index, env, active_constraints);

                if let Some((rows, _cols, scalar)) = extract_mat_type(&base_ty) {
                    return Ty::app(
                        Ty::app(Ty::Con(ty_name::VEC.into()), Ty::Nat(rows as u64)),
                        scalar,
                    );
                }

                if let Some((_, elem_ty)) = extract_vec_type(&base_ty) {
                    return elem_ty;
                }

                if let Some((_, elem_ty)) = shadml_typechecker::extract_tensor_type(&base_ty) {
                    return elem_ty;
                }

                self.engine.fresh_var()
            }

            Expr::VecLit(elems, span) => {
                if elems.is_empty() {
                    self.engine.diagnostics.push(
                        Diagnostic::error("Empty vec literal")
                            .with_label(Label::primary(*span, "needs at least 2 elements"))
                            .with_help(
                                "add vector elements so the scalar type and arity can be inferred",
                            ),
                    );
                    return Ty::Error;
                }

                let scalar_ty = self.engine.fresh_var();
                let mut total_components: u64 = 0;

                for elem in elems {
                    let elem_ty = self.infer_expr(elem, env, active_constraints);
                    let elem_ty = self.engine.finalize(&elem_ty);

                    if let Some((n, inner_scalar)) = extract_vec_type(&elem_ty) {
                        total_components += n as u64;
                        self.engine.unify(&scalar_ty, &inner_scalar, *span);
                    } else {
                        total_components += 1;
                        self.engine.unify(&scalar_ty, &elem_ty, *span);
                    }
                }

                if !(2..=4).contains(&total_components) {
                    self.engine.diagnostics.push(
                        Diagnostic::error(format!(
                            "Vec literal has {} components, expected 2, 3, or 4",
                            total_components
                        ))
                        .with_label(Label::primary(*span, "invalid component count"))
                        .with_help("WGSL vectors must have exactly 2, 3, or 4 scalar components"),
                    );
                    return Ty::Error;
                }

                Ty::app(
                    Ty::app(Ty::Con(ty_name::VEC.into()), Ty::Nat(total_components)),
                    scalar_ty,
                )
            }

            Expr::OpSection(op, span) => {
                if let Some(scheme) = env.lookup(op) {
                    self.engine.instantiate(scheme)
                } else {
                    self.engine.diagnostics.push(
                        Diagnostic::error(format!("Unknown operator: {}", op))
                            .with_label(Label::primary(*span, "not in scope"))
                            .with_help("operators are regular functions; define one or import it into scope"),
                    );
                    Ty::Error
                }
            }

            Expr::Neg(inner, _span) => {
                self.infer_expr_with_hint(inner, env, active_constraints, expected_ty)
            }

            Expr::Not(inner, span) => {
                let inner_ty = self.infer_expr(inner, env, active_constraints);
                let bool_ty = Ty::bool();
                self.engine.unify(&inner_ty, &bool_ty, *span);
                bool_ty
            }

            Expr::BitNot(inner, _span) => {
                self.infer_expr_with_hint(inner, env, active_constraints, expected_ty)
            }

            Expr::Do(stmts, _span) => {
                let mut local_env = env.clone();
                let mut last_ty = Ty::unit();
                for stmt in stmts {
                    match stmt {
                        DoStmt::Expr(expr, _) => {
                            last_ty = self.infer_expr(expr, &mut local_env, active_constraints);
                        }
                        DoStmt::Bind(bind) => {
                            let ty =
                                self.infer_expr(&bind.expr, &mut local_env, active_constraints);
                            let inner_ty = self.engine.fresh_var();
                            let scheme = Scheme::mono(inner_ty);
                            self.local_binding_schemes
                                .insert(bind.name_span, scheme.clone());
                            local_env.insert(bind.name.clone(), scheme);
                            last_ty = ty;
                        }
                        DoStmt::Let(bind) => {
                            let ty =
                                self.infer_expr(&bind.expr, &mut local_env, active_constraints);
                            let scheme = Scheme::mono(ty);
                            self.local_binding_schemes
                                .insert(bind.name_span, scheme.clone());
                            local_env.insert(bind.name.clone(), scheme);
                        }
                    }
                }
                last_ty
            }

            Expr::Loop(loop_name, bindings, body, span) => {
                let mut loop_env = env.clone();
                let mut binding_tys = Vec::new();
                for bind in bindings {
                    let init_ty = self.infer_expr(&bind.expr, env, active_constraints);
                    let scheme = Scheme::mono(init_ty.clone());
                    self.local_binding_schemes
                        .insert(bind.name_span, scheme.clone());
                    loop_env.insert(bind.name.clone(), scheme);
                    binding_tys.push(init_ty);
                }
                let result_ty = expected_ty
                    .cloned()
                    .unwrap_or_else(|| self.engine.fresh_var());
                let loop_fn_ty = binding_tys.iter().rev().fold(result_ty.clone(), |acc, ty| {
                    Ty::Arrow(Box::new(ty.clone()), Box::new(acc))
                });
                loop_env.insert(loop_name.clone(), Scheme::mono(loop_fn_ty));
                let body_ty = self.infer_expr_with_hint(
                    body,
                    &mut loop_env,
                    active_constraints,
                    Some(&result_ty),
                );
                self.engine.unify(&result_ty, &body_ty, *span);
                result_ty
            }

            Expr::RecordUpdate(base, fields, _span) => {
                let base_ty = self.infer_expr(base, env, active_constraints);
                for (_, expr) in fields {
                    self.infer_expr(expr, env, active_constraints);
                }
                base_ty
            }

            Expr::Qualified(_, _, _) => {
                panic!("Expr::Qualified should have been renamed by the Renamer")
            }
        };
        self.expr_types.insert(expr.span(), ty.clone());
        ty
    }

    pub(crate) fn lit_type(&self, lit: &Lit) -> Ty {
        match lit {
            Lit::Int(_) => Ty::i32(),
            Lit::UInt(_) => Ty::u32(),
            Lit::Float(_) => Ty::f32(),
            Lit::String(_) => Ty::Con(ty_name::STRING.into()),
            Lit::Char(_) => Ty::Con("Char".into()),
        }
    }
}
