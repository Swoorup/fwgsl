use std::collections::{HashMap, VecDeque};

use shadml_hir::*;
use shadml_span::Span;
use shadml_typechecker::*;

use super::*;
use crate::helpers::*;

impl AstLowering {
    pub(crate) fn monomorphize_generic_functions(&mut self, mut program: HirProgram) -> HirProgram {
        let generic_templates: HashMap<String, HirFunction> = program
            .functions
            .iter()
            .filter(|f| is_generic_hir_function(f))
            .map(|f| (f.name.clone(), f.clone()))
            .collect();

        if generic_templates.is_empty() {
            return program;
        }

        let mut pending = VecDeque::new();

        program.constants = program
            .constants
            .into_iter()
            .map(|constant| HirConst {
                value: self.rewrite_specialized_expr(
                    constant.value,
                    &generic_templates,
                    &mut pending,
                ),
                ..constant
            })
            .collect();

        program.entry_points = program
            .entry_points
            .into_iter()
            .map(|entry| HirEntryPoint {
                body: self.rewrite_specialized_expr(entry.body, &generic_templates, &mut pending),
                ..entry
            })
            .collect();

        let mut retained_functions = Vec::new();
        for function in program.functions.into_iter() {
            if generic_templates.contains_key(&function.name) {
                continue;
            }
            retained_functions.push(HirFunction {
                body: self.rewrite_specialized_expr(
                    function.body,
                    &generic_templates,
                    &mut pending,
                ),
                ..function
            });
        }

        let mut emitted = HashMap::new();
        let mut ordered_specializations = Vec::new();
        while let Some(spec) = pending.pop_front() {
            if emitted.contains_key(&spec.concrete_name) {
                continue;
            }
            let Some(template) = generic_templates.get(&spec.original_name) else {
                continue;
            };
            let specialized =
                self.specialize_function(template, &spec, &generic_templates, &mut pending);
            emitted.insert(spec.concrete_name.clone(), ());
            ordered_specializations.push(specialized);
        }

        retained_functions.extend(ordered_specializations);
        program.functions = retained_functions;
        program
    }

    pub(crate) fn eliminate_tuple_abi(&mut self, program: HirProgram) -> HirProgram {
        let abi_map: HashMap<String, AbiInfo> = program
            .functions
            .iter()
            .map(|function| {
                let param_tys: Vec<Ty> = function.params.iter().map(|(_, ty)| ty.clone()).collect();
                let flat_param_tys: Vec<Ty> = param_tys
                    .iter()
                    .flat_map(flatten_tuple_ty_components)
                    .collect();
                let flat_head_ty = flat_param_tys
                    .iter()
                    .rev()
                    .fold(function.return_ty.clone(), |acc, ty| {
                        Ty::arrow(ty.clone(), acc)
                    });
                (
                    function.name.clone(),
                    AbiInfo {
                        param_tys,
                        flat_head_ty,
                    },
                )
            })
            .collect();

        let functions = program
            .functions
            .into_iter()
            .map(|function| self.eliminate_tuple_function(function, &abi_map))
            .collect();
        let entry_points = program
            .entry_points
            .into_iter()
            .map(|entry| self.eliminate_tuple_entry_point(entry, &abi_map))
            .collect();
        let constants = program
            .constants
            .into_iter()
            .map(|constant| self.eliminate_tuple_const(constant, &abi_map))
            .collect();

        HirProgram {
            functions,
            entry_points,
            constants,
            ..program
        }
    }

    pub(crate) fn eliminate_tuple_function(
        &mut self,
        function: HirFunction,
        abi_map: &HashMap<String, AbiInfo>,
    ) -> HirFunction {
        let mut tuple_env = HashMap::new();
        let params = function
            .params
            .iter()
            .flat_map(|(name, ty)| {
                let flat = flatten_named_tuple_binding(name, ty);
                if matches!(ty, Ty::Tuple(_)) {
                    tuple_env.insert(
                        name.clone(),
                        tuple_value_from_binding(name, ty, function.span),
                    );
                }
                flat
            })
            .collect::<Vec<_>>();

        let body = match self.rewrite_tuple_expr(function.body, abi_map, &tuple_env) {
            Ok(TupleValue::Scalar(expr, _)) => expr,
            Ok(TupleValue::Tuple(_, _)) => {
                self.engine.diagnostics.push(
                    shadml_diagnostics::Diagnostic::error(format!(
                        "function `{}` still returns a tuple after tuple ABI lowering",
                        function.name
                    ))
                    .with_label(shadml_diagnostics::Label::primary(
                        function.span,
                        "tuple result cannot be lowered to WGSL",
                    )),
                );
                HirExpr::Lit(HirLit::Int(0), Ty::Error, function.span)
            }
            Err(message) => {
                self.engine.diagnostics.push(
                    shadml_diagnostics::Diagnostic::error(message).with_label(
                        shadml_diagnostics::Label::primary(function.span, "tuple lowering failed"),
                    ),
                );
                HirExpr::Lit(HirLit::Int(0), Ty::Error, function.span)
            }
        };

        HirFunction {
            params,
            body,
            ..function
        }
    }

    pub(crate) fn eliminate_tuple_entry_point(
        &mut self,
        entry: HirEntryPoint,
        abi_map: &HashMap<String, AbiInfo>,
    ) -> HirEntryPoint {
        let mut tuple_env = HashMap::new();
        let params = entry
            .params
            .iter()
            .flat_map(|(name, ty)| {
                let flat = flatten_named_tuple_binding(name, ty);
                if matches!(ty, Ty::Tuple(_)) {
                    tuple_env.insert(name.clone(), tuple_value_from_binding(name, ty, entry.span));
                }
                flat
            })
            .collect::<Vec<_>>();

        let body = match self.rewrite_tuple_expr(entry.body, abi_map, &tuple_env) {
            Ok(TupleValue::Scalar(expr, _)) => expr,
            Ok(TupleValue::Tuple(_, _)) => {
                self.engine.diagnostics.push(
                    shadml_diagnostics::Diagnostic::error(format!(
                        "entry point `{}` still returns a tuple after tuple ABI lowering",
                        entry.name
                    ))
                    .with_label(shadml_diagnostics::Label::primary(
                        entry.span,
                        "tuple result cannot be lowered to WGSL",
                    )),
                );
                HirExpr::Lit(HirLit::Int(0), Ty::Error, entry.span)
            }
            Err(message) => {
                self.engine.diagnostics.push(
                    shadml_diagnostics::Diagnostic::error(message).with_label(
                        shadml_diagnostics::Label::primary(entry.span, "tuple lowering failed"),
                    ),
                );
                HirExpr::Lit(HirLit::Int(0), Ty::Error, entry.span)
            }
        };

        HirEntryPoint {
            params,
            body,
            ..entry
        }
    }

    pub(crate) fn eliminate_tuple_const(
        &mut self,
        constant: HirConst,
        abi_map: &HashMap<String, AbiInfo>,
    ) -> HirConst {
        let value = match self.rewrite_tuple_expr(constant.value, abi_map, &HashMap::new()) {
            Ok(TupleValue::Scalar(expr, _)) => expr,
            Ok(TupleValue::Tuple(_, _)) => {
                self.engine.diagnostics.push(
                    shadml_diagnostics::Diagnostic::error(format!(
                        "constant `{}` still has a tuple value after tuple ABI lowering",
                        constant.name
                    ))
                    .with_label(shadml_diagnostics::Label::primary(
                        constant.span,
                        "tuple constant cannot be lowered to WGSL",
                    )),
                );
                HirExpr::Lit(HirLit::Int(0), Ty::Error, constant.span)
            }
            Err(message) => {
                self.engine.diagnostics.push(
                    shadml_diagnostics::Diagnostic::error(message).with_label(
                        shadml_diagnostics::Label::primary(constant.span, "tuple lowering failed"),
                    ),
                );
                HirExpr::Lit(HirLit::Int(0), Ty::Error, constant.span)
            }
        };

        HirConst { value, ..constant }
    }

    pub(crate) fn rewrite_tuple_expr(
        &mut self,
        expr: HirExpr,
        abi_map: &HashMap<String, AbiInfo>,
        tuple_env: &HashMap<String, TupleValue>,
    ) -> Result<TupleValue, String> {
        match expr {
            HirExpr::Lit(_, _, _) => {
                let ty = expr.ty().clone();
                Ok(TupleValue::Scalar(expr, ty))
            }
            HirExpr::Var(name, ty, span) if !matches!(ty, Ty::Tuple(_)) => {
                Ok(TupleValue::Scalar(HirExpr::Var(name, ty.clone(), span), ty))
            }
            HirExpr::Var(name, _ty, span) => tuple_env.get(&name).cloned().ok_or_else(|| {
                format!(
                    "tuple value `{}` escaped tuple ABI lowering at {:?}",
                    name, span
                )
            }),
            HirExpr::Tuple(items, ty, span) => {
                // Unit `()` is represented as an empty tuple with type Ty::Con("()").
                // It's a scalar value, not a multi-component tuple that needs ABI lowering.
                if items.is_empty() {
                    Ok(TupleValue::Scalar(
                        HirExpr::Tuple(vec![], ty.clone(), span),
                        ty,
                    ))
                } else {
                    Ok(TupleValue::Tuple(
                        items
                            .into_iter()
                            .map(|item| self.rewrite_tuple_expr(item, abi_map, tuple_env))
                            .collect::<Result<Vec<_>, _>>()?,
                        ty,
                    ))
                }
            }
            HirExpr::TupleIndex(base, index, _, span) => {
                let base = self.rewrite_tuple_expr(*base, abi_map, tuple_env)?;
                match base {
                    TupleValue::Tuple(items, _) => items.into_iter().nth(index).ok_or_else(|| {
                        format!(
                            "tuple index {} out of bounds during tuple ABI lowering at {:?}",
                            index, span
                        )
                    }),
                    TupleValue::Scalar(_, _) => Err(format!(
                        "tuple projection applied to a non-tuple value at {:?}",
                        span
                    )),
                }
            }
            HirExpr::App(_, _, _, _) => self.rewrite_tuple_app(expr, abi_map, tuple_env),
            HirExpr::Let(binds, body, _ty, span) => {
                let mut flat_binds = Vec::new();
                let mut local_env = tuple_env.clone();
                for (name, bind_expr) in binds {
                    let value = self.rewrite_tuple_expr(bind_expr, abi_map, &local_env)?;
                    self.emit_tuple_bindings(&name, value, span, &mut flat_binds, &mut local_env);
                }
                let body_value = self.rewrite_tuple_expr(*body, abi_map, &local_env)?;
                match body_value {
                    TupleValue::Scalar(body_expr, body_ty) => {
                        if flat_binds.is_empty() {
                            Ok(TupleValue::Scalar(body_expr, body_ty))
                        } else {
                            Ok(TupleValue::Scalar(
                                HirExpr::Let(
                                    flat_binds,
                                    Box::new(body_expr),
                                    body_ty.clone(),
                                    span,
                                ),
                                body_ty,
                            ))
                        }
                    }
                    TupleValue::Tuple(_, _) => Err(format!(
                        "tuple-valued let body escaped tuple ABI lowering at {:?}",
                        span
                    )),
                }
            }
            HirExpr::Case(scrutinee, arms, ty, span) => {
                let scrutinee = self
                    .rewrite_tuple_expr(*scrutinee, abi_map, tuple_env)?
                    .into_scalar()
                    .ok_or_else(|| {
                        format!("tuple scrutinee escaped tuple ABI lowering at {:?}", span)
                    })?;
                let arms = arms
                    .into_iter()
                    .map(|arm| {
                        let guard = arm
                            .guard
                            .map(|guard| {
                                self.rewrite_tuple_expr(guard, abi_map, tuple_env)?
                                    .into_scalar()
                                    .ok_or_else(|| {
                                        format!(
                                            "tuple guard escaped tuple ABI lowering at {:?}",
                                            span
                                        )
                                    })
                            })
                            .transpose()?;
                        let body = self
                            .rewrite_tuple_expr(arm.body, abi_map, tuple_env)?
                            .into_scalar()
                            .ok_or_else(|| {
                                format!("tuple case body escaped tuple ABI lowering at {:?}", span)
                            })?;
                        Ok(HirCaseArm {
                            pattern: arm.pattern,
                            guard,
                            body,
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                Ok(TupleValue::Scalar(
                    HirExpr::Case(Box::new(scrutinee), arms, ty.clone(), span),
                    ty,
                ))
            }
            HirExpr::If(cond, then_expr, else_expr, ty, span) => {
                let cond = self
                    .rewrite_tuple_expr(*cond, abi_map, tuple_env)?
                    .into_scalar()
                    .ok_or_else(|| {
                        format!("tuple condition escaped tuple ABI lowering at {:?}", span)
                    })?;
                let then_expr = self
                    .rewrite_tuple_expr(*then_expr, abi_map, tuple_env)?
                    .into_scalar()
                    .ok_or_else(|| {
                        format!("tuple then-branch escaped tuple ABI lowering at {:?}", span)
                    })?;
                let else_expr = self
                    .rewrite_tuple_expr(*else_expr, abi_map, tuple_env)?
                    .into_scalar()
                    .ok_or_else(|| {
                        format!("tuple else-branch escaped tuple ABI lowering at {:?}", span)
                    })?;
                Ok(TupleValue::Scalar(
                    HirExpr::If(
                        Box::new(cond),
                        Box::new(then_expr),
                        Box::new(else_expr),
                        ty.clone(),
                        span,
                    ),
                    ty,
                ))
            }
            HirExpr::BinOp(op, lhs, rhs, ty, span) => {
                let lhs = self
                    .rewrite_tuple_expr(*lhs, abi_map, tuple_env)?
                    .into_scalar()
                    .ok_or_else(|| format!("tuple lhs escaped tuple ABI lowering at {:?}", span))?;
                let rhs = self
                    .rewrite_tuple_expr(*rhs, abi_map, tuple_env)?
                    .into_scalar()
                    .ok_or_else(|| format!("tuple rhs escaped tuple ABI lowering at {:?}", span))?;
                Ok(TupleValue::Scalar(
                    HirExpr::BinOp(op, Box::new(lhs), Box::new(rhs), ty.clone(), span),
                    ty,
                ))
            }
            HirExpr::UnaryNeg(inner, ty, span) => {
                let inner = self
                    .rewrite_tuple_expr(*inner, abi_map, tuple_env)?
                    .into_scalar()
                    .ok_or_else(|| {
                        format!(
                            "tuple unary operand escaped tuple ABI lowering at {:?}",
                            span
                        )
                    })?;
                Ok(TupleValue::Scalar(
                    HirExpr::UnaryNeg(Box::new(inner), ty.clone(), span),
                    ty,
                ))
            }
            HirExpr::UnaryNot(inner, ty, span) => {
                let inner = self
                    .rewrite_tuple_expr(*inner, abi_map, tuple_env)?
                    .into_scalar()
                    .ok_or_else(|| {
                        format!(
                            "tuple unary operand escaped tuple ABI lowering at {:?}",
                            span
                        )
                    })?;
                Ok(TupleValue::Scalar(
                    HirExpr::UnaryNot(Box::new(inner), ty.clone(), span),
                    ty,
                ))
            }
            HirExpr::UnaryBitNot(inner, ty, span) => {
                let inner = self
                    .rewrite_tuple_expr(*inner, abi_map, tuple_env)?
                    .into_scalar()
                    .ok_or_else(|| {
                        format!(
                            "tuple unary operand escaped tuple ABI lowering at {:?}",
                            span
                        )
                    })?;
                Ok(TupleValue::Scalar(
                    HirExpr::UnaryBitNot(Box::new(inner), ty.clone(), span),
                    ty,
                ))
            }
            HirExpr::ConstructorCall(name, tag, args, ty, span) => {
                let args = args
                    .into_iter()
                    .map(|arg| {
                        self.rewrite_tuple_expr(arg, abi_map, tuple_env)?
                            .into_scalar()
                            .ok_or_else(|| {
                                format!(
                                    "tuple constructor argument escaped tuple ABI lowering at {:?}",
                                    span
                                )
                            })
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                Ok(TupleValue::Scalar(
                    HirExpr::ConstructorCall(name, tag, args, ty.clone(), span),
                    ty,
                ))
            }
            HirExpr::FieldAccess(base, field, ty, span) => {
                let base = self
                    .rewrite_tuple_expr(*base, abi_map, tuple_env)?
                    .into_scalar()
                    .ok_or_else(|| {
                        format!("tuple field base escaped tuple ABI lowering at {:?}", span)
                    })?;
                Ok(TupleValue::Scalar(
                    HirExpr::FieldAccess(Box::new(base), field, ty.clone(), span),
                    ty,
                ))
            }
            HirExpr::Index(base, index, ty, span) => {
                let base = self
                    .rewrite_tuple_expr(*base, abi_map, tuple_env)?
                    .into_scalar()
                    .ok_or_else(|| {
                        format!("tuple index base escaped tuple ABI lowering at {:?}", span)
                    })?;
                let index = self
                    .rewrite_tuple_expr(*index, abi_map, tuple_env)?
                    .into_scalar()
                    .ok_or_else(|| {
                        format!("tuple index escaped tuple ABI lowering at {:?}", span)
                    })?;
                Ok(TupleValue::Scalar(
                    HirExpr::Index(Box::new(base), Box::new(index), ty.clone(), span),
                    ty,
                ))
            }
            HirExpr::Loop(loop_name, bindings, body, ty, span) => {
                let bindings = bindings
                    .into_iter()
                    .map(|(name, expr)| {
                        self.rewrite_tuple_expr(expr, abi_map, tuple_env)?
                            .into_scalar()
                            .ok_or_else(|| {
                                format!(
                                    "tuple loop binding escaped tuple ABI lowering at {:?}",
                                    span
                                )
                            })
                            .map(|expr| (name, expr))
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                let body = self
                    .rewrite_tuple_expr(*body, abi_map, tuple_env)?
                    .into_scalar()
                    .ok_or_else(|| {
                        format!("tuple loop body escaped tuple ABI lowering at {:?}", span)
                    })?;
                Ok(TupleValue::Scalar(
                    HirExpr::Loop(loop_name, bindings, Box::new(body), ty.clone(), span),
                    ty,
                ))
            }
            HirExpr::BitfieldConstruct(name, fields, ty, span) => {
                let fields = fields
                    .into_iter()
                    .map(|(field_name, expr)| {
                        self.rewrite_tuple_expr(expr, abi_map, tuple_env)?
                            .into_scalar()
                            .ok_or_else(|| {
                                format!(
                                    "tuple bitfield construction field escaped tuple ABI lowering at {:?}",
                                    span
                                )
                            })
                            .map(|expr| (field_name, expr))
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                Ok(TupleValue::Scalar(
                    HirExpr::BitfieldConstruct(name, fields, ty.clone(), span),
                    ty,
                ))
            }
            HirExpr::BitfieldUpdate(name, base, fields, ty, span) => {
                let base = self
                    .rewrite_tuple_expr(*base, abi_map, tuple_env)?
                    .into_scalar()
                    .ok_or_else(|| {
                        format!(
                            "tuple bitfield base escaped tuple ABI lowering at {:?}",
                            span
                        )
                    })?;
                let fields = fields
                    .into_iter()
                    .map(|(field_name, expr)| {
                        self.rewrite_tuple_expr(expr, abi_map, tuple_env)?
                            .into_scalar()
                            .ok_or_else(|| {
                                format!(
                                    "tuple bitfield update field escaped tuple ABI lowering at {:?}",
                                    span
                                )
                            })
                            .map(|expr| (field_name, expr))
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                Ok(TupleValue::Scalar(
                    HirExpr::BitfieldUpdate(name, Box::new(base), fields, ty.clone(), span),
                    ty,
                ))
            }
        }
    }

    pub(crate) fn rewrite_tuple_app(
        &mut self,
        expr: HirExpr,
        abi_map: &HashMap<String, AbiInfo>,
        tuple_env: &HashMap<String, TupleValue>,
    ) -> Result<TupleValue, String> {
        let (head, args) = collect_hir_app_chain(expr.clone());
        if let HirExpr::Var(name, _head_ty, span) = &head {
            if let Some(abi) = abi_map.get(name) {
                let rewritten_args = args
                    .into_iter()
                    .map(|arg| self.rewrite_tuple_expr(arg, abi_map, tuple_env))
                    .collect::<Result<Vec<_>, _>>()?;
                if rewritten_args.len() > abi.param_tys.len() {
                    return Err(format!(
                        "call to `{}` has more arguments than its tuple-aware ABI expects",
                        name
                    ));
                }

                let mut flat_args = Vec::new();
                for (arg, param_ty) in rewritten_args.into_iter().zip(abi.param_tys.iter()) {
                    flat_args.extend(expand_tuple_argument(arg, param_ty)?);
                }

                let mut cursor = abi.flat_head_ty.clone();
                let mut app = HirExpr::Var(name.clone(), abi.flat_head_ty.clone(), *span);
                for arg in flat_args {
                    let Ty::Arrow(_, to) = cursor else {
                        return Err(format!(
                            "flattened tuple ABI for `{}` is not callable",
                            name
                        ));
                    };
                    let next_ty = (*to).clone();
                    app = HirExpr::App(Box::new(app), Box::new(arg), next_ty.clone(), *span);
                    cursor = next_ty;
                }
                return Ok(TupleValue::Scalar(app, cursor));
            }
        }
        let HirExpr::App(func, arg, ty, span) = expr else {
            unreachable!();
        };
        let func = self
            .rewrite_tuple_expr(*func, abi_map, tuple_env)?
            .into_scalar()
            .ok_or_else(|| {
                format!(
                    "tuple-valued callee escaped tuple ABI lowering at {:?}",
                    span
                )
            })?;
        let arg = self
            .rewrite_tuple_expr(*arg, abi_map, tuple_env)?
            .into_scalar()
            .ok_or_else(|| {
                format!(
                    "tuple-valued argument escaped tuple ABI lowering at {:?}",
                    span
                )
            })?;
        Ok(TupleValue::Scalar(
            HirExpr::App(Box::new(func), Box::new(arg), ty.clone(), span),
            ty,
        ))
    }

    pub(crate) fn emit_tuple_bindings(
        &self,
        name: &str,
        value: TupleValue,
        span: Span,
        flat_binds: &mut Vec<(String, HirExpr)>,
        tuple_env: &mut HashMap<String, TupleValue>,
    ) {
        match value {
            TupleValue::Scalar(expr, ty) => {
                flat_binds.push((name.to_string(), expr));
                tuple_env.insert(
                    name.to_string(),
                    TupleValue::Scalar(HirExpr::Var(name.to_string(), ty.clone(), span), ty),
                );
            }
            TupleValue::Tuple(items, ty) => {
                for (index, item) in items.into_iter().enumerate() {
                    let component_name = tuple_component_name(name, index);
                    self.emit_tuple_bindings(&component_name, item, span, flat_binds, tuple_env);
                }
                tuple_env.insert(name.to_string(), tuple_value_from_binding(name, &ty, span));
            }
        }
    }

    pub(crate) fn specialize_function(
        &mut self,
        template: &HirFunction,
        spec: &PendingSpecialization,
        generic_templates: &HashMap<String, HirFunction>,
        pending: &mut VecDeque<PendingSpecialization>,
    ) -> HirFunction {
        let body = self.rewrite_specialized_expr_with_subst(
            template.body.clone(),
            generic_templates,
            pending,
            &spec.subst,
        );
        let body = resolve_hir_expr_assoc_projections(body, &self.impls, &self.builtin_impls);
        let impls = &self.impls;
        let builtin_impls = &self.builtin_impls;
        let resolve = |ty: &Ty| -> Ty {
            let substituted = substitute_ty_vars(ty, &spec.subst);
            shadml_semantic::resolve_assoc_projections_with_impls(
                &substituted,
                impls,
                builtin_impls,
            )
        };
        HirFunction {
            name: spec.concrete_name.clone(),
            params: template
                .params
                .iter()
                .map(|(name, ty)| (name.clone(), resolve(ty)))
                .collect(),
            return_ty: resolve(&template.return_ty),
            body,
            span: template.span,
            comments: template.comments.clone(),
        }
    }

    pub(crate) fn rewrite_specialized_expr(
        &mut self,
        expr: HirExpr,
        generic_templates: &HashMap<String, HirFunction>,
        pending: &mut VecDeque<PendingSpecialization>,
    ) -> HirExpr {
        self.rewrite_specialized_expr_with_subst(expr, generic_templates, pending, &HashMap::new())
    }

    pub(crate) fn rewrite_specialized_expr_with_subst(
        &mut self,
        expr: HirExpr,
        generic_templates: &HashMap<String, HirFunction>,
        pending: &mut VecDeque<PendingSpecialization>,
        subst: &HashMap<TyVarId, Ty>,
    ) -> HirExpr {
        let rewritten = match expr {
            HirExpr::Lit(lit, ty, span) => HirExpr::Lit(lit, substitute_ty_vars(&ty, subst), span),
            HirExpr::Var(name, ty, span) => {
                let ty = substitute_ty_vars(&ty, subst);
                let resolved_name = self.resolve_trait_method_or_diag(&name, &ty, span);
                HirExpr::Var(resolved_name, ty, span)
            }
            HirExpr::Tuple(items, ty, span) => HirExpr::Tuple(
                items
                    .into_iter()
                    .map(|item| {
                        self.rewrite_specialized_expr_with_subst(
                            item,
                            generic_templates,
                            pending,
                            subst,
                        )
                    })
                    .collect(),
                substitute_ty_vars(&ty, subst),
                span,
            ),
            HirExpr::TupleIndex(base, index, ty, span) => HirExpr::TupleIndex(
                Box::new(self.rewrite_specialized_expr_with_subst(
                    *base,
                    generic_templates,
                    pending,
                    subst,
                )),
                index,
                substitute_ty_vars(&ty, subst),
                span,
            ),
            HirExpr::App(func, arg, ty, span) => HirExpr::App(
                Box::new(self.rewrite_specialized_expr_with_subst(
                    *func,
                    generic_templates,
                    pending,
                    subst,
                )),
                Box::new(self.rewrite_specialized_expr_with_subst(
                    *arg,
                    generic_templates,
                    pending,
                    subst,
                )),
                substitute_ty_vars(&ty, subst),
                span,
            ),
            HirExpr::Let(binds, body, ty, span) => HirExpr::Let(
                binds
                    .into_iter()
                    .map(|(name, expr)| {
                        (
                            name,
                            self.rewrite_specialized_expr_with_subst(
                                expr,
                                generic_templates,
                                pending,
                                subst,
                            ),
                        )
                    })
                    .collect(),
                Box::new(self.rewrite_specialized_expr_with_subst(
                    *body,
                    generic_templates,
                    pending,
                    subst,
                )),
                substitute_ty_vars(&ty, subst),
                span,
            ),
            HirExpr::Case(scrutinee, arms, ty, span) => HirExpr::Case(
                Box::new(self.rewrite_specialized_expr_with_subst(
                    *scrutinee,
                    generic_templates,
                    pending,
                    subst,
                )),
                arms.into_iter()
                    .map(|arm| HirCaseArm {
                        pattern: substitute_pattern_ty_vars(arm.pattern, subst),
                        guard: arm.guard.map(|guard| {
                            self.rewrite_specialized_expr_with_subst(
                                guard,
                                generic_templates,
                                pending,
                                subst,
                            )
                        }),
                        body: self.rewrite_specialized_expr_with_subst(
                            arm.body,
                            generic_templates,
                            pending,
                            subst,
                        ),
                    })
                    .collect(),
                substitute_ty_vars(&ty, subst),
                span,
            ),
            HirExpr::If(cond, then_expr, else_expr, ty, span) => HirExpr::If(
                Box::new(self.rewrite_specialized_expr_with_subst(
                    *cond,
                    generic_templates,
                    pending,
                    subst,
                )),
                Box::new(self.rewrite_specialized_expr_with_subst(
                    *then_expr,
                    generic_templates,
                    pending,
                    subst,
                )),
                Box::new(self.rewrite_specialized_expr_with_subst(
                    *else_expr,
                    generic_templates,
                    pending,
                    subst,
                )),
                substitute_ty_vars(&ty, subst),
                span,
            ),
            HirExpr::BinOp(op, lhs, rhs, ty, span) => HirExpr::BinOp(
                op,
                Box::new(self.rewrite_specialized_expr_with_subst(
                    *lhs,
                    generic_templates,
                    pending,
                    subst,
                )),
                Box::new(self.rewrite_specialized_expr_with_subst(
                    *rhs,
                    generic_templates,
                    pending,
                    subst,
                )),
                substitute_ty_vars(&ty, subst),
                span,
            ),
            HirExpr::UnaryNeg(inner, ty, span) => HirExpr::UnaryNeg(
                Box::new(self.rewrite_specialized_expr_with_subst(
                    *inner,
                    generic_templates,
                    pending,
                    subst,
                )),
                substitute_ty_vars(&ty, subst),
                span,
            ),
            HirExpr::UnaryNot(inner, ty, span) => HirExpr::UnaryNot(
                Box::new(self.rewrite_specialized_expr_with_subst(
                    *inner,
                    generic_templates,
                    pending,
                    subst,
                )),
                substitute_ty_vars(&ty, subst),
                span,
            ),
            HirExpr::UnaryBitNot(inner, ty, span) => HirExpr::UnaryBitNot(
                Box::new(self.rewrite_specialized_expr_with_subst(
                    *inner,
                    generic_templates,
                    pending,
                    subst,
                )),
                substitute_ty_vars(&ty, subst),
                span,
            ),
            HirExpr::ConstructorCall(name, tag, args, ty, span) => HirExpr::ConstructorCall(
                name,
                tag,
                args.into_iter()
                    .map(|arg| {
                        self.rewrite_specialized_expr_with_subst(
                            arg,
                            generic_templates,
                            pending,
                            subst,
                        )
                    })
                    .collect(),
                substitute_ty_vars(&ty, subst),
                span,
            ),
            HirExpr::FieldAccess(base, field, ty, span) => HirExpr::FieldAccess(
                Box::new(self.rewrite_specialized_expr_with_subst(
                    *base,
                    generic_templates,
                    pending,
                    subst,
                )),
                field,
                substitute_ty_vars(&ty, subst),
                span,
            ),
            HirExpr::Index(base, index, ty, span) => HirExpr::Index(
                Box::new(self.rewrite_specialized_expr_with_subst(
                    *base,
                    generic_templates,
                    pending,
                    subst,
                )),
                Box::new(self.rewrite_specialized_expr_with_subst(
                    *index,
                    generic_templates,
                    pending,
                    subst,
                )),
                substitute_ty_vars(&ty, subst),
                span,
            ),
            HirExpr::Loop(loop_name, bindings, body, ty, span) => HirExpr::Loop(
                loop_name,
                bindings
                    .into_iter()
                    .map(|(name, expr)| {
                        (
                            name,
                            self.rewrite_specialized_expr_with_subst(
                                expr,
                                generic_templates,
                                pending,
                                subst,
                            ),
                        )
                    })
                    .collect(),
                Box::new(self.rewrite_specialized_expr_with_subst(
                    *body,
                    generic_templates,
                    pending,
                    subst,
                )),
                substitute_ty_vars(&ty, subst),
                span,
            ),
            HirExpr::BitfieldConstruct(name, fields, ty, span) => HirExpr::BitfieldConstruct(
                name,
                fields
                    .into_iter()
                    .map(|(name, expr)| {
                        (
                            name,
                            self.rewrite_specialized_expr_with_subst(
                                expr,
                                generic_templates,
                                pending,
                                subst,
                            ),
                        )
                    })
                    .collect(),
                substitute_ty_vars(&ty, subst),
                span,
            ),
            HirExpr::BitfieldUpdate(name, base, fields, ty, span) => HirExpr::BitfieldUpdate(
                name,
                Box::new(self.rewrite_specialized_expr_with_subst(
                    *base,
                    generic_templates,
                    pending,
                    subst,
                )),
                fields
                    .into_iter()
                    .map(|(name, expr)| {
                        (
                            name,
                            self.rewrite_specialized_expr_with_subst(
                                expr,
                                generic_templates,
                                pending,
                                subst,
                            ),
                        )
                    })
                    .collect(),
                substitute_ty_vars(&ty, subst),
                span,
            ),
        };

        self.specialize_application_if_needed(rewritten, generic_templates, pending)
    }

    pub(crate) fn specialize_application_if_needed(
        &mut self,
        expr: HirExpr,
        generic_templates: &HashMap<String, HirFunction>,
        pending: &mut VecDeque<PendingSpecialization>,
    ) -> HirExpr {
        let (head, _) = collect_hir_app_args(&expr);
        let Some((name, head_ty, span)) = (match head {
            HirExpr::Var(name, head_ty, span) => Some((name.clone(), head_ty.clone(), *span)),
            _ => None,
        }) else {
            return expr;
        };
        let Some(template) = generic_templates.get(&name) else {
            return expr;
        };

        let template_ty = hir_function_type(template);
        let mut subst = HashMap::new();
        if !collect_specialization_bindings(&template_ty, &head_ty, &mut subst) || subst.is_empty()
        {
            return expr;
        }
        if subst.values().any(|ty| !ty.free_vars().is_empty()) {
            return expr;
        }

        let mut vars = template_ty.free_vars();
        vars.sort_unstable();
        vars.dedup();
        let concrete_args: Vec<Ty> = vars
            .into_iter()
            .filter_map(|var| subst.get(&var).cloned())
            .collect();
        if concrete_args.is_empty() {
            return expr;
        }

        let concrete_name = mono_mangled_function_name(&name, &concrete_args);
        pending.push_back(PendingSpecialization {
            original_name: name,
            concrete_name: concrete_name.clone(),
            subst,
        });

        rename_hir_app_head(expr, &concrete_name, head_ty, span)
    }
}

pub(crate) fn collect_specialization_bindings(
    pattern: &Ty,
    concrete: &Ty,
    subst: &mut HashMap<TyVarId, Ty>,
) -> bool {
    match pattern {
        Ty::Var(id) => match subst.get(id) {
            Some(existing) => existing == concrete,
            None => {
                subst.insert(*id, concrete.clone());
                true
            }
        },
        Ty::Con(name) => matches!(concrete, Ty::Con(other) if other == name),
        Ty::Nat(n) => matches!(concrete, Ty::Nat(other) if other == n),
        Ty::App(pf, pa) => match concrete {
            Ty::App(cf, ca) => {
                collect_specialization_bindings(pf, cf, subst)
                    && collect_specialization_bindings(pa, ca, subst)
            }
            _ => false,
        },
        Ty::Arrow(pa, pb) => match concrete {
            Ty::Arrow(ca, cb) => {
                collect_specialization_bindings(pa, ca, subst)
                    && collect_specialization_bindings(pb, cb, subst)
            }
            _ => false,
        },
        Ty::Tuple(pats) => match concrete {
            Ty::Tuple(args) if pats.len() == args.len() => pats
                .iter()
                .zip(args.iter())
                .all(|(pat, arg)| collect_specialization_bindings(pat, arg, subst)),
            _ => false,
        },
        Ty::Forall(_, body) => collect_specialization_bindings(body, concrete, subst),
        Ty::Error => true,
        Ty::AssocProj { .. } => {
            // Associated type projections in the template (e.g., a.Output) will
            // resolve to concrete types once the trait parameters are bound.
            // The type system has already verified consistency, so we can
            // accept the match here. The important bindings come from the
            // parameter types; the return type's AssocProj is derived.
            true
        }
    }
}

pub(crate) fn flatten_tuple_ty_components(ty: &Ty) -> Vec<Ty> {
    match ty {
        Ty::Tuple(items) => items.iter().flat_map(flatten_tuple_ty_components).collect(),
        _ => vec![ty.clone()],
    }
}

pub(crate) fn tuple_component_name(base: &str, index: usize) -> String {
    format!("__tuple_{}_{}", base, index)
}

pub(crate) fn flatten_named_tuple_binding(base: &str, ty: &Ty) -> Vec<(String, Ty)> {
    match ty {
        Ty::Tuple(items) => items
            .iter()
            .enumerate()
            .flat_map(|(index, item)| {
                flatten_named_tuple_binding(&tuple_component_name(base, index), item)
            })
            .collect(),
        _ => vec![(base.to_string(), ty.clone())],
    }
}

pub(crate) fn tuple_value_from_binding(base: &str, ty: &Ty, span: Span) -> TupleValue {
    match ty {
        Ty::Tuple(items) => TupleValue::Tuple(
            items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    tuple_value_from_binding(&tuple_component_name(base, index), item, span)
                })
                .collect(),
            ty.clone(),
        ),
        _ => TupleValue::Scalar(HirExpr::Var(base.to_string(), ty.clone(), span), ty.clone()),
    }
}

pub(crate) fn expand_tuple_argument(
    value: TupleValue,
    param_ty: &Ty,
) -> Result<Vec<HirExpr>, String> {
    match (value, param_ty) {
        (TupleValue::Scalar(_, _), Ty::Tuple(_)) => Err(format!(
            "expected a tuple argument for parameter type `{}`",
            param_ty
        )),
        (TupleValue::Tuple(_, _), ty) if !matches!(ty, Ty::Tuple(_)) => Err(format!(
            "tuple argument does not match non-tuple parameter type `{}`",
            ty
        )),
        (TupleValue::Scalar(expr, _), _) => Ok(vec![expr]),
        (TupleValue::Tuple(items, _), Ty::Tuple(param_items)) => {
            if items.len() != param_items.len() {
                return Err(format!(
                    "tuple argument arity mismatch: expected {}, found {}",
                    param_items.len(),
                    items.len()
                ));
            }
            let mut flat = Vec::new();
            for (item, param_item) in items.into_iter().zip(param_items.iter()) {
                flat.extend(expand_tuple_argument(item, param_item)?);
            }
            Ok(flat)
        }
        (TupleValue::Tuple(_, _), _) => unreachable!(),
    }
}

pub(crate) fn rename_hir_app_head(
    expr: HirExpr,
    new_name: &str,
    new_ty: Ty,
    span: Span,
) -> HirExpr {
    match expr {
        HirExpr::App(func, arg, ty, app_span) => HirExpr::App(
            Box::new(rename_hir_app_head(*func, new_name, new_ty, span)),
            arg,
            ty,
            app_span,
        ),
        HirExpr::Var(_, _, _) => HirExpr::Var(new_name.to_string(), new_ty, span),
        other => other,
    }
}

/// Produce a mangled name for a monomorphized function.
///
/// `AssocProj` types should be resolved to concrete types before reaching
/// this function. An unresolved `AssocProj` in mangling indicates an earlier
/// pipeline error where associated type resolution did not complete.
pub(crate) fn mono_mangled_function_name(name: &str, concrete_args: &[Ty]) -> String {
    let mut mangled = name.to_string();
    for ty in concrete_args {
        mangled.push('_');
        mangled.push_str(&ty_to_mono_suffix_local(ty));
    }
    mangled
}

pub(crate) fn should_emit_missing_trait_impl_diag(trait_name: &str) -> bool {
    !matches!(
        trait_name,
        "Add"
            | "Sub"
            | "Mul"
            | "Div"
            | "Mod"
            | "BitAnd"
            | "BitXor"
            | "Shl"
            | "Shr"
            | "BitNot"
            | "Neg"
    )
}

pub(crate) fn ty_to_mono_suffix_local(ty: &Ty) -> String {
    match ty {
        Ty::Con(name) => match name.as_str() {
            ty_name::I32 => "i32".to_string(),
            ty_name::U32 => "u32".to_string(),
            ty_name::F32 => "f32".to_string(),
            ty_name::BOOL => "bool".to_string(),
            ty_name::UNIT => "unit".to_string(),
            other => other.to_lowercase(),
        },
        Ty::App(_, _) => {
            let mut parts = Vec::new();
            let mut cursor = ty;
            loop {
                match cursor {
                    Ty::App(f, arg) => {
                        parts.push(ty_to_mono_suffix_local(arg));
                        cursor = f.as_ref();
                    }
                    other => {
                        parts.push(ty_to_mono_suffix_local(other));
                        break;
                    }
                }
            }
            parts.reverse();
            parts.join("_")
        }
        Ty::Nat(n) => n.to_string(),
        Ty::Var(id) => format!("t{}", id),
        Ty::Arrow(_, _) => "fn".to_string(),
        Ty::Tuple(elems) => elems
            .iter()
            .map(ty_to_mono_suffix_local)
            .collect::<Vec<_>>()
            .join("_"),
        Ty::Forall(_, body) => ty_to_mono_suffix_local(body),
        Ty::Error => "error".to_string(),
        Ty::AssocProj {
            name, trait_name, ..
        } => {
            debug_assert!(
                !trait_name.is_empty(),
                "AssocProj with empty trait_name should not reach mangling"
            );
            format!("{}_{}", trait_name.to_lowercase(), name.to_lowercase())
        }
    }
}
