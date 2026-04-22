
use shadml_diagnostics::{Diagnostic, Label};
use shadml_parser::parser::*;
use shadml_span::Span;
use shadml_typechecker::*;
use crate::helpers::*;
use super::*;

impl SemanticAnalyzer {
    pub(crate) fn register_data_type(
        &mut self,
        name: &str,
        type_params: &[String],
        cons: &[ConDecl],
        _span: Span,
    ) {
        self.data_types.insert(
            name.to_string(),
            DataTypeInfo {
                name: name.to_string(),
                type_params: type_params.to_vec(),
                constructors: vec![],
            },
        );

        let mut type_scope = self.new_type_var_scope(type_params);
        let scheme_vars = scope_vars(&type_scope);
        let result_ty = apply_type_params(name, type_params, &type_scope);
        let mut con_names = Vec::new();

        for (tag, con) in cons.iter().enumerate() {
            let con_ty = match &con.fields {
                ConFields::Empty => result_ty.clone(),
                ConFields::Positional(fields) => {
                    let mut ty = result_ty.clone();
                    for field in fields.iter().rev() {
                        let field_ty = self.convert_syntax_type_with_scope(field, &mut type_scope);
                        ty = Ty::arrow(field_ty, ty);
                    }
                    ty
                }
                ConFields::Record(fields) => {
                    // Record constructor: takes all fields positionally
                    let mut ty = result_ty.clone();
                    for f in fields.iter().rev() {
                        let ft = self.convert_syntax_type_with_scope(&f.ty, &mut type_scope);
                        ty = Ty::arrow(ft, ty);
                    }
                    ty
                }
            };

            let field_info = match &con.fields {
                ConFields::Empty => ConstructorFields::Empty,
                ConFields::Positional(fields) => ConstructorFields::Positional(
                    fields
                        .iter()
                        .map(|f| self.convert_syntax_type_with_scope(f, &mut type_scope))
                        .collect(),
                ),
                ConFields::Record(fields) => ConstructorFields::Record(
                    fields
                        .iter()
                        .map(|f| {
                            (
                                f.name.clone(),
                                self.convert_syntax_type_with_scope(&f.ty, &mut type_scope),
                            )
                        })
                        .collect(),
                ),
            };

            let resolved_tag = con.discriminant.unwrap_or(tag as i64) as u32;
            self.constructors.insert(
                con.name.clone(),
                ConstructorInfo {
                    type_name: name.to_string(),
                    tag: resolved_tag,
                    scheme_vars: scheme_vars.clone(),
                    fields: field_info,
                    result_ty: result_ty.clone(),
                },
            );

            self.env
                .insert(con.name.clone(), Scheme::poly(scheme_vars.clone(), con_ty));
            con_names.push(con.name.clone());
        }

        if let Some(info) = self.data_types.get_mut(name) {
            info.constructors = con_names;
        }
    }

    /// Type-check an entry point.
    /// When `is_render_block` is false (module scope) and the entry carries
    /// `@vertex` or `@fragment`, an error is emitted — render blocks are
    /// required for vertex/fragment shaders.
    pub(crate) fn check_entry_point(
        &mut self,
        name: &str,
        params: &[Pat],
        body: &Expr,
        span: Span,
        attributes: &[Attribute],
        is_render_block: bool,
    ) {
        if !is_render_block {
            let is_vertex_or_fragment =
                attributes.iter().any(|a| a.name == "vertex" || a.name == "fragment");
            if is_vertex_or_fragment {
                self.engine.diagnostics.push(
                    Diagnostic::error(
                        "@vertex and @fragment entry points must be inside a render block",
                    )
                    .with_label(Label::primary(span, "consider wrapping in a `render` block")),
                );
            }
        }
        self.check_function(name, params, body, &[], span);
    }

    pub(crate) fn check_function(
        &mut self,
        name: &str,
        params: &[Pat],
        body: &Expr,
        where_binds: &[LocalBind],
        span: Span,
    ) {
        let mut local_env = self.env.clone();
        let has_explicit_signature = self.env.lookup(name).is_some();
        let mut declared = self
            .env
            .lookup(name)
            .map(|scheme| self.engine.instantiate_qualified(scheme));
        let active_constraints = declared
            .as_ref()
            .map(|qualified| qualified.constraints.clone())
            .unwrap_or_default();

        // Create one parameter type per syntactic parameter.
        let mut param_types = Vec::new();
        for pat in params {
            let ty = self.engine.fresh_var();
            self.bind_pattern(pat, &ty, &mut local_env);
            param_types.push(ty);
        }

        // If there's a declared type, pre-unify parameter types so that
        // concrete type information (e.g. Vec dimensions) is available
        // during body inference for swizzle resolution and field access.
        if let Some(qualified) = &declared {
            let expected_arity = function_arity(&qualified.ty);
            if params.len() != expected_arity {
                self.engine.diagnostics.push(
                    Diagnostic::error(format!(
                        "function `{}` has {} parameter{} but its type signature expects {}",
                        name,
                        params.len(),
                        if params.len() == 1 { "" } else { "s" },
                        expected_arity
                    ))
                    .with_label(Label::primary(
                        span,
                        "function parameters do not match the declared function type",
                    )),
                );
                return;
            }
            let declared_ty = qualified.ty.clone();
            let mut cursor = &declared_ty;
            for param_ty in &param_types {
                if let Ty::Arrow(from, to) = cursor {
                    self.engine.unify(param_ty, from, span);
                    cursor = to;
                } else {
                    break;
                }
            }
        }

        // Infer body type. `where` is desugared to a local `let`.
        let body = desugar_where(body, where_binds, span);
        let predicate_start = self.inferred_predicates.len();
        let body_ty = self.infer_expr(&body, &mut local_env, &active_constraints);

        // Build function type: p1 -> p2 -> ... -> body_ty
        let mut fun_ty = body_ty;
        for param_ty in param_types.into_iter().rev() {
            fun_ty = Ty::arrow(param_ty, fun_ty);
        }
        // Resolve associated type projections (e.g., `a.Output` → `F32`)
        fun_ty = self.apply_subst_resolve(&fun_ty);

        // If there's a declared type, unify with it
        if let Some(qualified) = declared.take() {
            // Resolve inferred predicates before unifying with the declared
            // type. This ensures type variables constrained by trait
            // predicates (e.g., the type of an indexed expression used with
            // an arithmetic operator) are unified with concrete types first.
            // Without this, AssocProj types with free type variables in their
            // trait_params cannot unify with the declared return type.
            let inferred_constraints =
                self.resolve_inferred_predicates(predicate_start, &active_constraints, span);
            // Re-resolve after predicate improvement may have unified type
            // variables with concrete types.
            fun_ty = self.apply_subst_resolve(&fun_ty);
            self.engine.unify(&fun_ty, &qualified.ty, span);
            for predicate in inferred_constraints {
                self.engine.diagnostics.push(
                    Diagnostic::error(format!(
                        "missing trait constraint `{}`",
                        format_predicate(&predicate)
                    ))
                    .with_label(Label::primary(span, "trait use requires a declared constraint"))
                    .with_help(format!(
                        "add a type signature like `{} : {} => ...`",
                        name,
                        format_predicate(&predicate)
                    )),
                );
            }
        } else {
            let inferred_constraints =
                self.resolve_inferred_predicates(predicate_start, &active_constraints, span);
            // Re-resolve after predicate improvement may have updated the substitution
            fun_ty = self.apply_subst_resolve(&fun_ty);
            // Add inferred type
            let scheme = self
                .engine
                .generalize_with_constraints(&self.env, &fun_ty, &inferred_constraints);
            if has_explicit_signature {
                self.env.insert(name.to_string(), scheme);
                return;
            }
            if !scheme.constraints.is_empty() {
                self.engine.diagnostics.push(
                    Diagnostic::error(format!(
                        "top-level binding `{}` requires an explicit constrained type signature",
                        name
                    ))
                    .with_label(Label::primary(
                        span,
                        "constrained top-level binding needs a type signature",
                    ))
                    .with_help(format!("write `{} : {} => ...`", name, format_constraints(&scheme.constraints))),
                );
            }
            self.env.insert(name.to_string(), scheme);
        }
    }

    pub(crate) fn check_impl_method(
        &mut self,
        local_name: &str,
        declared_scheme: &Scheme,
        params: &[Pat],
        body: &Expr,
        span: Span,
        bind_local_name: bool,
    ) {
        let mut local_env = self.env.clone();
        let declared = self.engine.instantiate_qualified(declared_scheme);
        let active_constraints = declared.constraints.clone();
        if bind_local_name {
            local_env.insert(local_name.to_string(), Scheme::mono(declared.ty.clone()));
        }

        let mut param_types = Vec::new();
        for pat in params {
            let ty = self.engine.fresh_var();
            self.bind_pattern(pat, &ty, &mut local_env);
            param_types.push(ty);
        }

        let expected_arity = function_arity(&declared.ty);
        if params.len() != expected_arity {
            self.engine.diagnostics.push(
                Diagnostic::error(format!(
                    "method `{}` has {} parameter{} but its declared type expects {}",
                    local_name,
                    params.len(),
                    if params.len() == 1 { "" } else { "s" },
                    expected_arity
                ))
                .with_label(Label::primary(
                    span,
                    "method parameters do not match the declared function type",
                )),
            );
            return;
        }

        let mut cursor = &declared.ty;
        for param_ty in &param_types {
            if let Ty::Arrow(from, to) = cursor {
                self.engine.unify(param_ty, from, span);
                cursor = to;
            } else {
                break;
            }
        }

        let predicate_start = self.inferred_predicates.len();
        let body_ty = self.infer_expr(body, &mut local_env, &active_constraints);

        let mut fun_ty = body_ty;
        for param_ty in param_types.into_iter().rev() {
            fun_ty = Ty::arrow(param_ty, fun_ty);
        }

        // Resolve inferred predicates before unifying with the declared type,
        // same as in check_function.
        let inferred_constraints =
            self.resolve_inferred_predicates(predicate_start, &active_constraints, span);
        fun_ty = self.apply_subst_resolve(&fun_ty);
        self.engine.unify(&fun_ty, &declared.ty, span);
        for predicate in inferred_constraints {
            self.engine.diagnostics.push(
                Diagnostic::error(format!(
                    "missing trait constraint `{}`",
                    format_predicate(&predicate)
                ))
                .with_label(Label::primary(span, "trait use requires a declared constraint"))
                .with_help("add the corresponding constraint to the method signature"),
            );
        }
    }
}
