use std::collections::HashMap;

use super::*;
use shadml_diagnostics::{Diagnostic, Label};
use shadml_span::Span;

impl SemanticAnalyzer {
    pub(crate) fn convert_syntax_type(&mut self, ty: &Type) -> Scheme {
        let mut scope = HashMap::new();
        let ty = self.convert_syntax_type_with_scope(ty, &mut scope);
        Scheme::poly(scope_vars(&scope), ty)
    }

    pub(crate) fn convert_syntax_type_sig(
        &mut self,
        constraints: &[TypeConstraint],
        ty: &Type,
    ) -> Scheme {
        let mut scope = HashMap::new();
        let predicates = constraints
            .iter()
            .map(|constraint| Predicate {
                trait_name: constraint.trait_name.clone(),
                tys: constraint
                    .tys
                    .iter()
                    .map(|ty| self.convert_syntax_type_with_scope(ty, &mut scope))
                    .collect(),
            })
            .collect();

        // Build constraint contexts from constraints that have associated types,
        // so Type::Proj in the type body (e.g., `a.Output` in
        // `Add a b => a -> b -> a.Output`) can be properly resolved.
        let constraint_contexts: Vec<(String, Vec<Ty>)> = constraints
            .iter()
            .filter_map(|constraint| {
                let trait_info = self.traits.get(&constraint.trait_name)?;
                if trait_info.associated_types.is_empty() {
                    return None;
                }
                let trait_params: Vec<Ty> = constraint
                    .tys
                    .iter()
                    .map(|ty| self.convert_syntax_type_with_scope(ty, &mut scope))
                    .collect();
                Some((constraint.trait_name.clone(), trait_params))
            })
            .collect();
        let ty = self.convert_syntax_type_with_scope_assoc(ty, &mut scope, &constraint_contexts);
        Scheme::poly_with_constraints(predicates, scope_vars(&scope), ty)
    }

    pub(crate) fn convert_syntax_type_with_scope(
        &mut self,
        ty: &Type,
        scope: &mut HashMap<String, TyVarId>,
    ) -> Ty {
        self.convert_syntax_type_with_scope_assoc(ty, scope, &[])
    }

    pub(crate) fn convert_syntax_type_with_scope_assoc(
        &mut self,
        ty: &Type,
        scope: &mut HashMap<String, TyVarId>,
        constraint_contexts: &[(String, Vec<Ty>)],
    ) -> Ty {
        let ty = match ty {
            Type::Con(name, span) => {
                if let Some(expanded) = self.type_aliases.get(name).cloned() {
                    return expanded;
                }
                if !self.is_known_type_constructor(name) {
                    self.engine.diagnostics.push(
                        Diagnostic::error(format!("Unknown type '{}'", name))
                            .with_label(Label::primary(*span, "unknown type"))
                            .with_help(
                                "declare a `data`, `bitfield`, or `alias` before using this type name",
                            ),
                    );
                    return Ty::Error;
                }
                Ty::Con(name.clone())
            }
            Type::Var(name, _) => Ty::Var(
                *scope
                    .entry(name.clone())
                    .or_insert_with(|| fresh_var_id(&mut self.engine)),
            ),
            Type::Proj(_base, name, span) => {
                // Collect all constraint contexts that have this associated type name
                let mut matches: Vec<(String, Vec<Ty>)> = Vec::new();
                for (tn, tp) in constraint_contexts.iter() {
                    if let Some(trait_info) = self.traits.get(tn) {
                        if trait_info.associated_types.iter().any(|n| n == name) {
                            matches.push((tn.clone(), tp.clone()));
                        }
                    }
                }
                match matches.len() {
                    0 => {
                        // No constraint context has this associated type.
                        // Try global trait search as fallback.
                        let base_ty = self.convert_syntax_type_with_scope_assoc(
                            _base,
                            scope,
                            constraint_contexts,
                        );
                        if let Some((trait_name, trait_params)) =
                            self.find_assoc_type_context(&base_ty, name)
                        {
                            Ty::AssocProj {
                                trait_params,
                                name: name.clone(),
                                trait_name,
                            }
                        } else {
                            self.engine.diagnostics.push(
                                Diagnostic::error(format!(
                                    "cannot determine which trait `.{name}` refers to"
                                ))
                                .with_label(Label::primary(*span, "associated type projection"))
                                .with_help("add a trait constraint (e.g., `Add a b =>`) to identify which trait's associated type is meant"),
                            );
                            Ty::Error
                        }
                    }
                    1 => Ty::AssocProj {
                        trait_params: matches[0].1.clone(),
                        name: name.clone(),
                        trait_name: matches[0].0.clone(),
                    },
                    _ => {
                        let trait_names: Vec<&str> =
                            matches.iter().map(|(tn, _)| tn.as_str()).collect();
                        self.engine.diagnostics.push(
                            Diagnostic::error(format!(
                                "ambiguous associated type `.{name}` — found in traits: {}",
                                trait_names.join(", ")
                            ))
                            .with_label(Label::primary(*span, "ambiguous projection"))
                            .with_help("use qualified syntax to disambiguate"),
                        );
                        Ty::Error
                    }
                }
            }
            Type::Nat(n, _) => Ty::Nat(*n),
            Type::Arrow(a, b, _) => {
                let a = self.convert_syntax_type_with_scope_assoc(a, scope, constraint_contexts);
                let b = self.convert_syntax_type_with_scope_assoc(b, scope, constraint_contexts);
                Ty::arrow(a, b)
            }
            Type::App(f, a, _) => {
                let f = self.convert_syntax_type_with_scope_assoc(f, scope, constraint_contexts);
                let a = self.convert_syntax_type_with_scope_assoc(a, scope, constraint_contexts);
                Ty::app(f, a)
            }
            Type::Paren(inner, _) => {
                self.convert_syntax_type_with_scope_assoc(inner, scope, constraint_contexts)
            }
            Type::Tuple(elems, _) => {
                if elems.is_empty() {
                    Ty::unit()
                } else {
                    Ty::Tuple(
                        elems
                            .iter()
                            .map(|e| {
                                self.convert_syntax_type_with_scope_assoc(
                                    e,
                                    scope,
                                    constraint_contexts,
                                )
                            })
                            .collect(),
                    )
                }
            }
            Type::Unit(_) => Ty::unit(),
            Type::Self_(span) => {
                // `Self` resolves to the first type parameter of the enclosing trait.
                // When used as `Self.Output`, the Type::Proj arm handles the projection;
                // the base `Self` is resolved here.
                if let Some((_, trait_params)) = constraint_contexts.first() {
                    if let Some(first_param) = trait_params.first() {
                        first_param.clone()
                    } else {
                        self.engine.diagnostics.push(
                            Diagnostic::error("`Self` used in trait with no type parameters")
                                .with_label(Label::primary(*span, "`Self` here")),
                        );
                        Ty::Error
                    }
                } else {
                    self.engine.diagnostics.push(
                        Diagnostic::error("`Self` is only valid inside trait bodies")
                            .with_label(Label::primary(*span, "`Self` outside trait context")),
                    );
                    Ty::Error
                }
            }
        };
        normalize_type_aliases(&ty)
    }

    fn is_known_type_constructor(&self, name: &str) -> bool {
        matches!(name, ty_name::UNIT | ty_name::UNIFORM | ty_name::STORAGE)
            || self.data_types.contains_key(name)
            || self.builtin_types.contains_key(name)
            || self.bitfield_field_names.contains_key(name)
            || self.type_aliases.contains_key(name)
    }

    /// Check whether a name is a compiler-internal mangled impl method name.
    /// Uses a pre-built HashSet for O(1) lookup instead of scanning all impls.
    pub fn is_internal_impl_method_name(&self, name: &str) -> bool {
        self.impl_method_names.contains(name)
    }

    /// Find a trait that has an associated type with the given name, and
    /// return the trait name and parameter types. Used when Type::Proj
    /// is encountered outside a trait body (e.g., in type signatures).
    fn find_assoc_type_context(&self, base_ty: &Ty, assoc_name: &str) -> Option<(String, Vec<Ty>)> {
        for (trait_name, trait_info) in &self.traits {
            if trait_info.associated_types.iter().any(|n| n == assoc_name) {
                // Found a trait with this associated type name.
                // Build the trait_params: the first param is base_ty,
                // and the rest are filled with base_ty as a
                // placeholder. The type checker will constrain these
                // through unification during inference.
                let mut trait_params = vec![base_ty.clone()];
                for _ in 1..trait_info.vars.len() {
                    trait_params.push(base_ty.clone());
                }
                return Some((trait_name.clone(), trait_params));
            }
        }
        None
    }

    pub(crate) fn standalone_impl_method_scheme(&mut self, impl_ty: &Ty, arity: usize) -> Scheme {
        let mut poly_vars = Vec::new();
        let ret_var = self.engine.fresh_var();
        if let Ty::Var(id) = ret_var {
            poly_vars.push(id);
        }
        let mut result_ty = ret_var;
        for i in (0..arity).rev() {
            let param_ty = if i == 0 {
                impl_ty.clone()
            } else {
                let v = self.engine.fresh_var();
                if let Ty::Var(id) = v {
                    poly_vars.push(id);
                }
                v
            };
            result_ty = Ty::arrow(param_ty, result_ty);
        }
        Scheme::poly(poly_vars, result_ty)
    }

    pub(crate) fn new_type_var_scope(&mut self, names: &[String]) -> HashMap<String, TyVarId> {
        names
            .iter()
            .map(|name| (name.clone(), fresh_var_id(&mut self.engine)))
            .collect()
    }

    pub(crate) fn resolve_inferred_predicates(
        &mut self,
        start: usize,
        active_constraints: &[Predicate],
        span: Span,
    ) -> Vec<Predicate> {
        let pending: Vec<Predicate> = self.inferred_predicates.drain(start..).collect();
        resolve_predicates_fixpoint(
            &mut self.engine,
            &self.impls,
            &self.builtin_impls,
            pending,
            active_constraints,
            span,
        )
    }

    /// Apply substitution and resolve any AssocProj nodes in a type.
    pub(crate) fn apply_subst_resolve(&self, ty: &Ty) -> Ty {
        let substituted = ty.apply_subst(&self.engine.subst);
        resolve_assoc_projections_with_impls(&substituted, &self.impls, &self.builtin_impls)
    }
}
