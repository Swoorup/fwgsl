use shadml_hir::*;
use shadml_parser::parser::*;
use shadml_span::Span;
use shadml_typechecker::*;

use super::*;

impl AstLowering {
    pub(crate) fn build_pattern_bindings(
        &mut self,
        pat: &Pat,
        base: HirExpr,
        ty: &Ty,
    ) -> Vec<(String, HirExpr)> {
        let final_ty = self.finalize_resolve(ty);
        match pat {
            Pat::Var(name, _) => vec![(name.clone(), base)],
            Pat::Wild(_) => vec![],
            Pat::Paren(inner, _) => self.build_pattern_bindings(inner, base, &final_ty),
            Pat::As(name, inner, _) => {
                let mut binds = vec![(name.clone(), base.clone())];
                binds.extend(self.build_pattern_bindings(inner, base, &final_ty));
                binds
            }
            Pat::Tuple(items, span) => match final_ty {
                Ty::Tuple(elem_tys) if elem_tys.len() == items.len() => items
                    .iter()
                    .zip(elem_tys.iter())
                    .enumerate()
                    .flat_map(|(index, (item, item_ty))| {
                        let projection = HirExpr::TupleIndex(
                            Box::new(base.clone()),
                            index,
                            item_ty.clone(),
                            *span,
                        );
                        self.build_pattern_bindings(item, projection, item_ty)
                    })
                    .collect(),
                _ => vec![],
            },
            _ => vec![],
        }
    }

    pub(crate) fn lower_pattern(&mut self, pat: &Pat, _scrutinee_ty: &Ty) -> HirPattern {
        match pat {
            Pat::Wild(_) => HirPattern::Wild,
            Pat::Var(name, _) => {
                HirPattern::Var(name.clone(), self.finalize_resolve(_scrutinee_ty))
            }
            Pat::Con(name, sub_pats, _) => {
                if let Some(con_info) = self
                    .constructors
                    .get(name)
                    .cloned()
                    .map(|info| info.instantiate(&mut self.engine))
                {
                    let sub_hir: Vec<HirPattern> = match &con_info.fields {
                        ConstructorFields::Positional(field_tys) => sub_pats
                            .iter()
                            .zip(field_tys.iter())
                            .map(|(p, ty)| self.lower_pattern(p, ty))
                            .collect(),
                        ConstructorFields::Record(fields) => sub_pats
                            .iter()
                            .zip(fields.iter())
                            .map(|(p, (_, ty))| self.lower_pattern(p, ty))
                            .collect(),
                        ConstructorFields::Empty => vec![],
                    };
                    HirPattern::Constructor(name.clone(), con_info.tag, sub_hir)
                } else {
                    HirPattern::Wild
                }
            }
            Pat::Lit(lit, _) => {
                let (hir_lit, _) = self.lower_lit(lit);
                HirPattern::Lit(hir_lit)
            }
            Pat::Paren(inner, _) => self.lower_pattern(inner, _scrutinee_ty),
            Pat::Tuple(pats, _) => {
                if let Some(first) = pats.first() {
                    self.lower_pattern(first, _scrutinee_ty)
                } else {
                    HirPattern::Wild
                }
            }
            Pat::Record(con_name, fields, _, _) => {
                if let Some(con_info) = self
                    .constructors
                    .get(con_name)
                    .cloned()
                    .map(|info| info.instantiate(&mut self.engine))
                {
                    let sub_pats: Vec<HirPattern> =
                        if let ConstructorFields::Record(con_fields) = &con_info.fields {
                            con_fields
                                .iter()
                                .map(|(field_name, field_ty)| {
                                    if let Some((_, maybe_pat)) =
                                        fields.iter().find(|(name, _)| name == field_name)
                                    {
                                        if let Some(p) = maybe_pat {
                                            self.lower_pattern(p, field_ty)
                                        } else {
                                            HirPattern::Var(
                                                field_name.clone(),
                                                self.finalize_resolve(field_ty),
                                            )
                                        }
                                    } else {
                                        HirPattern::Wild
                                    }
                                })
                                .collect()
                        } else {
                            vec![]
                        };
                    HirPattern::Constructor(con_name.clone(), con_info.tag, sub_pats)
                } else {
                    HirPattern::Wild
                }
            }
            Pat::As(name, inner, _) => {
                // As-pattern: for HIR, just use the inner pattern
                // (the name binding is already in the env)
                let _ = name;
                self.lower_pattern(inner, _scrutinee_ty)
            }
            Pat::Or(alternatives, _) => {
                let hir_alts: Vec<HirPattern> = alternatives
                    .iter()
                    .map(|p| self.lower_pattern(p, _scrutinee_ty))
                    .collect();
                HirPattern::Or(hir_alts)
            }
        }
    }

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
                    self.engine.unify(ty, &con_info.result_ty, *span);
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
                }
            }
            Pat::Lit(lit, span) => {
                let lit_ty = match lit {
                    Lit::Int(_) => {
                        // Use a fresh var so integer literals can unify with
                        // both I32 and U32 (determined by the scrutinee type).
                        self.engine.fresh_var()
                    }
                    Lit::UInt(_) => Ty::u32(),
                    Lit::Float(_) => Ty::f32(),
                    Lit::String(_) => Ty::Con(ty_name::STRING.into()),
                    Lit::Char(_) => Ty::Con("Char".into()),
                };
                self.engine.unify(ty, &lit_ty, *span);
            }
            Pat::Paren(inner, _) => self.bind_pattern(inner, ty, env),
            Pat::Tuple(pats, _) => {
                let elem_tys: Vec<Ty> = pats.iter().map(|_| self.engine.fresh_var()).collect();
                if !elem_tys.is_empty() {
                    let tuple_ty = Ty::Tuple(elem_tys.clone());
                    self.engine.unify(ty, &tuple_ty, Span::new(0, 0));
                }
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
                                    env.insert(field_name.clone(), Scheme::mono(field_ty.clone()));
                                }
                            }
                        }
                    }
                }
            }
            Pat::As(name, inner, _) => {
                env.insert(name.clone(), Scheme::mono(ty.clone()));
                self.bind_pattern(inner, ty, env);
            }
            Pat::Or(alternatives, _) => {
                for alt in alternatives {
                    self.bind_pattern(alt, ty, env);
                }
            }
        }
    }

    pub(crate) fn finalize_pattern(&self, pattern: HirPattern) -> HirPattern {
        match pattern {
            HirPattern::Wild => HirPattern::Wild,
            HirPattern::Var(name, ty) => HirPattern::Var(name, self.finalize_resolve(&ty)),
            HirPattern::Constructor(name, tag, sub_patterns) => HirPattern::Constructor(
                name,
                tag,
                sub_patterns
                    .into_iter()
                    .map(|pattern| self.finalize_pattern(pattern))
                    .collect(),
            ),
            HirPattern::Lit(lit) => HirPattern::Lit(lit),
            HirPattern::Or(alts) => {
                HirPattern::Or(alts.into_iter().map(|p| self.finalize_pattern(p)).collect())
            }
        }
    }
}
