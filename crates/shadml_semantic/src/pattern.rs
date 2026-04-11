use super::*;
use shadml_diagnostics::{Diagnostic, Label};

impl SemanticAnalyzer {
    pub(crate) fn bind_pattern(&mut self, pat: &Pat, ty: &Ty, env: &mut TypeEnv) {
        match pat {
            Pat::Var(name, _) => {
                env.insert(name.clone(), Scheme::mono(ty.clone()));
            }
            Pat::Wild(_) => {}
            Pat::Con(name, sub_pats, span) => {
                if let Some(con_info) = self
                    .constructors
                    .get(name)
                    .cloned()
                    .map(|info| info.instantiate(&mut self.engine))
                {
                    // Unify result type
                    self.engine.unify(ty, &con_info.result_ty, *span);
                    // Bind sub-patterns
                    match &con_info.fields {
                        ConstructorFields::Positional(field_tys) => {
                            for (pat, field_ty) in sub_pats.iter().zip(field_tys.iter()) {
                                self.bind_pattern(pat, field_ty, env);
                            }
                        }
                        ConstructorFields::Empty => {}
                        ConstructorFields::Record(fields) => {
                            for (pat, (_, field_ty)) in sub_pats.iter().zip(fields.iter()) {
                                self.bind_pattern(pat, field_ty, env);
                            }
                        }
                    }
                } else {
                    self.engine.diagnostics.push(
                        Diagnostic::error(format!("Unknown constructor: {}", name))
                            .with_label(Label::primary(*span, "not found"))
                            .with_help(
                                "declare it in a data definition before using it in a pattern",
                            ),
                    );
                }
            }
            Pat::Lit(lit, span) => {
                let lit_ty = match lit {
                    // Integer literals in patterns should be polymorphic over
                    // numeric types (I32, U32) — use a fresh var so the
                    // scrutinee type drives the unification.
                    Lit::Int(_) => self.engine.fresh_var(),
                    _ => self.lit_type(lit),
                };
                self.engine.unify(ty, &lit_ty, *span);
            }
            Pat::Paren(inner, _) => self.bind_pattern(inner, ty, env),
            Pat::Tuple(pats, span) => {
                let elem_tys: Vec<Ty> = pats.iter().map(|_| self.engine.fresh_var()).collect();
                let tuple_ty = if elem_tys.is_empty() {
                    Ty::unit()
                } else {
                    Ty::Tuple(elem_tys.clone())
                };
                self.engine.unify(ty, &tuple_ty, *span);
                for (pat, elem_ty) in pats.iter().zip(elem_tys.iter()) {
                    self.bind_pattern(pat, elem_ty, env);
                }
            }
            Pat::Record(con_name, fields, _, span) => {
                if let Some(con_info) = self
                    .constructors
                    .get(con_name)
                    .cloned()
                    .map(|info| info.instantiate(&mut self.engine))
                {
                    self.engine.unify(ty, &con_info.result_ty, *span);
                    if let ConstructorFields::Record(con_fields) = &con_info.fields {
                        for (field_name, maybe_pat) in fields {
                            if let Some((_, field_ty)) =
                                con_fields.iter().find(|(n, _)| n == field_name)
                            {
                                if let Some(pat) = maybe_pat {
                                    self.bind_pattern(pat, field_ty, env);
                                } else {
                                    // Punned field: bind field name as variable
                                    env.insert(field_name.clone(), Scheme::mono(field_ty.clone()));
                                }
                            } else {
                                self.engine.diagnostics.push(
                                    Diagnostic::error(format!(
                                        "no field `{}` on constructor `{}`",
                                        field_name, con_name
                                    ))
                                    .with_label(Label::primary(*span, "unknown record field")),
                                );
                            }
                        }
                    }
                }
            }
            Pat::As(name, inner, _) => {
                env.insert(name.clone(), Scheme::mono(ty.clone()));
                self.bind_pattern(inner, ty, env);
            }
            Pat::Or(alternatives, _span) => {
                for alt in alternatives {
                    self.bind_pattern(alt, ty, env);
                }
            }
        }
    }
}
