use std::collections::HashMap;

use shadml_parser::parser::*;
use shadml_semantic::helpers::*;
use shadml_typechecker::*;

use super::*;

impl AstLowering {
    pub(crate) fn convert_syntax_type_scheme(&mut self, ty: &Type) -> Scheme {
        let mut scope = HashMap::new();
        let ty = self.convert_syntax_type_with_scope(ty, &mut scope);
        Scheme::poly(scope_vars(&scope), ty)
    }

    pub(crate) fn convert_syntax_type_sig_scheme(
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

        // Build constraint info for Type::Proj resolution
        // Collect trait names with associated types first (before mutable borrows)
        let traits_with_assoc: Vec<(String, bool)> = constraints
            .iter()
            .map(|constraint| {
                let has_assoc = self
                    .traits
                    .get(&constraint.trait_name)
                    .map(|info| !info.associated_types.is_empty())
                    .unwrap_or(false);
                (constraint.trait_name.clone(), has_assoc)
            })
            .collect();

        let constraint_traits: Vec<(String, Vec<Ty>)> = constraints
            .iter()
            .zip(traits_with_assoc.iter())
            .filter_map(|(constraint, (trait_name, has_assoc))| {
                if !has_assoc {
                    return None;
                }
                let trait_tys: Vec<Ty> = constraint
                    .tys
                    .iter()
                    .map(|ty| self.convert_syntax_type_with_scope(ty, &mut scope))
                    .collect();
                Some((trait_name.clone(), trait_tys))
            })
            .collect();

        let ty = if constraint_traits.is_empty() {
            self.convert_syntax_type_with_scope(ty, &mut scope)
        } else {
            self.convert_syntax_type_with_scope_assoc(ty, &mut scope, &constraint_traits)
        };
        Scheme::poly_with_constraints(predicates, scope_vars(&scope), ty)
    }

    pub(crate) fn convert_syntax_type_with_scope(
        &mut self,
        ty: &Type,
        scope: &mut HashMap<String, TyVarId>,
    ) -> Ty {
        self.convert_syntax_type_with_scope_assoc(ty, scope, &[])
    }

    /// Convert a syntax type with associated type context from constraints.
    /// `constraint_traits` is a list of (trait_name, trait_param_types) for
    /// constraints that have associated types.
    pub(crate) fn convert_syntax_type_with_scope_assoc(
        &mut self,
        ty: &Type,
        scope: &mut HashMap<String, TyVarId>,
        constraint_traits: &[(String, Vec<Ty>)],
    ) -> Ty {
        let ty = match ty {
            Type::Con(name, _) => {
                if let Some(expanded) = self.type_aliases.get(name).cloned() {
                    return expanded;
                }
                Ty::Con(name.clone())
            }
            Type::Var(name, _) => Ty::Var(
                *scope
                    .entry(name.clone())
                    .or_insert_with(|| fresh_var_id(&mut self.engine)),
            ),
            Type::Self_(_) => {
                // Standalone `Self` is invalid — only `Self.Output` is valid
                // and handled in the Type::Proj arm. This arm is reached
                // when `Self` appears without a projection.
                Ty::Error
            }
            Type::Proj(_base, name, span) => {
                // Self.Output: search constraint traits for a matching associated type
                if matches!(_base.as_ref(), Type::Self_(_)) {
                    if let Some((trait_name, trait_params)) =
                        constraint_traits.iter().find_map(|(tn, tp)| {
                            let trait_info = self.traits.get(tn)?;
                            if trait_info.associated_types.iter().any(|n| n == name) {
                                Some((tn.clone(), tp.clone()))
                            } else {
                                None
                            }
                        })
                    {
                        return Ty::AssocProj {
                            trait_params,
                            name: name.clone(),
                            trait_name,
                        };
                    }
                    // Self.Name outside a trait body or with unknown associated type —
                    // semantic analysis already reported errors, produce a placeholder.
                    return Ty::Error;
                }
                // a.Output: find the matching constraint trait for this associated type
                if let Some((trait_name, trait_params)) =
                    constraint_traits.iter().find_map(|(tn, tp)| {
                        let trait_info = self.traits.get(tn)?;
                        if trait_info.associated_types.iter().any(|n| n == name) {
                            Some((tn.clone(), tp.clone()))
                        } else {
                            None
                        }
                    })
                {
                    Ty::AssocProj {
                        trait_params,
                        name: name.clone(),
                        trait_name,
                    }
                } else {
                    self.engine.diagnostics.push(
                        shadml_diagnostics::Diagnostic::error(format!(
                            "cannot determine which trait `.{name}` refers to"
                        ))
                        .with_label(shadml_diagnostics::Label::primary(*span, "associated type projection"))
                        .with_help("add a trait constraint (e.g., `Add a b =>`) to identify which trait's associated type is meant"),
                    );
                    Ty::Error
                }
            }
            Type::Nat(n, _) => Ty::Nat(*n),
            Type::Arrow(a, b, _) => {
                let a = self.convert_syntax_type_with_scope_assoc(a, scope, constraint_traits);
                let b = self.convert_syntax_type_with_scope_assoc(b, scope, constraint_traits);
                Ty::arrow(a, b)
            }
            Type::App(f, a, _) => {
                let f = self.convert_syntax_type_with_scope_assoc(f, scope, constraint_traits);
                let a = self.convert_syntax_type_with_scope_assoc(a, scope, constraint_traits);
                Ty::app(f, a)
            }
            Type::Paren(inner, _) => {
                self.convert_syntax_type_with_scope_assoc(inner, scope, constraint_traits)
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
                                    constraint_traits,
                                )
                            })
                            .collect(),
                    )
                }
            }
            Type::Unit(_) => Ty::unit(),
        };
        normalize_type_aliases(&ty)
    }

    pub(crate) fn new_type_var_scope(&mut self, names: &[String]) -> HashMap<String, TyVarId> {
        names
            .iter()
            .map(|name| (name.clone(), fresh_var_id(&mut self.engine)))
            .collect()
    }

    /// Pure version that doesn't need &mut self (no fresh vars for type vars).
    pub(crate) fn convert_syntax_type_pure(&self, ty: &Type) -> Ty {
        let ty = match ty {
            Type::Con(name, _) => {
                if let Some(expanded) = self.type_aliases.get(name) {
                    return expanded.clone();
                }
                Ty::Con(name.clone())
            }
            Type::Var(name, _) => Ty::Con(name.clone()),
            Type::Self_(_) => Ty::Error,
            Type::Proj(_base, name, _) => Ty::Con(name.clone()),
            Type::Nat(n, _) => Ty::Nat(*n),
            Type::Arrow(a, b, _) => {
                let a = self.convert_syntax_type_pure(a);
                let b = self.convert_syntax_type_pure(b);
                Ty::arrow(a, b)
            }
            Type::App(f, a, _) => {
                let f = self.convert_syntax_type_pure(f);
                let a = self.convert_syntax_type_pure(a);
                Ty::app(f, a)
            }
            Type::Paren(inner, _) => self.convert_syntax_type_pure(inner),
            Type::Tuple(elems, _) => {
                if elems.is_empty() {
                    Ty::unit()
                } else {
                    Ty::Tuple(
                        elems
                            .iter()
                            .map(|e| self.convert_syntax_type_pure(e))
                            .collect(),
                    )
                }
            }
            Type::Unit(_) => Ty::unit(),
        };
        normalize_type_aliases(&ty)
    }
}
