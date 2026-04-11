use shadml_hir::*;
use shadml_parser::parser::*;
use shadml_semantic::helpers::*;
use shadml_span::Span;
use shadml_typechecker::*;

use super::*;
use crate::helpers::*;

impl AstLowering {
    pub(crate) fn lower_fun_decl(
        &mut self,
        name: &str,
        params: &[Pat],
        body: &Expr,
        where_binds: &[LocalBind],
        span: Span,
        comments: Vec<String>,
    ) -> Option<HirFunction> {
        let mut local_env = self.env.clone();
        let predicate_start = self.inferred_predicates.len();

        let mut hir_params = Vec::new();
        let mut param_types = Vec::new();
        let mut param_pattern_binds = Vec::new();
        for (index, pat) in params.iter().enumerate() {
            let ty = self.engine.fresh_var();
            let pname = param_binding_name(pat, index);
            self.bind_pattern(pat, &ty, &mut local_env);
            param_types.push(ty.clone());
            hir_params.push((pname.clone(), ty.clone()));
            if !matches!(pat, Pat::Var(..) | Pat::Wild(..)) {
                let base = HirExpr::Var(pname, ty.clone(), span);
                param_pattern_binds.extend(self.build_pattern_bindings(pat, base, &ty));
            }
        }

        // Unify parameter types with declared type signature BEFORE lowering
        // the body, so that record update expressions can resolve concrete types.
        let ret_ty_var = self.engine.fresh_var();
        let mut fun_ty = ret_ty_var.clone();
        for pt in param_types.iter().rev() {
            fun_ty = Ty::arrow(pt.clone(), fun_ty);
        }
        let active_constraints = if let Some(scheme) = self.env.lookup(name) {
            let declared = self.engine.instantiate_qualified(scheme);
            self.engine.unify(&fun_ty, &declared.ty, span);
            declared.constraints
        } else {
            vec![]
        };

        let body = desugar_where(body, where_binds, span);
        self.active_constraints_stack
            .push(active_constraints.clone());
        let (mut hir_body, body_ty) = self.lower_expr(&body, &mut local_env);
        self.active_constraints_stack.pop();
        if !param_pattern_binds.is_empty() {
            hir_body = HirExpr::Let(
                param_pattern_binds,
                Box::new(hir_body),
                body_ty.clone(),
                span,
            );
        }

        // Resolve inferred predicates before unifying body type with return
        // type. This ensures type variables constrained by trait predicates
        // (e.g., the result type of an infix operator like `+`) are unified
        // with concrete types before we check the body against the declared
        // return type. Without this, AssocProj types with free type variables
        // cannot unify with concrete types.
        let inferred_constraints =
            self.resolve_inferred_predicates(predicate_start, &active_constraints, span);
        shadml_semantic::resolve_assoc_projections_in_subst(
            &mut self.engine.subst,
            &self.impls,
            &self.builtin_impls,
        );
        let body_ty_resolved = body_ty.apply_subst(&self.engine.subst);
        let body_ty_resolved = shadml_semantic::resolve_assoc_projections_with_impls(
            &body_ty_resolved,
            &self.impls,
            &self.builtin_impls,
        );
        self.engine.unify(&body_ty_resolved, &ret_ty_var, span);
        for predicate in inferred_constraints {
            self.engine.diagnostics.push(
                shadml_diagnostics::Diagnostic::error(format!(
                    "missing trait constraint `{}`",
                    format_predicate_local(&predicate)
                ))
                .with_label(shadml_diagnostics::Label::primary(
                    span,
                    "trait use requires a declared constraint",
                ))
                .with_help("add the corresponding constraint to the function signature"),
            );
        }

        // Finalize types
        let final_params: Vec<(String, Ty)> = hir_params
            .into_iter()
            .map(|(n, ty)| (n, self.finalize_resolve(&ty)))
            .collect();
        let return_ty = self.finalize_resolve(&body_ty);
        let body = self.finalize_expr(hir_body);

        Some(HirFunction {
            name: name.to_string(),
            params: final_params,
            return_ty,
            body,
            span,
            comments,
        })
    }

    /// Lower an impl method body into a regular HIR function.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn lower_impl_method(
        &mut self,
        local_name: &str,
        mangled_name: &str,
        params: &[Pat],
        body: &Expr,
        concrete_ty: &Ty,
        span: Span,
        comments: Vec<String>,
        bind_local_name: bool,
    ) -> Option<HirFunction> {
        let mut local_env = self.env.clone();
        if bind_local_name {
            local_env.insert(local_name.to_string(), Scheme::mono(concrete_ty.clone()));
        }

        // Extract parameter types from the concrete method type (which is curried arrows)
        let mut hir_params = Vec::new();
        let mut remaining_ty = concrete_ty.clone();
        for pat in params {
            let param_ty = match &remaining_ty {
                Ty::Arrow(arg, ret) => {
                    let pt = (**arg).clone();
                    remaining_ty = (**ret).clone();
                    pt
                }
                _ => self.engine.fresh_var(),
            };
            let pname = pat_name(pat);
            self.bind_pattern(pat, &param_ty, &mut local_env);
            hir_params.push((pname, param_ty));
        }

        let (hir_body, body_ty) = self.lower_expr(body, &mut local_env);
        self.engine.unify(&body_ty, &remaining_ty, span);

        let final_params: Vec<(String, Ty)> = hir_params
            .into_iter()
            .map(|(n, ty)| (n, self.finalize_resolve(&ty)))
            .collect();
        let return_ty = self.finalize_resolve(&body_ty);
        let body = self.finalize_expr(hir_body);

        Some(HirFunction {
            name: mangled_name.to_string(),
            params: final_params,
            return_ty,
            body,
            span,
            comments,
        })
    }

    /// Lower a standalone impl method — infer types from parameters and body.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn lower_standalone_impl_method(
        &mut self,
        local_name: &str,
        mangled_name: &str,
        params: &[Pat],
        body: &Expr,
        impl_ty: &Ty,
        span: Span,
        comments: Vec<String>,
    ) -> Option<HirFunction> {
        let mut local_env = self.env.clone();
        let mut hir_params = Vec::new();
        let mut fun_ty = self.engine.fresh_var();

        for i in (0..params.len()).rev() {
            let param_ty = if i == 0 {
                impl_ty.clone()
            } else {
                self.engine.fresh_var()
            };
            fun_ty = Ty::arrow(param_ty, fun_ty);
        }
        local_env.insert(local_name.to_string(), Scheme::mono(fun_ty));

        for (i, pat) in params.iter().enumerate() {
            // First parameter gets the impl type; rest are inferred.
            let param_ty = if i == 0 {
                impl_ty.clone()
            } else {
                self.engine.fresh_var()
            };
            let pname = pat_name(pat);
            self.bind_pattern(pat, &param_ty, &mut local_env);
            hir_params.push((pname, param_ty));
        }

        let (hir_body, body_ty) = self.lower_expr(body, &mut local_env);

        let final_params: Vec<(String, Ty)> = hir_params
            .into_iter()
            .map(|(n, ty)| (n, self.finalize_resolve(&ty)))
            .collect();
        let return_ty = self.finalize_resolve(&body_ty);
        let body = self.finalize_expr(hir_body);

        Some(HirFunction {
            name: mangled_name.to_string(),
            params: final_params,
            return_ty,
            body,
            span,
            comments,
        })
    }

    pub(crate) fn lower_entry_point(
        &mut self,
        attributes: &[Attribute],
        name: &str,
        params: &[Pat],
        body: &Expr,
        span: Span,
        comments: Vec<String>,
    ) -> Option<HirEntryPoint> {
        let mut local_env = self.env.clone();
        let predicate_start = self.inferred_predicates.len();

        let mut hir_params = Vec::new();
        let mut param_types = Vec::new();
        let mut param_pattern_binds = Vec::new();
        for (index, pat) in params.iter().enumerate() {
            let ty = self.engine.fresh_var();
            let pname = param_binding_name(pat, index);
            self.bind_pattern(pat, &ty, &mut local_env);
            param_types.push(ty.clone());
            hir_params.push((pname.clone(), ty.clone()));
            if !matches!(pat, Pat::Var(..) | Pat::Wild(..)) {
                let base = HirExpr::Var(pname, ty.clone(), span);
                param_pattern_binds.extend(self.build_pattern_bindings(pat, base, &ty));
            }
        }

        // Unify parameter types with declared type signature BEFORE lowering
        // the body, so that record update expressions can resolve concrete types.
        let ret_ty_var = self.engine.fresh_var();
        let mut fun_ty = ret_ty_var.clone();
        for pt in param_types.iter().rev() {
            fun_ty = Ty::arrow(pt.clone(), fun_ty);
        }

        let active_constraints = if let Some(scheme) = self.env.lookup(name) {
            let declared = self.engine.instantiate_qualified(scheme);
            self.engine.unify(&fun_ty, &declared.ty, span);
            declared.constraints
        } else {
            vec![]
        };

        self.active_constraints_stack
            .push(active_constraints.clone());
        let (mut hir_body, body_ty) = self.lower_expr(body, &mut local_env);
        self.active_constraints_stack.pop();
        if !param_pattern_binds.is_empty() {
            hir_body = HirExpr::Let(
                param_pattern_binds,
                Box::new(hir_body),
                body_ty.clone(),
                span,
            );
        }
        // Resolve inferred predicates before unifying body type with return
        // type (same as in lower_function).
        let inferred_constraints =
            self.resolve_inferred_predicates(predicate_start, &active_constraints, span);
        shadml_semantic::resolve_assoc_projections_in_subst(
            &mut self.engine.subst,
            &self.impls,
            &self.builtin_impls,
        );
        let body_ty_resolved = body_ty.apply_subst(&self.engine.subst);
        let body_ty_resolved = shadml_semantic::resolve_assoc_projections_with_impls(
            &body_ty_resolved,
            &self.impls,
            &self.builtin_impls,
        );
        self.engine.unify(&body_ty_resolved, &ret_ty_var, span);
        for predicate in inferred_constraints {
            self.engine.diagnostics.push(
                shadml_diagnostics::Diagnostic::error(format!(
                    "missing trait constraint `{}`",
                    format_predicate_local(&predicate)
                ))
                .with_label(shadml_diagnostics::Label::primary(
                    span,
                    "trait use requires a declared constraint",
                ))
                .with_help("add the corresponding constraint to the entry-point signature"),
            );
        }

        let final_params: Vec<(String, Ty)> = hir_params
            .into_iter()
            .map(|(n, ty)| (n, self.finalize_resolve(&ty)))
            .collect();
        let return_ty = self.finalize_resolve(&body_ty);
        let body = self.finalize_expr(hir_body);

        let hir_attrs = attributes
            .iter()
            .map(|a| HirAttribute {
                name: a.name.clone(),
                args: a.args.clone(),
            })
            .collect();

        Some(HirEntryPoint {
            name: name.to_string(),
            attributes: hir_attrs,
            params: final_params,
            return_ty,
            body,
            span,
            comments,
        })
    }

    pub(crate) fn instantiate_scheme(&mut self, scheme: &Scheme) -> Ty {
        let qualified = self.engine.instantiate_qualified(scheme);
        self.inferred_predicates
            .extend(qualified.constraints.iter().cloned());
        qualified.ty
    }

    pub(crate) fn active_constraints(&self) -> &[Predicate] {
        self.active_constraints_stack
            .last()
            .map(|constraints| constraints.as_slice())
            .unwrap_or(&[])
    }

    /// Eagerly resolve associated type projections in an operator's return type.
    ///
    /// When an operator like `(+)` returns `a.Output`, the substitution maps
    /// the return type variable to an `AssocProj`. If the trait params are
    /// already concrete, we can resolve the projection immediately.
    ///
    /// We must update the substitution directly rather than calling unify,
    /// because if `subst[ret_ty_var]` is already `AssocProj`, unify would
    /// normalize `ret_ty` to `AssocProj` and the permissive `AssocProj`-vs-concrete
    /// case would accept it without updating the substitution.
    pub(crate) fn eagerly_resolve_assoc_proj_in_ret(&mut self, ret_ty: &Ty, span: Span) {
        let ret_substituted = ret_ty.apply_subst(&self.engine.subst);
        let ret_resolved = shadml_semantic::resolve_assoc_projections_with_impls(
            &ret_substituted,
            &self.impls,
            &self.builtin_impls,
        );
        if ret_resolved != ret_substituted {
            if let Ty::Var(v) = ret_ty {
                self.engine.subst.insert(*v, ret_resolved);
            } else {
                self.engine.unify(ret_ty, &ret_resolved, span);
            }
        }
    }

    pub(crate) fn resolve_inferred_predicates(
        &mut self,
        start: usize,
        active_constraints: &[Predicate],
        span: Span,
    ) -> Vec<Predicate> {
        let pending: Vec<Predicate> = self.inferred_predicates.drain(start..).collect();
        shadml_semantic::resolve_predicates_fixpoint(
            &mut self.engine,
            &self.impls,
            &self.builtin_impls,
            pending,
            active_constraints,
            span,
        )
    }

    pub(crate) fn lower_data_decl(
        &self,
        name: &str,
        type_params: &[String],
        cons: &[ConDecl],
    ) -> HirDataType {
        let mut hir_cons = Vec::new();
        for (tag, con) in cons.iter().enumerate() {
            let fields = match &con.fields {
                ConFields::Empty => vec![],
                ConFields::Positional(tys) => tys
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let ty = self.convert_syntax_type_pure(t);
                        HirFieldDef {
                            name: format!("field{}", i),
                            ty,
                            attributes: vec![],
                        }
                    })
                    .collect(),
                ConFields::Record(fields) => fields
                    .iter()
                    .map(|f| {
                        let ty = self.convert_syntax_type_pure(&f.ty);
                        let attrs = f
                            .attributes
                            .iter()
                            .map(|a| HirAttribute {
                                name: a.name.clone(),
                                args: a.args.clone(),
                            })
                            .collect();
                        HirFieldDef {
                            name: f.name.clone(),
                            ty,
                            attributes: attrs,
                        }
                    })
                    .collect(),
            };
            let resolved_tag = con.discriminant.unwrap_or(tag as i64) as u32;
            hir_cons.push(HirConstructor {
                name: con.name.clone(),
                tag: resolved_tag,
                fields,
            });
        }
        HirDataType {
            name: name.to_string(),
            type_params: type_params.to_vec(),
            constructors: hir_cons,
        }
    }
}
