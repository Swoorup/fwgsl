//! Semantic analysis for shadml.
//!
//! Performs name resolution and type inference over the parser's AST.
//! Collects data type definitions, constructor info, type signatures,
//! and infers types for function bodies using Algorithm W (HM inference).

use std::collections::HashMap;

use shadml_diagnostics::{Diagnostic, DiagnosticSink, Label};
use shadml_parser::parser::*;
use shadml_span::Span;
use shadml_typechecker::*;

/// Information about a trait declaration.
#[derive(Debug, Clone)]
pub struct TraitInfo {
    pub name: String,
    /// The type variables the trait is parameterised over.
    pub vars: Vec<String>,
    pub var_ids: Vec<TyVarId>,
    /// Method signatures: method_name → type (with trait vars as free type variables).
    pub methods: Vec<(String, Ty)>,
}

/// Information about a trait impl.
#[derive(Debug, Clone)]
pub struct ImplInfo {
    /// None for standalone impls.
    pub trait_name: Option<String>,
    /// The concrete types this impl is for. Standalone impls contain one type.
    pub tys: Vec<Ty>,
    /// Method implementations: method_name → mangled function name.
    pub methods: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct BuiltinExternInfo {
    pub name: String,
    pub ty: Ty,
    pub lowering: BuiltinLowering,
}

#[derive(Debug, Clone)]
pub struct BuiltinImplInfo {
    pub trait_name: String,
    pub tys: Vec<Ty>,
    pub methods: HashMap<String, BuiltinLowering>,
}

/// The semantic analyzer: collects definitions and performs type inference.
pub struct SemanticAnalyzer {
    pub env: TypeEnv,
    pub engine: InferEngine,
    pub constructors: HashMap<String, ConstructorInfo>,
    pub data_types: HashMap<String, DataTypeInfo>,
    /// User-defined type aliases (e.g. `alias Float2 = Vec<2, F32>`).
    /// Maps alias name → expanded Ty so they can be resolved during type conversion.
    pub type_aliases: HashMap<String, Ty>,
    /// Trait declarations: trait_name → TraitInfo.
    pub traits: HashMap<String, TraitInfo>,
    /// Trait impls.
    pub impls: Vec<ImplInfo>,
    /// Compiler-provided builtin callable overloads.
    pub builtin_externs: HashMap<String, Vec<BuiltinExternInfo>>,
    /// Compiler-provided builtin trait impls.
    pub builtin_impls: Vec<BuiltinImplInfo>,
    /// Predicates inferred while typing the current binding.
    inferred_predicates: Vec<Predicate>,
    /// Bitfield field names: bitfield_type_name → list of field names.
    pub bitfield_field_names: HashMap<String, Vec<String>>,
}

/// Information about a data type collected during semantic analysis.
#[derive(Debug, Clone)]
pub struct DataTypeInfo {
    pub name: String,
    pub type_params: Vec<String>,
    pub constructors: Vec<String>,
}

impl SemanticAnalyzer {
    pub fn new() -> Self {
        Self {
            env: TypeEnv::new(),
            engine: InferEngine::new(),
            constructors: HashMap::new(),
            data_types: HashMap::new(),
            type_aliases: HashMap::new(),
            traits: HashMap::new(),
            impls: Vec::new(),
            builtin_externs: HashMap::new(),
            builtin_impls: Vec::new(),
            inferred_predicates: Vec::new(),
            bitfield_field_names: HashMap::new(),
        }
    }

    /// Analyze a full program.
    pub fn analyze(&mut self, program: &Program) {
        // Flatten CfgDecl nodes so we see declarations from both branches.
        // This ensures the semantic analyzer registers names from all conditional
        // compilation paths (the compiler narrows later via evaluate_features).
        let all_decls = Decl::flatten_cfg_decls(&program.decls);

        // Pass 1: predeclare type names so aliases and data declarations in the
        // same module can refer to each other regardless of source order.
        for decl in &all_decls {
            match decl {
                Decl::DataDecl {
                    name, type_params, ..
                } => {
                    self.data_types.insert(
                        name.clone(),
                        DataTypeInfo {
                            name: name.clone(),
                            type_params: type_params.clone(),
                            constructors: vec![],
                        },
                    );
                }
                Decl::BitfieldDecl { name, .. } => {
                    self.bitfield_field_names.entry(name.clone()).or_default();
                }
                _ => {}
            }
        }

        // Pass 1b: collect type aliases (treated as synonyms for semantic purposes)
        for decl in &all_decls {
            if let Decl::TypeAlias { name, ty, .. } = decl {
                let alias_ty = self.convert_syntax_type(ty);
                // Store the expanded type for alias resolution during type conversion
                self.type_aliases.insert(name.clone(), alias_ty.ty.clone());
                // Register the alias name as a type constructor
                self.env.insert(name.clone(), alias_ty);
            }
        }

        // Pass 1c: collect data types and constructors
        for decl in &all_decls {
            if let Decl::DataDecl {
                name,
                type_params,
                constructors,
                span,
                ..
            } = decl
            {
                self.register_data_type(name, type_params, constructors, *span);
            }
        }

        // Pass 1d: collect bitfield metadata
        for decl in &all_decls {
            if let Decl::BitfieldDecl {
                name,
                base_ty,
                fields,
                ..
            } = decl
            {
                let _base = self.convert_syntax_type(base_ty);
                let field_names: Vec<String> = fields.iter().map(|f| f.name.clone()).collect();
                self.bitfield_field_names.insert(name.clone(), field_names);
            }
        }

        // Pass 2: collect type signatures
        for decl in &all_decls {
            if let Decl::TypeSig {
                name,
                constraints,
                ty,
                ..
            } = decl
            {
                let inferred_ty = self.convert_syntax_type_sig(constraints, ty);
                self.env.insert(name.clone(), inferred_ty);
            }
            if let Decl::ConstDecl { name, ty, .. } = decl {
                let inferred_ty = self.convert_syntax_type(ty);
                self.env.insert(name.clone(), inferred_ty);
            }
            if let Decl::BindingDecl { name, ty, .. } = decl {
                let inferred_ty = self.convert_syntax_type(ty);
                // The type is already the inner type (no Uniform/Storage wrappers).
                self.env.insert(name.clone(), Scheme::mono(inferred_ty.ty));
            }
            if let Decl::ExternDecl { name, ty, .. } = decl {
                let inferred_ty = self.convert_syntax_type(ty);
                self.env.insert(name.clone(), inferred_ty);
            }
            if let Decl::BuiltinExternDecl {
                name,
                ty,
                lowering,
                ..
            } = decl
            {
                let inferred_ty = self.convert_syntax_type(ty);
                self.env.insert(name.clone(), inferred_ty.clone());
                self.builtin_externs
                    .entry(name.clone())
                    .or_default()
                    .push(BuiltinExternInfo {
                        name: name.clone(),
                        ty: normalize_type_aliases(&inferred_ty.ty),
                        lowering: lowering.clone(),
                    });
            }
        }

        // Pass 2b: collect trait declarations
        for decl in &all_decls {
            if let Decl::TraitDecl {
                name, vars, methods, ..
            } = decl
            {
                let var_ids: Vec<TyVarId> = vars
                    .iter()
                    .map(|_| fresh_var_id(&mut self.engine))
                    .collect();
                let mut trait_methods = Vec::new();
                for m in methods {
                    let canonical_name = canonical_trait_method_name(name, &m.name);
                    let mut scope: HashMap<String, TyVarId> = vars
                        .iter()
                        .cloned()
                        .zip(var_ids.iter().copied())
                        .collect();
                    let method_ty = self.convert_syntax_type_with_scope(&m.ty, &mut scope);
                    let scheme = Scheme::poly_with_constraints(
                        vec![Predicate {
                            trait_name: name.clone(),
                            tys: var_ids.iter().copied().map(Ty::Var).collect(),
                        }],
                        scope_vars(&scope),
                        method_ty.clone(),
                    );
                    // Register the method as a polymorphic function in the env
                    // (overrides any existing builtin operator with the same name)
                    self.env.insert(canonical_name.clone(), scheme);
                    trait_methods.push((canonical_name, method_ty));
                }
                self.traits.insert(
                    name.clone(),
                    TraitInfo {
                        name: name.clone(),
                        vars: vars.clone(),
                        var_ids,
                        methods: trait_methods,
                    },
                );
            }
        }

        // Pass 2c: collect impl declarations — generate mangled function names
        for decl in &all_decls {
            if let Decl::ImplDecl {
                trait_name,
                tys,
                methods,
                span,
                ..
            } = decl
            {
                let impl_tys: Vec<Ty> = tys
                    .iter()
                    .map(|ty| normalize_type_aliases(&self.convert_syntax_type(ty).ty))
                    .collect();
                let type_suffix = impl_tys
                    .iter()
                    .map(format_type_suffix)
                    .collect::<Vec<_>>()
                    .join("__");
                let mut impl_methods = HashMap::new();
                for m in methods {
                    let logical_name = trait_name
                        .as_deref()
                        .map(|tname| canonical_trait_method_name(tname, &m.name))
                        .unwrap_or_else(|| m.name.clone());
                    let mangled = mangle_instance_method(&logical_name, &type_suffix);
                    impl_methods.insert(logical_name.clone(), mangled.clone());

                    if let Some(tname) = trait_name {
                        // Trait impl: look up trait method signature and substitute
                        if let Some(trait_info) = self.traits.get(tname).cloned() {
                            for (tmethod_name, tmethod_ty) in trait_info.methods {
                                if tmethod_name == logical_name {
                                    let concrete_ty =
                                        replace_trait_vars(&tmethod_ty, &trait_info.var_ids, &impl_tys);
                                    if let Some(method_ty) = &m.ty {
                                        let declared_scheme = self.convert_syntax_type(method_ty);
                                        let declared_ty = self.engine.instantiate(&declared_scheme);
                                        self.engine.unify(&declared_ty, &concrete_ty, m.span);
                                    }
                                    self.env.insert(mangled.clone(), Scheme::mono(concrete_ty));
                                }
                            }
                        } else {
                            self.engine.diagnostics.push(
                                Diagnostic::error(format!(
                                    "Unknown trait '{}' in impl declaration",
                                    tname
                                ))
                                .with_label(Label::primary(*span, "unknown trait"))
                                .with_help("define the trait with `trait Name a where ...` before writing an impl for it"),
                            );
                        }
                    } else {
                        let scheme = if let Some(method_ty) = &m.ty {
                            let scheme = self.convert_syntax_type(method_ty);
                            let declared_ty = self.engine.instantiate(&scheme);
                            let expected_scheme = self
                                .standalone_impl_method_scheme(&impl_tys[0], m.params.len());
                            let expected_ty = self.engine.instantiate(&expected_scheme);
                            self.engine.unify(&declared_ty, &expected_ty, m.span);
                            scheme
                        } else {
                            self.standalone_impl_method_scheme(&impl_tys[0], m.params.len())
                        };
                        self.env.insert(mangled.clone(), scheme);
                    }
                }
                if let Some(tname) = trait_name {
                    if impl_tys.iter().any(|ty| !ty.free_vars().is_empty()) {
                        self.engine.diagnostics.push(
                            Diagnostic::error(format!(
                                "Trait impl heads must be concrete: blanket impls like `impl {} ...` are not supported",
                                tname
                            ))
                            .with_label(Label::primary(*span, "non-concrete impl head"))
                            .with_help(
                                "use a named wrapper type or a constrained generic function instead",
                            ),
                        );
                    }

                    if let Some(trait_info) = self.traits.get(tname).cloned() {
                        if trait_info.var_ids.len() != impl_tys.len() {
                            self.engine.diagnostics.push(
                                Diagnostic::error(format!(
                                    "Trait '{}' expects {} type argument(s), found {}",
                                    tname,
                                    trait_info.var_ids.len(),
                                    impl_tys.len()
                                ))
                                .with_label(Label::primary(*span, "wrong impl head arity")),
                            );
                        }
                        let missing_methods: Vec<String> = trait_info
                            .methods
                            .iter()
                            .filter_map(|(method_name, _)| {
                                (!impl_methods.contains_key(method_name))
                                    .then(|| method_name.clone())
                            })
                            .collect();
                        if !missing_methods.is_empty() {
                            self.engine.diagnostics.push(
                                Diagnostic::error(format!(
                                    "Incomplete implementation of trait '{}' for type '{}': missing method(s): {}",
                                    tname,
                                    format_impl_head(&impl_tys),
                                    missing_methods.join(", ")
                                ))
                                .with_label(Label::primary(*span, "incomplete impl"))
                                .with_help("implement all required trait methods"),
                            );
                        }
                    }

                    if self.impls.iter().any(|existing| {
                        existing.trait_name.as_deref() == Some(tname.as_str())
                            && existing.tys == impl_tys
                    }) {
                        self.engine.diagnostics.push(
                            Diagnostic::error(format!(
                                "Duplicate implementation of trait '{}' for type '{}'",
                                tname,
                                format_impl_head(&impl_tys)
                            ))
                            .with_label(Label::primary(*span, "duplicate impl"))
                            .with_help("each trait can only be implemented once per type"),
                        );
                    }
                }
                self.impls.push(ImplInfo {
                    trait_name: trait_name.clone(),
                    tys: impl_tys,
                    methods: impl_methods,
                });
            }
            if let Decl::BuiltinImplDecl {
                trait_name,
                tys,
                methods,
                span,
                ..
            } = decl
            {
                let impl_tys: Vec<Ty> = tys
                    .iter()
                    .map(|ty| self.convert_syntax_type(ty).ty)
                    .collect();
                if impl_tys.iter().any(|ty| !ty.free_vars().is_empty()) {
                    self.engine.diagnostics.push(
                        Diagnostic::error(format!(
                            "Builtin impl heads must be concrete: `builtin impl {} ...` may not contain free type variables",
                            trait_name
                        ))
                        .with_label(Label::primary(*span, "non-concrete builtin impl head")),
                    );
                }
                if let Some(trait_info) = self.traits.get(trait_name) {
                    if trait_info.var_ids.len() != impl_tys.len() {
                        self.engine.diagnostics.push(
                            Diagnostic::error(format!(
                                "Trait '{}' expects {} type argument(s), found {}",
                                trait_name,
                                trait_info.var_ids.len(),
                                impl_tys.len()
                            ))
                            .with_label(Label::primary(*span, "wrong builtin impl head arity")),
                        );
                    }
                    for (method_name, _) in &trait_info.methods {
                        if !methods.iter().any(|method| {
                            canonical_trait_method_name(trait_name, &method.name) == *method_name
                        }) {
                            self.engine.diagnostics.push(
                                Diagnostic::error(format!(
                                    "Incomplete builtin implementation of trait '{}' for type '{}': missing method(s): {}",
                                    trait_name,
                                    format_impl_head(&impl_tys),
                                    method_name
                                ))
                                .with_label(Label::primary(*span, "incomplete builtin impl")),
                            );
                        }
                    }
                }
                if self.builtin_impls.iter().any(|existing| {
                    existing.trait_name == *trait_name && existing.tys == impl_tys
                }) {
                    self.engine.diagnostics.push(
                        Diagnostic::error(format!(
                            "Duplicate builtin implementation of trait '{}' for type '{}'",
                            trait_name,
                            format_impl_head(&impl_tys)
                        ))
                        .with_label(Label::primary(*span, "duplicate builtin impl")),
                    );
                }
                let mut method_map = HashMap::new();
                for method in methods {
                    let logical_name = canonical_trait_method_name(trait_name, &method.name);
                    method_map.insert(logical_name, method.lowering.clone());
                }
                self.builtin_impls.push(BuiltinImplInfo {
                    trait_name: trait_name.clone(),
                    tys: impl_tys,
                    methods: method_map,
                });
            }
        }

        // Pass 3: type check function bodies
        for decl in &all_decls {
            match decl {
                Decl::FunDecl {
                    name,
                    params,
                    body,
                    where_binds,
                    span,
                    ..
                } => {
                    self.check_function(name, params, body, where_binds, *span);
                }
                Decl::EntryPoint {
                    name,
                    params,
                    body,
                    span,
                    ..
                } => {
                    self.check_function(name, params, body, &[], *span);
                }
                Decl::ImplDecl {
                    trait_name,
                    tys,
                    methods,
                    ..
                } => {
                    // Type-check impl method bodies
                    let impl_tys: Vec<Ty> = tys
                        .iter()
                        .map(|ty| self.convert_syntax_type(ty).ty)
                        .collect();
                    for m in methods {
                        let logical_name = trait_name
                            .as_deref()
                            .map(|tname| canonical_trait_method_name(tname, &m.name))
                            .unwrap_or_else(|| m.name.clone());
                        let type_suffix = impl_tys
                            .iter()
                            .map(format_type_suffix)
                            .collect::<Vec<_>>()
                            .join("__");
                        let mangled = mangle_instance_method(&logical_name, &type_suffix);
                        let scheme = if trait_name.is_some() {
                            let Some(scheme) = self.env.lookup(&mangled).cloned() else {
                                continue;
                            };
                            scheme
                        } else if let Some(method_ty) = &m.ty {
                            self.convert_syntax_type(method_ty)
                        } else {
                            self.standalone_impl_method_scheme(&impl_tys[0], m.params.len())
                        };
                        self.check_impl_method(
                            &logical_name,
                            &scheme,
                            &m.params,
                            &m.body,
                            m.span,
                            trait_name.is_none(),
                        );
                    }
                }
                _ => {}
            }
        }
    }

    fn register_data_type(
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

    /// Convert syntax-level Type to internal Ty.
    fn convert_syntax_type(&mut self, ty: &Type) -> Scheme {
        let mut scope = HashMap::new();
        let ty = self.convert_syntax_type_with_scope(ty, &mut scope);
        Scheme::poly(scope_vars(&scope), ty)
    }

    fn convert_syntax_type_sig(&mut self, constraints: &[TypeConstraint], ty: &Type) -> Scheme {
        let mut scope = HashMap::new();
        let predicates = constraints
            .iter()
            .map(|constraint| {
                Predicate {
                    trait_name: constraint.trait_name.clone(),
                    tys: constraint
                        .tys
                        .iter()
                        .map(|ty| self.convert_syntax_type_with_scope(ty, &mut scope))
                        .collect(),
                }
            })
            .collect();
        let ty = self.convert_syntax_type_with_scope(ty, &mut scope);
        Scheme::poly_with_constraints(predicates, scope_vars(&scope), ty)
    }

    fn convert_syntax_type_with_scope(
        &mut self,
        ty: &Type,
        scope: &mut HashMap<String, TyVarId>,
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
            Type::Nat(n, _) => Ty::Nat(*n),
            Type::Arrow(a, b, _) => {
                let a = self.convert_syntax_type_with_scope(a, scope);
                let b = self.convert_syntax_type_with_scope(b, scope);
                Ty::arrow(a, b)
            }
            Type::App(f, a, _) => {
                let f = self.convert_syntax_type_with_scope(f, scope);
                let a = self.convert_syntax_type_with_scope(a, scope);
                Ty::app(f, a)
            }
            Type::Paren(inner, _) => self.convert_syntax_type_with_scope(inner, scope),
            Type::Tuple(elems, _) => {
                if elems.is_empty() {
                    Ty::unit()
                } else {
                    Ty::Tuple(
                        elems
                            .iter()
                            .map(|e| self.convert_syntax_type_with_scope(e, scope))
                            .collect(),
                    )
                }
            }
            Type::Unit(_) => Ty::unit(),
        };
        normalize_type_aliases(&ty)
    }

    fn is_known_type_constructor(&self, name: &str) -> bool {
        matches!(
            name,
            ty_name::I32
                | ty_name::U32
                | ty_name::F32
                | ty_name::BOOL
                | ty_name::UNIT
                | ty_name::STRING
                | ty_name::VEC
                | "Vector"
                | ty_name::MAT
                | "Matrix"
                | ty_name::TENSOR
                | "Array"
                | "Ten"
                | "Scalar"
                | "Sca"
                | "Option"
                | "Options"
                | "Result"
                | "Pair"
                | ty_name::UNIFORM
                | ty_name::STORAGE
        ) || self.data_types.contains_key(name)
            || self.bitfield_field_names.contains_key(name)
            || self.type_aliases.contains_key(name)
    }

    fn standalone_impl_method_scheme(&mut self, impl_ty: &Ty, arity: usize) -> Scheme {
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

    fn new_type_var_scope(&mut self, names: &[String]) -> HashMap<String, TyVarId> {
        names
            .iter()
            .map(|name| (name.clone(), fresh_var_id(&mut self.engine)))
            .collect()
    }

    fn check_function(
        &mut self,
        name: &str,
        params: &[Pat],
        body: &Expr,
        where_binds: &[(String, Expr)],
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

        // If there's a declared type, unify with it
        if let Some(qualified) = declared.take() {
            self.engine.unify(&fun_ty, &qualified.ty, span);
            let inferred_constraints =
                self.resolve_inferred_predicates(predicate_start, &active_constraints, span);
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

    fn check_impl_method(
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

        self.engine.unify(&fun_ty, &declared.ty, span);
        let inferred_constraints =
            self.resolve_inferred_predicates(predicate_start, &active_constraints, span);
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

    fn resolve_inferred_predicates(
        &mut self,
        start: usize,
        active_constraints: &[Predicate],
        span: Span,
    ) -> Vec<Predicate> {
        let pending: Vec<Predicate> = self.inferred_predicates.drain(start..).collect();
        let mut retained = Vec::new();
        let subst = self.engine.subst.clone();
        for predicate in pending.into_iter().map(|predicate| predicate.apply_subst(&subst)) {
            self.try_improve_predicate(&predicate, span);
            let predicate = predicate.apply_subst(&self.engine.subst);
            if retained.iter().any(|existing| existing == &predicate) {
                continue;
            }
            if active_constraints.iter().any(|active| {
                active.apply_subst(&self.engine.subst) == predicate
            }) {
                continue;
            }
            if predicate.tys.iter().all(|ty| ty.free_vars().is_empty()) {
                if self.has_impl_for_predicate(&predicate) {
                    continue;
                }
                self.engine.diagnostics.push(
                    Diagnostic::error(format!(
                        "type `{}` does not implement trait `{}`",
                        format_impl_head(&predicate.tys),
                        predicate.trait_name
                    ))
                    .with_label(Label::primary(span, "missing trait implementation"))
                    .with_help(format!(
                        "define `impl {} {} where ...`",
                        predicate.trait_name,
                        format_impl_head(&predicate.tys)
                    )),
                );
                continue;
            }
            retained.push(predicate);
        }
        retained
    }

    fn has_impl_for_predicate(&self, predicate: &Predicate) -> bool {
        self.impls.iter().any(|inst| {
            inst.trait_name.as_deref() == Some(predicate.trait_name.as_str())
                && inst.tys == predicate.tys
        }) || self.builtin_impls.iter().any(|inst| {
            inst.trait_name == predicate.trait_name && inst.tys == predicate.tys
        })
    }

    fn try_improve_predicate(&mut self, predicate: &Predicate, span: Span) {
        let predicate = predicate.apply_subst(&self.engine.subst);

        if predicate.tys.iter().any(|ty| ty.free_vars().is_empty()) {
            let candidates: Vec<Vec<Ty>> = self
                .impls
                .iter()
                .filter(|inst| inst.trait_name.as_deref() == Some(predicate.trait_name.as_str()))
                .filter(|inst| inst.tys.len() == predicate.tys.len())
                .filter(|inst| predicate_matches_head(&predicate, &inst.tys))
                .map(|inst| inst.tys.clone())
                .collect();
            if candidates.len() == 1 {
                for (actual, expected) in predicate.tys.iter().zip(candidates[0].iter()) {
                    self.engine.unify(actual, expected, span);
                }
                return;
            }
            let builtin_candidates: Vec<Vec<Ty>> = self
                .builtin_impls
                .iter()
                .filter(|inst| inst.trait_name == predicate.trait_name)
                .filter(|inst| inst.tys.len() == predicate.tys.len())
                .filter(|inst| predicate_matches_head(&predicate, &inst.tys))
                .map(|inst| inst.tys.clone())
                .collect();
            if builtin_candidates.len() == 1 {
                for (actual, expected) in predicate.tys.iter().zip(builtin_candidates[0].iter()) {
                    self.engine.unify(actual, expected, span);
                }
            } else if let Some(preferred) = builtin_head_for_predicate(&predicate) {
                let preferred = preferred
                    .into_iter()
                    .map(|ty| normalize_type_aliases(&ty))
                    .collect::<Vec<_>>();
                if self.builtin_impls.iter().any(|inst| {
                    inst.trait_name == predicate.trait_name
                        && inst.tys.len() == preferred.len()
                        && inst
                            .tys
                            .iter()
                            .map(normalize_type_aliases)
                            .eq(preferred.iter().cloned())
                }) {
                    for (actual, expected) in predicate.tys.iter().zip(preferred.iter()) {
                        self.engine.unify(actual, expected, span);
                    }
                }
            }
        }
    }

    fn bind_pattern(&mut self, pat: &Pat, ty: &Ty, env: &mut TypeEnv) {
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

    fn infer_expr(
        &mut self,
        expr: &Expr,
        env: &mut TypeEnv,
        active_constraints: &[Predicate],
    ) -> Ty {
        match expr {
            Expr::Lit(lit, _) => self.lit_type(lit),

            Expr::Var(name, span) => {
                if is_internal_impl_method_name(&self.impls, name) {
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
                let func_ty = self.infer_expr(func, env, active_constraints);
                let arg_ty = self.infer_expr(arg, env, active_constraints);
                let ret_ty = self.engine.fresh_var();
                let expected = Ty::arrow(arg_ty, ret_ty.clone());
                self.engine.unify(&func_ty, &expected, *span);
                ret_ty
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
                ret_ty
            }

            Expr::Lambda(pats, body, _span) => {
                let mut local_env = env.clone();
                let mut param_types = Vec::new();
                for pat in pats {
                    let ty = self.engine.fresh_var();
                    self.bind_pattern(pat, &ty, &mut local_env);
                    param_types.push(ty);
                }
                let body_ty = self.infer_expr(body, &mut local_env, active_constraints);
                let mut result = body_ty;
                for pt in param_types.into_iter().rev() {
                    result = Ty::arrow(pt, result);
                }
                result
            }

            Expr::Let(binds, body, _span) => {
                let mut local_env = env.clone();
                for (name, expr) in binds {
                    let predicate_start = self.inferred_predicates.len();
                    let ty = self.infer_expr(expr, &mut local_env, active_constraints);
                    let inferred_constraints =
                        self.resolve_inferred_predicates(predicate_start, active_constraints, expr.span());
                    let scheme = self
                        .engine
                        .generalize_with_constraints(&local_env, &ty, &inferred_constraints);
                    local_env.insert(name.clone(), scheme);
                }
                self.infer_expr(body, &mut local_env, active_constraints)
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
                    let body_ty = self.infer_expr(body, &mut arm_env, active_constraints);
                    self.engine.unify(&result_ty, &body_ty, *span);
                }
                result_ty
            }

            Expr::If(cond, then_expr, else_expr, span) => {
                let cond_ty = self.infer_expr(cond, env, active_constraints);
                self.engine.unify(&cond_ty, &Ty::bool(), *span);
                let then_ty = self.infer_expr(then_expr, env, active_constraints);
                let else_ty = self.infer_expr(else_expr, env, active_constraints);
                self.engine.unify(&then_ty, &else_ty, *span);
                then_ty
            }

            Expr::Paren(inner, _) => self.infer_expr(inner, env, active_constraints),

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
                            // Constructor exists but has positional/empty fields, infer anyway
                            for (_, expr) in fields {
                                self.infer_expr(expr, env, active_constraints);
                            }
                        }
                        con_info.result_ty.clone()
                    } else {
                        // Unknown constructor name, fall back to old behavior
                        for (_, expr) in fields {
                            self.infer_expr(expr, env, active_constraints);
                        }
                        Ty::Con(con_name.clone())
                    }
                } else {
                    // Anonymous record
                    for (_, expr) in fields {
                        self.infer_expr(expr, env, active_constraints);
                    }
                    self.engine.fresh_var()
                }
            }

            Expr::FieldAccess(expr, field, span) => {
                let base_ty = self.infer_expr(expr, env, active_constraints);
                let base_ty = self.engine.finalize(&base_ty);

                // Check for Vec swizzle patterns
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
                }

                // Method-call syntax sugar: `x.method` → `method x`
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

                // Validate field exists on known types
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
                let _ = self.infer_expr(base, env, active_constraints);
                let _ = self.infer_expr(index, env, active_constraints);
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

                // Infer types of all elements and unify their scalar types
                let scalar_ty = self.engine.fresh_var();
                let mut total_components: u64 = 0;

                for elem in elems {
                    let elem_ty = self.infer_expr(elem, env, active_constraints);
                    let elem_ty = self.engine.finalize(&elem_ty);

                    if let Some((n, inner_scalar)) = extract_vec_type(&elem_ty) {
                        // Vec element: contributes n components
                        total_components += n as u64;
                        self.engine.unify(&scalar_ty, &inner_scalar, *span);
                    } else {
                        // Scalar element: contributes 1 component
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
                // Negation works on numeric types
                self.infer_expr(inner, env, active_constraints)
            }

            Expr::Not(inner, span) => {
                // Boolean not: operand and result are Bool
                let inner_ty = self.infer_expr(inner, env, active_constraints);
                let bool_ty = Ty::bool();
                self.engine.unify(&inner_ty, &bool_ty, *span);
                bool_ty
            }

            Expr::BitNot(inner, _span) => {
                // Bitwise not: works on integer types, result is same type
                self.infer_expr(inner, env, active_constraints)
            }

            Expr::Do(stmts, _span) => {
                let mut local_env = env.clone();
                let mut last_ty = Ty::unit();
                for stmt in stmts {
                    match stmt {
                        DoStmt::Expr(expr, _) => {
                            last_ty = self.infer_expr(expr, &mut local_env, active_constraints);
                        }
                        DoStmt::Bind(name, expr, _) => {
                            let ty = self.infer_expr(expr, &mut local_env, active_constraints);
                            // bind extracts the inner type from m a
                            let inner_ty = self.engine.fresh_var();
                            local_env.insert(name.clone(), Scheme::mono(inner_ty));
                            last_ty = ty;
                        }
                        DoStmt::Let(name, expr, _) => {
                            let ty = self.infer_expr(expr, &mut local_env, active_constraints);
                            local_env.insert(name.clone(), Scheme::mono(ty));
                        }
                    }
                }
                last_ty
            }

            Expr::Loop(loop_name, bindings, body, span) => {
                let mut loop_env = env.clone();
                let mut binding_tys = Vec::new();
                for (bind_name, init_expr) in bindings {
                    let init_ty = self.infer_expr(init_expr, env, active_constraints);
                    loop_env.insert(bind_name.clone(), Scheme::mono(init_ty.clone()));
                    binding_tys.push(init_ty);
                }
                let result_ty = self.engine.fresh_var();
                let loop_fn_ty = binding_tys.iter().rev().fold(result_ty.clone(), |acc, ty| {
                    Ty::Arrow(Box::new(ty.clone()), Box::new(acc))
                });
                loop_env.insert(loop_name.clone(), Scheme::mono(loop_fn_ty));
                let body_ty = self.infer_expr(body, &mut loop_env, active_constraints);
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
        }
    }

    fn lit_type(&self, lit: &Lit) -> Ty {
        match lit {
            Lit::Int(_) => Ty::i32(),
            Lit::UInt(_) => Ty::u32(),
            Lit::Float(_) => Ty::f32(),
            Lit::String(_) => Ty::Con(ty_name::STRING.into()),
            Lit::Char(_) => Ty::Con("Char".into()),
        }
    }

    pub fn has_errors(&self) -> bool {
        self.engine.diagnostics.has_errors()
    }

    pub fn diagnostics(&self) -> &DiagnosticSink {
        &self.engine.diagnostics
    }

}

fn desugar_where(body: &Expr, where_binds: &[(String, Expr)], span: Span) -> Expr {
    if where_binds.is_empty() {
        body.clone()
    } else {
        Expr::Let(where_binds.to_vec(), Box::new(body.clone()), span)
    }
}

impl Default for SemanticAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

/// Produce a short suffix string from a Ty for name-mangling.
pub fn format_type_suffix(ty: &Ty) -> String {
    match ty {
        Ty::Con(name) => name.clone(),
        Ty::App(f, a) => format!("{}_{}", format_type_suffix(f), format_type_suffix(a)),
        Ty::Nat(n) => format!("{}", n),
        _ => "unknown".to_string(),
    }
}

/// Map operator symbols to readable names for mangling.
pub fn sanitise_operator_name(name: &str) -> String {
    match name {
        "+" => "add".to_string(),
        "-" => "sub".to_string(),
        "*" => "mul".to_string(),
        "/" => "div".to_string(),
        "%" => "mod".to_string(),
        "==" => "eq".to_string(),
        "/=" => "ne".to_string(),
        "<" => "lt".to_string(),
        ">" => "gt".to_string(),
        "<=" => "le".to_string(),
        ">=" => "ge".to_string(),
        "&&" => "and".to_string(),
        "||" => "or".to_string(),
        "&" => "bitand".to_string(),
        "^" => "bitxor".to_string(),
        "<<" => "shl".to_string(),
        ">>" => "shr".to_string(),
        _ => name.replace(|c: char| !c.is_alphanumeric() && c != '_', "_"),
    }
}

/// Mangle an instance method name: `(+)` for F32 → `add_F32`, `scale` for F32 → `scale_F32`.
pub fn mangle_instance_method(method_name: &str, type_suffix: &str) -> String {
    let sanitised = sanitise_operator_name(method_name);
    format!("{}_{}", sanitised, type_suffix)
}

fn canonical_trait_method_name(trait_name: &str, method_name: &str) -> String {
    match (trait_name, method_name) {
        ("Neg", "-") => "negate".to_owned(),
        ("BitNot", "~") => "bitnot".to_owned(),
        ("Shr", ">>") => "shr".to_owned(),
        _ => method_name.to_owned(),
    }
}

fn resolve_impl_method_name(impls: &[ImplInfo], name: &str, receiver_ty: &Ty) -> Option<String> {
    impls
        .iter()
        .filter(|inst| inst.tys.len() == 1 && inst.tys[0] == *receiver_ty)
        .find_map(|inst| inst.methods.get(name).cloned())
}

pub fn is_internal_impl_method_name(impls: &[ImplInfo], name: &str) -> bool {
    impls
        .iter()
        .any(|inst| inst.methods.values().any(|mangled| mangled == name))
}

fn resolve_unique_standalone_impl_method_name(impls: &[ImplInfo], name: &str) -> Option<String> {
    let mut matches = impls
        .iter()
        .filter(|inst| inst.trait_name.is_none())
        .filter_map(|inst| inst.methods.get(name).cloned());
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

/// Resolve the callable target for dot-call syntax.
///
/// Matching impl methods win when the receiver type is already concrete.
/// Otherwise, any in-scope function remains callable with dot syntax so the
/// WGSL-oriented prelude keeps working naturally.
pub fn resolve_dot_call_target(
    env: &TypeEnv,
    impls: &[ImplInfo],
    name: &str,
    receiver_ty: &Ty,
) -> Option<String> {
    resolve_impl_method_name(impls, name, receiver_ty)
        .or_else(|| resolve_unique_standalone_impl_method_name(impls, name))
        .or_else(|| env.lookup(name).map(|_| name.to_string()))
}

fn format_predicate(predicate: &Predicate) -> String {
    format!("{} {}", predicate.trait_name, format_impl_head(&predicate.tys))
}

fn format_constraints(predicates: &[Predicate]) -> String {
    predicates
        .iter()
        .map(format_predicate)
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_impl_head(tys: &[Ty]) -> String {
    tys.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

fn predicate_matches_head(predicate: &Predicate, head: &[Ty]) -> bool {
    predicate.tys.len() == head.len()
        && predicate.tys.iter().zip(head.iter()).all(|(actual, expected)| {
            let actual = normalize_type_aliases(actual);
            let expected = normalize_type_aliases(expected);
            actual == expected || !actual.free_vars().is_empty()
        })
}

fn builtin_head_for_predicate(predicate: &Predicate) -> Option<Vec<Ty>> {
    use shadml_typechecker::ty_name;

    fn scalar_numeric_name(ty: &Ty) -> Option<&str> {
        match ty {
            Ty::Con(name) if matches!(name.as_str(), ty_name::F32 | ty_name::I32 | ty_name::U32) => {
                Some(name.as_str())
            }
            _ => None,
        }
    }

    fn same_or_var(a: &Ty, b: &Ty) -> bool {
        normalize_type_aliases(a) == normalize_type_aliases(b)
            || !a.free_vars().is_empty()
            || !b.free_vars().is_empty()
    }

    fn first_concrete(tys: &[&Ty]) -> Option<Ty> {
        tys.iter()
            .find(|ty| ty.free_vars().is_empty())
            .map(|ty| normalize_type_aliases(ty))
    }

    if !predicate.tys.iter().any(|ty| ty.free_vars().is_empty()) {
        return None;
    }

    match (predicate.trait_name.as_str(), predicate.tys.as_slice()) {
        ("Add", [a, b, c])
        | ("Sub", [a, b, c])
        | ("Div", [a, b, c])
        | ("Mod", [a, b, c])
        | ("BitAnd", [a, b, c])
        | ("BitXor", [a, b, c]) => {
            for candidate in [Ty::f32(), Ty::i32(), Ty::u32()] {
                if same_or_var(a, &candidate) && same_or_var(b, &candidate) && same_or_var(c, &candidate) {
                    return Some(vec![candidate.clone(), candidate.clone(), candidate]);
                }
            }
            if same_or_var(a, b) && same_or_var(b, c) {
                if let Some(vec_ty) = a
                    .free_vars()
                    .is_empty()
                    .then(|| normalize_type_aliases(a))
                    .or_else(|| b.free_vars().is_empty().then(|| normalize_type_aliases(b)))
                    .or_else(|| c.free_vars().is_empty().then(|| normalize_type_aliases(c)))
                {
                    if extract_vec_type(&vec_ty).is_some() || extract_mat_type(&vec_ty).is_some() {
                        return Some(vec![vec_ty.clone(), vec_ty.clone(), vec_ty]);
                    }
                }
            }
            None
        }
        ("Mul", [a, b, c]) => {
            for candidate in [Ty::f32(), Ty::i32(), Ty::u32()] {
                if same_or_var(a, &candidate) && same_or_var(b, &candidate) && same_or_var(c, &candidate) {
                    return Some(vec![candidate.clone(), candidate.clone(), candidate]);
                }
            }
            if let Some(lhs) = first_concrete(&[a, b, c]) {
                if same_or_var(&lhs, b) && same_or_var(&lhs, c) && (extract_vec_type(&lhs).is_some() || extract_mat_type(&lhs).is_some()) {
                    return Some(vec![lhs.clone(), lhs.clone(), lhs]);
                }
            }
            if let Some((_, elem)) = extract_vec_type(a) {
                if scalar_numeric_name(b) == scalar_numeric_name(&elem) && same_or_var(a, c) {
                    return Some(vec![normalize_type_aliases(a), elem.clone(), normalize_type_aliases(c)]);
                }
            }
            if let Some((_, elem)) = extract_vec_type(b) {
                if scalar_numeric_name(a) == scalar_numeric_name(&elem) && same_or_var(b, c) {
                    return Some(vec![elem.clone(), normalize_type_aliases(b), normalize_type_aliases(c)]);
                }
            }
            if let Some((_, _, elem)) = extract_mat_type(a) {
                if scalar_numeric_name(b) == scalar_numeric_name(&elem) && same_or_var(a, c) {
                    return Some(vec![normalize_type_aliases(a), elem.clone(), normalize_type_aliases(c)]);
                }
            }
            if let Some((_, _, elem)) = extract_mat_type(b) {
                if scalar_numeric_name(a) == scalar_numeric_name(&elem) && same_or_var(b, c) {
                    return Some(vec![elem.clone(), normalize_type_aliases(b), normalize_type_aliases(c)]);
                }
            }
            if let (Some((rows, cols, elem_a)), Some((cols_b, elem_b)), Some((rows_c, elem_c))) =
                (extract_mat_type(a), extract_vec_type(b), extract_vec_type(c))
            {
                if cols == cols_b && rows == rows_c && elem_a == elem_b && elem_b == elem_c {
                    return Some(vec![normalize_type_aliases(a), normalize_type_aliases(b), normalize_type_aliases(c)]);
                }
            }
            None
        }
        ("Shl", [a, b, c]) | ("Shr", [a, b, c]) => {
            for lhs in [Ty::i32(), Ty::u32()] {
                for rhs in [Ty::i32(), Ty::u32()] {
                    if same_or_var(a, &lhs) && same_or_var(b, &rhs) && same_or_var(c, &lhs) {
                        return Some(vec![lhs.clone(), rhs.clone(), lhs]);
                    }
                }
            }
            None
        }
        ("Neg", [a]) | ("BitNot", [a]) => {
            for candidate in [Ty::f32(), Ty::i32(), Ty::u32()] {
                if same_or_var(a, &candidate) {
                    return Some(vec![candidate]);
                }
            }
            None
        }
        _ => None,
    }
}

pub fn replace_trait_vars(ty: &Ty, trait_vars: &[TyVarId], replacements: &[Ty]) -> Ty {
    match ty {
        Ty::Var(var) => trait_vars
            .iter()
            .position(|trait_var| trait_var == var)
            .and_then(|idx| replacements.get(idx))
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        Ty::Con(_) | Ty::Nat(_) | Ty::Error => ty.clone(),
        Ty::App(f, a) => Ty::App(
            Box::new(replace_trait_vars(f, trait_vars, replacements)),
            Box::new(replace_trait_vars(a, trait_vars, replacements)),
        ),
        Ty::Arrow(a, b) => Ty::Arrow(
            Box::new(replace_trait_vars(a, trait_vars, replacements)),
            Box::new(replace_trait_vars(b, trait_vars, replacements)),
        ),
        Ty::Tuple(elems) => Ty::Tuple(
            elems
                .iter()
                .map(|e| replace_trait_vars(e, trait_vars, replacements))
                .collect(),
        ),
        Ty::Forall(vars, body) => Ty::Forall(
            vars.clone(),
            Box::new(replace_trait_vars(body, trait_vars, replacements)),
        ),
    }
}

fn fresh_var_id(engine: &mut InferEngine) -> TyVarId {
    match engine.fresh_var() {
        Ty::Var(id) => id,
        _ => unreachable!(),
    }
}

fn scope_vars(scope: &HashMap<String, TyVarId>) -> Vec<TyVarId> {
    let mut vars: Vec<_> = scope.values().copied().collect();
    vars.sort_unstable();
    vars.dedup();
    vars
}

fn apply_type_params(name: &str, type_params: &[String], scope: &HashMap<String, TyVarId>) -> Ty {
    type_params
        .iter()
        .fold(Ty::Con(name.to_string()), |ty, param| {
            let var = scope
                .get(param)
                .copied()
                .expect("type parameter should exist in scope");
            Ty::app(ty, Ty::Var(var))
        })
}

// ── Swizzle / Vec helpers ─────────────────────────────────────────────

/// Check if a field name is a valid swizzle pattern (xyzw or rgba, 1-4 chars).
pub fn is_swizzle(field: &str) -> bool {
    if field.is_empty() || field.len() > 4 {
        return false;
    }
    let all_xyzw = field.chars().all(|c| matches!(c, 'x' | 'y' | 'z' | 'w'));
    let all_rgba = field.chars().all(|c| matches!(c, 'r' | 'g' | 'b' | 'a'));
    all_xyzw || all_rgba
}

/// Get the component index for a swizzle character.
fn swizzle_index(c: char) -> usize {
    match c {
        'x' | 'r' => 0,
        'y' | 'g' => 1,
        'z' | 'b' => 2,
        'w' | 'a' => 3,
        _ => 0,
    }
}

/// Validate that all swizzle components are within bounds for a Vec of size n.
pub fn validate_swizzle(field: &str, n: u8) -> bool {
    field.chars().all(|c| swizzle_index(c) < n as usize)
}

/// Extract Vec type info: Vec n T → Some((n, T))
pub fn extract_vec_type(ty: &Ty) -> Option<(u8, Ty)> {
    let ty = normalize_type_aliases(ty);

    // Vec n T = App(App(Con("Vec"), Nat(n)), T)
    if let Ty::App(f, scalar) = &ty {
        if let Ty::App(con, nat) = f.as_ref() {
            if let (Ty::Con(name), Ty::Nat(n)) = (con.as_ref(), nat.as_ref()) {
                if name == ty_name::VEC {
                    return Some((*n as u8, scalar.as_ref().clone()));
                }
            }
        }
    }
    None
}

/// Extract Mat type info: Mat r c T -> Some((r, c, T))
pub fn extract_mat_type(ty: &Ty) -> Option<(u8, u8, Ty)> {
    let ty = normalize_type_aliases(ty);

    if let Ty::App(f, scalar) = &ty {
        if let Ty::App(g, cols) = f.as_ref() {
            if let Ty::App(con, rows) = g.as_ref() {
                if let (Ty::Con(name), Ty::Nat(r), Ty::Nat(c)) =
                    (con.as_ref(), rows.as_ref(), cols.as_ref())
                {
                    if name == ty_name::MAT {
                        return Some((*r as u8, *c as u8, scalar.as_ref().clone()));
                    }
                }
            }
        }
    }
    None
}

fn function_arity(ty: &Ty) -> usize {
    let mut arity = 0;
    let mut cursor = ty;
    while let Ty::Arrow(_, to) = cursor {
        arity += 1;
        cursor = to;
    }
    arity
}

#[cfg(test)]
mod tests {
    use super::*;
    use shadml_span::Span;

    fn span() -> Span {
        Span::new(0, 0)
    }

    fn with_prelude(program: &mut Program) {
        let prelude = shadml_parser::prelude_program();
        let mut combined = prelude.decls.clone();
        combined.append(&mut program.decls);
        program.decls = combined;
    }

    #[test]
    fn test_empty_program() {
        let mut sa = SemanticAnalyzer::new();
        let mut program = Program { decls: vec![] };
        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(!sa.has_errors());
    }

    #[test]
    fn test_simple_function_inference() {
        let mut sa = SemanticAnalyzer::new();
        // f x = x + 1
        let mut program = Program {
            decls: vec![Decl::FunDecl {
                name: "f".into(),
                params: vec![Pat::Var("x".into(), span())],
                body: Expr::Infix(
                    Box::new(Expr::Var("x".into(), span())),
                    "+".into(),
                    Box::new(Expr::Lit(Lit::Int(1), span())),
                    span(),
                ),
                where_binds: vec![],
                span: span(),
                comments: vec![],
            }],
        };
        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(!sa.has_errors());
        // f should have type I32 -> I32
        let scheme = sa.env.lookup("f").expect("f should be in env");
        let ty = sa.engine.finalize(&scheme.ty);
        assert_eq!(format!("{}", ty), "(I32 -> I32)");
    }

    #[test]
    fn test_data_type_registration() {
        let mut sa = SemanticAnalyzer::new();
        // data Color = Red | Green | Blue
        let mut program = Program {
            decls: vec![Decl::DataDecl {
                name: "Color".into(),
                type_params: vec![],
                constructors: vec![
                    ConDecl {
                        name: "Red".into(),
                        fields: ConFields::Empty,
                        discriminant: None,
                        span: span(),
                        doc: None,
                    },
                    ConDecl {
                        name: "Green".into(),
                        fields: ConFields::Empty,
                        discriminant: None,
                        span: span(),
                        doc: None,
                    },
                    ConDecl {
                        name: "Blue".into(),
                        fields: ConFields::Empty,
                        discriminant: None,
                        span: span(),
                        doc: None,
                    },
                ],
                span: span(),
                comments: vec![],
            }],
        };
        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(!sa.has_errors());
        assert!(sa.constructors.contains_key("Red"));
        assert!(sa.constructors.contains_key("Green"));
        assert!(sa.constructors.contains_key("Blue"));
        assert_eq!(sa.constructors["Red"].tag, 0);
        assert_eq!(sa.constructors["Green"].tag, 1);
        assert_eq!(sa.constructors["Blue"].tag, 2);
    }

    #[test]
    fn test_unbound_variable_error() {
        let mut sa = SemanticAnalyzer::new();
        // f x = y  (y is unbound)
        let mut program = Program {
            decls: vec![Decl::FunDecl {
                name: "f".into(),
                params: vec![Pat::Var("x".into(), span())],
                body: Expr::Var("y".into(), span()),
                where_binds: vec![],
                span: span(),
                comments: vec![],
            }],
        };
        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(sa.has_errors());
    }

    #[test]
    fn test_type_signature_check() {
        let mut sa = SemanticAnalyzer::new();
        // add :: I32 -> I32 -> I32
        // add x y = x + y
        let mut program = Program {
            decls: vec![
                Decl::TypeSig {
                    name: "add".into(),
                    constraints: vec![],
                    ty: Type::Arrow(
                        Box::new(Type::Con("I32".into(), span())),
                        Box::new(Type::Arrow(
                            Box::new(Type::Con("I32".into(), span())),
                            Box::new(Type::Con("I32".into(), span())),
                            span(),
                        )),
                        span(),
                    ),
                    span: span(),
                    comments: vec![],
                },
                Decl::FunDecl {
                    name: "add".into(),
                    params: vec![Pat::Var("x".into(), span()), Pat::Var("y".into(), span())],
                    body: Expr::Infix(
                        Box::new(Expr::Var("x".into(), span())),
                        "+".into(),
                        Box::new(Expr::Var("y".into(), span())),
                        span(),
                    ),
                    where_binds: vec![],
                    span: span(),
                    comments: vec![],
                },
            ],
        };
        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(!sa.has_errors());
    }

    #[test]
    fn test_generic_type_signature_reuses_type_variable() {
        let mut sa = SemanticAnalyzer::new();
        let mut program = Program {
            decls: vec![
                Decl::TypeSig {
                    name: "id".into(),
                    constraints: vec![],
                    ty: Type::Arrow(
                        Box::new(Type::Var("a".into(), span())),
                        Box::new(Type::Var("a".into(), span())),
                        span(),
                    ),
                    span: span(),
                    comments: vec![],
                },
                Decl::FunDecl {
                    name: "id".into(),
                    params: vec![Pat::Var("x".into(), span())],
                    body: Expr::Var("x".into(), span()),
                    where_binds: vec![],
                    span: span(),
                    comments: vec![],
                },
            ],
        };

        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(!sa.has_errors());

        let scheme = sa.env.lookup("id").expect("id should be in env");
        assert_eq!(scheme.vars.len(), 1);
        assert!(matches!(
            &scheme.ty,
            Ty::Arrow(from, to) if from.as_ref() == to.as_ref()
        ));
    }

    #[test]
    fn test_standalone_impl_method_signature_is_checked() {
        let mut sa = SemanticAnalyzer::new();
        let mut program = Program {
            decls: vec![Decl::ImplDecl {
                trait_name: None,
                tys: vec![Type::Con("F32".into(), span())],
                methods: vec![ImplMethod {
                    name: "half".into(),
                    ty: Some(Type::Arrow(
                        Box::new(Type::Con("F32".into(), span())),
                        Box::new(Type::Con("F32".into(), span())),
                        span(),
                    )),
                    params: vec![Pat::Var("x".into(), span())],
                    body: Expr::Lit(Lit::String("nope".into()), span()),
                    span: span(),
                }],
                span: span(),
                comments: vec![],
            }],
        };
        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(
            sa.has_errors(),
            "standalone impl method bodies should be checked against impl-local signatures"
        );
    }

    #[test]
    fn test_tuple_parameter_signature_requires_one_tuple_parameter() {
        let mut sa = SemanticAnalyzer::new();
        let mut program = Program {
            decls: vec![
                Decl::TypeSig {
                    name: "test".into(),
                    constraints: vec![],
                    ty: Type::Arrow(
                        Box::new(Type::Tuple(
                            vec![
                                Type::Con("I32".into(), span()),
                                Type::Con("I32".into(), span()),
                            ],
                            span(),
                        )),
                        Box::new(Type::Con("I32".into(), span())),
                        span(),
                    ),
                    span: span(),
                    comments: vec![],
                },
                Decl::FunDecl {
                    name: "test".into(),
                    params: vec![Pat::Var("a".into(), span()), Pat::Var("b".into(), span())],
                    body: Expr::Infix(
                        Box::new(Expr::Var("a".into(), span())),
                        "+".into(),
                        Box::new(Expr::Var("b".into(), span())),
                        span(),
                    ),
                    where_binds: vec![],
                    span: span(),
                    comments: vec![],
                },
            ],
        };
        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(sa.has_errors());
        assert!(sa
            .diagnostics()
            .iter()
            .any(|diag| diag.message.contains("function `test` has 2 parameters but its type signature expects 1")));
    }

    #[test]
    fn test_tuple_parameter_signature_accepts_tuple_pattern() {
        let mut sa = SemanticAnalyzer::new();
        let mut program = Program {
            decls: vec![
                Decl::TypeSig {
                    name: "test".into(),
                    constraints: vec![],
                    ty: Type::Arrow(
                        Box::new(Type::Tuple(
                            vec![
                                Type::Con("I32".into(), span()),
                                Type::Con("I32".into(), span()),
                            ],
                            span(),
                        )),
                        Box::new(Type::Con("I32".into(), span())),
                        span(),
                    ),
                    span: span(),
                    comments: vec![],
                },
                Decl::FunDecl {
                    name: "test".into(),
                    params: vec![Pat::Tuple(
                        vec![Pat::Var("a".into(), span()), Pat::Var("b".into(), span())],
                        span(),
                    )],
                    body: Expr::Infix(
                        Box::new(Expr::Var("a".into(), span())),
                        "+".into(),
                        Box::new(Expr::Var("b".into(), span())),
                        span(),
                    ),
                    where_binds: vec![],
                    span: span(),
                    comments: vec![],
                },
            ],
        };
        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(!sa.has_errors());
    }

    #[test]
    fn test_unknown_type_constructor_produces_error() {
        let mut sa = SemanticAnalyzer::new();
        let mut program = Program {
            decls: vec![Decl::DataDecl {
                name: "ParticleState".into(),
                type_params: vec![],
                constructors: vec![ConDecl {
                    name: "Active".into(),
                    fields: ConFields::Record(vec![RecordField {
                        name: "position".into(),
                        ty: Type::Con("Vec3F".into(), span()),
                        attributes: vec![],
                        doc: None,
                    }]),
                    discriminant: None,
                    span: span(),
                    doc: None,
                }],
                span: span(),
                comments: vec![],
            }],
        };
        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(
            sa.has_errors(),
            "undeclared type constructors should be rejected"
        );
        assert!(sa
            .diagnostics()
            .iter()
            .any(|diag| diag.message.contains("Unknown type 'Vec3F'")));
    }

    #[test]
    fn test_standalone_impl_receiver_must_match_impl_type() {
        let mut sa = SemanticAnalyzer::new();
        let mut program = Program {
            decls: vec![Decl::ImplDecl {
                trait_name: None,
                tys: vec![Type::Con("ParticleState".into(), span())],
                methods: vec![ImplMethod {
                    name: "sex".into(),
                    ty: Some(Type::Arrow(
                        Box::new(Type::Con("F32".into(), span())),
                        Box::new(Type::Con("F32".into(), span())),
                        span(),
                    )),
                    params: vec![Pat::Var("x".into(), span())],
                    body: Expr::Var("x".into(), span()),
                    span: span(),
                }],
                span: span(),
                comments: vec![],
            }],
        };
        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(
            sa.has_errors(),
            "standalone impl methods should use the impl type as their first parameter"
        );
    }

    #[test]
    fn test_same_file_alias_is_visible_in_data_decl() {
        let mut sa = SemanticAnalyzer::new();
        let mut program = Program {
            decls: vec![
                Decl::TypeAlias {
                    name: "Vec3F".into(),
                    params: vec![],
                    ty: Type::App(
                        Box::new(Type::App(
                            Box::new(Type::Con("Vec".into(), span())),
                            Box::new(Type::Nat(3, span())),
                            span(),
                        )),
                        Box::new(Type::Con("F32".into(), span())),
                        span(),
                    ),
                    span: span(),
                    comments: vec![],
                },
                Decl::DataDecl {
                    name: "ParticleState".into(),
                    type_params: vec![],
                    constructors: vec![ConDecl {
                        name: "Active".into(),
                        fields: ConFields::Record(vec![RecordField {
                            name: "position".into(),
                            ty: Type::Con("Vec3F".into(), span()),
                            attributes: vec![],
                            doc: None,
                        }]),
                        discriminant: None,
                        span: span(),
                        doc: None,
                    }],
                    span: span(),
                    comments: vec![],
                },
            ],
        };
        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(
            !sa.has_errors(),
            "aliases declared in the same module should be usable inside data declarations"
        );
    }

    #[test]
    fn test_standalone_impl_method_is_not_callable_as_plain_function() {
        let mut sa = SemanticAnalyzer::new();
        let mut program = Program {
            decls: vec![
                Decl::ImplDecl {
                    trait_name: None,
                    tys: vec![Type::Con("F32".into(), span())],
                    methods: vec![ImplMethod {
                        name: "half".into(),
                        ty: Some(Type::Arrow(
                            Box::new(Type::Con("F32".into(), span())),
                            Box::new(Type::Con("F32".into(), span())),
                            span(),
                        )),
                        params: vec![Pat::Var("x".into(), span())],
                        body: Expr::Var("x".into(), span()),
                        span: span(),
                    }],
                    span: span(),
                    comments: vec![],
                },
                Decl::FunDecl {
                    name: "bad".into(),
                    params: vec![],
                    body: Expr::App(
                        Box::new(Expr::Var("half".into(), span())),
                        Box::new(Expr::Lit(Lit::Float(1.0), span())),
                        span(),
                    ),
                    where_binds: vec![],
                    span: span(),
                    comments: vec![],
                },
            ],
        };
        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(
            sa.has_errors(),
            "standalone impl methods should only be callable through method syntax"
        );
    }

    #[test]
    fn test_internal_impl_mangled_name_is_not_callable_from_source() {
        let source = r#"
impl F32 where
  half : F32 -> F32
  half x = x

bad = half_F32 1.0
"#;
        let mut parser = Parser::new(source);
        let mut program = parser.parse_program();
        with_prelude(&mut program);

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);
        assert!(sa.has_errors(), "mangled impl names should stay internal");
        assert!(sa
            .diagnostics()
            .iter()
            .any(|diag| diag.message.contains("Internal impl method `half_F32`")));
    }

    #[test]
    fn test_record_pattern_unknown_field_produces_error() {
        let source = r#"
data ParticleState
  = Active { life : F32 }
  | Dead

f particle = match particle
  | Active { lif, .. } -> lif
  | Dead -> 0.0
"#;
        let mut parser = Parser::new(source);
        let mut program = parser.parse_program();
        with_prelude(&mut program);

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);
        assert!(
            sa.has_errors(),
            "unknown record-pattern fields should be rejected"
        );
        assert!(sa.diagnostics().iter().any(|diag| diag
            .message
            .contains("no field `lif` on constructor `Active`")));
    }

    #[test]
    fn test_standalone_impl_method_remains_callable_with_dot_syntax() {
        let mut sa = SemanticAnalyzer::new();
        let mut program = Program {
            decls: vec![
                Decl::ImplDecl {
                    trait_name: None,
                    tys: vec![Type::Con("F32".into(), span())],
                    methods: vec![ImplMethod {
                        name: "half".into(),
                        ty: Some(Type::Arrow(
                            Box::new(Type::Con("F32".into(), span())),
                            Box::new(Type::Con("F32".into(), span())),
                            span(),
                        )),
                        params: vec![Pat::Var("x".into(), span())],
                        body: Expr::Var("x".into(), span()),
                        span: span(),
                    }],
                    span: span(),
                    comments: vec![],
                },
                Decl::FunDecl {
                    name: "ok".into(),
                    params: vec![Pat::Var("x".into(), span())],
                    body: Expr::FieldAccess(
                        Box::new(Expr::Var("x".into(), span())),
                        "half".into(),
                        span(),
                    ),
                    where_binds: vec![],
                    span: span(),
                    comments: vec![],
                },
            ],
        };
        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(
            !sa.has_errors(),
            "standalone impl methods should still resolve through dot syntax"
        );
    }

    #[test]
    fn test_dot_syntax_accepts_prelude_functions() {
        let mut sa = SemanticAnalyzer::new();
        let mut program = Program {
            decls: vec![
                Decl::TypeSig {
                    name: "apply".into(),
                    constraints: vec![],
                    ty: Type::Arrow(
                        Box::new(Type::Con("F32".into(), span())),
                        Box::new(Type::Con("F32".into(), span())),
                        span(),
                    ),
                    span: span(),
                    comments: vec![],
                },
                Decl::FunDecl {
                    name: "apply".into(),
                    params: vec![Pat::Var("x".into(), span())],
                    body: Expr::FieldAccess(
                        Box::new(Expr::Var("x".into(), span())),
                        "sin".into(),
                        span(),
                    ),
                    where_binds: vec![],
                    span: span(),
                    comments: vec![],
                },
            ],
        };
        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(
            !sa.has_errors(),
            "dot syntax should continue to accept prelude functions"
        );
    }

    #[test]
    fn test_missing_trait_constraint_produces_error() {
        let source = r#"
trait Light a where
  position : a -> Vec<3, F32>

data PointLight = PointLight {
  lightPosition : Vec<3, F32>
}

impl Light PointLight where
  position light = light.lightPosition

lighting : a -> Vec<3, F32>
lighting light = position light
"#;
        let mut parser = shadml_parser::parser::Parser::new(source);
        let mut program = parser.parse_program();
        with_prelude(&mut program);
        assert!(!parser.diagnostics().has_errors());

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);
        assert!(
            sa.has_errors(),
            "trait method use without an explicit constraint should fail"
        );
    }

    #[test]
    fn test_explicit_trait_constraint_allows_generic_method_use() {
        let source = r#"
trait Light a where
  position : a -> Vec<3, F32>

data PointLight = PointLight {
  lightPosition : Vec<3, F32>
}

impl Light PointLight where
  position light = light.lightPosition

lighting : Light a => a -> Vec<3, F32>
lighting light = position light
"#;
        let mut parser = shadml_parser::parser::Parser::new(source);
        let mut program = parser.parse_program();
        with_prelude(&mut program);
        assert!(!parser.diagnostics().has_errors());

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);
        assert!(
            !sa.has_errors(),
            "explicit trait constraint should allow trait method use"
        );
    }

    #[test]
    fn test_duplicate_impl_produces_error() {
        let source = r#"
trait Light a where
  position : a -> I32

data SpotLight = SpotLight

impl Light SpotLight where
  position light = 1

impl Light SpotLight where
  position light = 2
"#;
        let mut parser = shadml_parser::parser::Parser::new(source);
        let mut program = parser.parse_program();
        with_prelude(&mut program);
        assert!(!parser.diagnostics().has_errors());

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);

        assert!(sa.has_errors());
        assert!(sa.diagnostics().iter().any(|diag| diag
            .message
            .contains("Duplicate implementation of trait 'Light'")));
    }

    #[test]
    fn test_incomplete_impl_produces_error() {
        let source = r#"
trait Light a where
  position : a -> I32
  color : a -> I32
  illumination : a -> I32

data SpotLight = SpotLight

impl Light SpotLight where
  position light = 1
  illumination light = 2
"#;
        let mut parser = shadml_parser::parser::Parser::new(source);
        let mut program = parser.parse_program();
        with_prelude(&mut program);
        assert!(!parser.diagnostics().has_errors());

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);

        assert!(sa.has_errors());
        assert!(sa.diagnostics().iter().any(|diag| {
            diag.message
                .contains("Incomplete implementation of trait 'Light'")
                && diag.message.contains("color")
        }));
    }

    #[test]
    fn test_complete_impl_no_error() {
        let source = r#"
trait Light a where
  position : a -> I32
  color : a -> I32
  illumination : a -> I32

data SpotLight = SpotLight

impl Light SpotLight where
  position light = 1
  color light = 2
  illumination light = 3
"#;
        let mut parser = shadml_parser::parser::Parser::new(source);
        let mut program = parser.parse_program();
        with_prelude(&mut program);
        assert!(!parser.diagnostics().has_errors());

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);

        assert!(!sa.diagnostics().iter().any(|diag| {
            diag.message.contains("Incomplete implementation")
                || diag.message.contains("Duplicate implementation")
        }));
    }

    #[test]
    fn test_blanket_trait_impl_produces_error() {
        let source = r#"
trait Convert a where
  convert : a -> I32

impl Convert (Vec<3, a>) where
  convert _ = 1
"#;
        let mut parser = shadml_parser::parser::Parser::new(source);
        let mut program = parser.parse_program();
        with_prelude(&mut program);
        assert!(!parser.diagnostics().has_errors());

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);

        assert!(sa.has_errors());
        assert!(sa.diagnostics().iter().any(|diag| {
            diag.message.contains("Trait impl heads must be concrete")
                && diag.message.contains("blanket impls")
        }));
    }

    #[test]
    fn test_overlapping_impl_produces_error() {
        let source = r#"
trait Convert a where
  convert : a -> I32

impl Convert (Vec<3, a>) where
  convert _ = 1

impl Convert (Vec<3, F32>) where
  convert _ = 2
"#;
        let mut parser = shadml_parser::parser::Parser::new(source);
        let mut program = parser.parse_program();
        with_prelude(&mut program);
        assert!(!parser.diagnostics().has_errors());

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);

        assert!(sa.has_errors());
        assert!(sa.diagnostics().iter().any(|diag| {
            diag.message.contains("Trait impl heads must be concrete")
                && diag.message.contains("blanket impls")
        }));
    }

    #[test]
    fn test_generic_constructor_pattern_inference() {
        let mut sa = SemanticAnalyzer::new();
        let mut program = Program {
            decls: vec![
                Decl::DataDecl {
                    name: "Box".into(),
                    type_params: vec!["a".into()],
                    constructors: vec![ConDecl {
                        name: "Box".into(),
                        fields: ConFields::Positional(vec![Type::Var("a".into(), span())]),
                        discriminant: None,
                        span: span(),
                        doc: None,
                    }],
                    span: span(),
                    comments: vec![],
                },
                Decl::FunDecl {
                    name: "unbox".into(),
                    params: vec![Pat::Con(
                        "Box".into(),
                        vec![Pat::Var("x".into(), span())],
                        span(),
                    )],
                    body: Expr::Var("x".into(), span()),
                    where_binds: vec![],
                    span: span(),
                    comments: vec![],
                },
            ],
        };

        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(!sa.has_errors());

        let constructor = sa
            .env
            .lookup("Box")
            .expect("Box constructor should be in env");
        assert_eq!(constructor.vars.len(), 1);

        let scheme = sa.env.lookup("unbox").expect("unbox should be in env");
        assert_eq!(scheme.vars.len(), 1);
        assert!(format!("{}", scheme.ty).contains("Box"));
    }

    #[test]
    fn test_phantom_constructor_is_polymorphic() {
        let mut sa = SemanticAnalyzer::new();
        let mut program = Program {
            decls: vec![Decl::DataDecl {
                name: "Phantom".into(),
                type_params: vec!["a".into()],
                constructors: vec![ConDecl {
                    name: "Phantom".into(),
                    fields: ConFields::Empty,
                    discriminant: None,
                    span: span(),
                    doc: None,
                }],
                span: span(),
                comments: vec![],
            }],
        };

        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(!sa.has_errors());

        let scheme = sa
            .env
            .lookup("Phantom")
            .expect("Phantom constructor should be in env");
        assert_eq!(scheme.vars.len(), 1);
        assert_eq!(sa.constructors["Phantom"].scheme_vars.len(), 1);
    }

    #[test]
    fn test_if_expr_type_check() {
        let mut sa = SemanticAnalyzer::new();
        // f x = if x == 0 then 1 else 2
        let mut program = Program {
            decls: vec![Decl::FunDecl {
                name: "f".into(),
                params: vec![Pat::Var("x".into(), span())],
                body: Expr::If(
                    Box::new(Expr::Infix(
                        Box::new(Expr::Var("x".into(), span())),
                        "==".into(),
                        Box::new(Expr::Lit(Lit::Int(0), span())),
                        span(),
                    )),
                    Box::new(Expr::Lit(Lit::Int(1), span())),
                    Box::new(Expr::Lit(Lit::Int(2), span())),
                    span(),
                ),
                where_binds: vec![],
                span: span(),
                comments: vec![],
            }],
        };
        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(!sa.has_errors());
    }

    #[test]
    fn test_let_expr() {
        let mut sa = SemanticAnalyzer::new();
        // f = let x = 42 in x + 1
        let mut program = Program {
            decls: vec![Decl::FunDecl {
                name: "f".into(),
                params: vec![],
                body: Expr::Let(
                    vec![("x".into(), Expr::Lit(Lit::Int(42), span()))],
                    Box::new(Expr::Infix(
                        Box::new(Expr::Var("x".into(), span())),
                        "+".into(),
                        Box::new(Expr::Lit(Lit::Int(1), span())),
                        span(),
                    )),
                    span(),
                ),
                where_binds: vec![],
                span: span(),
                comments: vec![],
            }],
        };
        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(!sa.has_errors());
    }

    #[test]
    fn test_where_clause() {
        let mut sa = SemanticAnalyzer::new();
        // f x = y + 1 where y = x
        let mut program = Program {
            decls: vec![Decl::FunDecl {
                name: "f".into(),
                params: vec![Pat::Var("x".into(), span())],
                body: Expr::Infix(
                    Box::new(Expr::Var("y".into(), span())),
                    "+".into(),
                    Box::new(Expr::Lit(Lit::Int(1), span())),
                    span(),
                ),
                where_binds: vec![("y".into(), Expr::Var("x".into(), span()))],
                span: span(),
                comments: vec![],
            }],
        };
        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(!sa.has_errors());

        let scheme = sa.env.lookup("f").expect("f should be in env");
        let ty = sa.engine.finalize(&scheme.ty);
        assert_eq!(format!("{}", ty), "(I32 -> I32)");
    }

    #[test]
    fn test_lambda_inference() {
        let mut sa = SemanticAnalyzer::new();
        // f = \x -> x + 1
        let mut program = Program {
            decls: vec![Decl::FunDecl {
                name: "f".into(),
                params: vec![],
                body: Expr::Lambda(
                    vec![Pat::Var("x".into(), span())],
                    Box::new(Expr::Infix(
                        Box::new(Expr::Var("x".into(), span())),
                        "+".into(),
                        Box::new(Expr::Lit(Lit::Int(1), span())),
                        span(),
                    )),
                    span(),
                ),
                where_binds: vec![],
                span: span(),
                comments: vec![],
            }],
        };
        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(!sa.has_errors());
    }

    #[test]
    fn test_case_expr_with_data_type() {
        let mut sa = SemanticAnalyzer::new();
        // data Bool2 = True2 | False2
        // f x = match x | True2 -> 1 | False2 -> 0
        let mut program = Program {
            decls: vec![
                Decl::DataDecl {
                    name: "Bool2".into(),
                    type_params: vec![],
                    constructors: vec![
                        ConDecl {
                            name: "True2".into(),
                            fields: ConFields::Empty,
                            discriminant: None,
                            span: span(),
                            doc: None,
                        },
                        ConDecl {
                            name: "False2".into(),
                            fields: ConFields::Empty,
                            discriminant: None,
                            span: span(),
                            doc: None,
                        },
                    ],
                    span: span(),
                    comments: vec![],
                },
                Decl::FunDecl {
                    name: "f".into(),
                    params: vec![Pat::Var("x".into(), span())],
                    body: Expr::Case(
                        Box::new(Expr::Var("x".into(), span())),
                        vec![
                            (
                                Pat::Con("True2".into(), vec![], span()),
                                None,
                                Expr::Lit(Lit::Int(1), span()),
                            ),
                            (
                                Pat::Con("False2".into(), vec![], span()),
                                None,
                                Expr::Lit(Lit::Int(0), span()),
                            ),
                        ],
                        span(),
                    ),
                    where_binds: vec![],
                    span: span(),
                    comments: vec![],
                },
            ],
        };
        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(!sa.has_errors());
    }

    #[test]
    fn test_builtin_option_result_and_tensor_utilities_are_registered() {
        let mut sa = SemanticAnalyzer::new();
        let mut program = Program { decls: vec![] };
        with_prelude(&mut program);
        sa.analyze(&program);

        for name in [
            "Some",
            "None",
            "Ok",
            "Err",
            "Pair",
            "sin",
            "cos",
            "normalize",
            "length",
            "vec2",
            "vec4",
            "load",
            "toF32",
            "writeAt",
        ] {
            assert!(sa.env.lookup(name).is_some(), "missing builtin: {}", name);
        }
    }

    #[test]
    fn test_builtin_option_result_and_map_type_check() {
        let mut sa = SemanticAnalyzer::new();
        let mut program = Program {
            decls: vec![
                // lift : Option I32 -> Option I32
                // lift value = match value | Some x -> Some (x + 1) | None -> None
                Decl::TypeSig {
                    name: "lift".into(),
                    constraints: vec![],
                    ty: Type::Arrow(
                        Box::new(Type::App(
                            Box::new(Type::Con("Option".into(), span())),
                            Box::new(Type::Con("I32".into(), span())),
                            span(),
                        )),
                        Box::new(Type::App(
                            Box::new(Type::Con("Option".into(), span())),
                            Box::new(Type::Con("I32".into(), span())),
                            span(),
                        )),
                        span(),
                    ),
                    span: span(),
                    comments: vec![],
                },
                Decl::FunDecl {
                    name: "lift".into(),
                    params: vec![Pat::Var("value".into(), span())],
                    body: Expr::Case(
                        Box::new(Expr::Var("value".into(), span())),
                        vec![
                            (
                                Pat::Con("Some".into(), vec![Pat::Var("x".into(), span())], span()),
                                None,
                                Expr::App(
                                    Box::new(Expr::Var("Some".into(), span())),
                                    Box::new(Expr::Infix(
                                        Box::new(Expr::Var("x".into(), span())),
                                        "+".into(),
                                        Box::new(Expr::Lit(Lit::Int(1), span())),
                                        span(),
                                    )),
                                    span(),
                                ),
                            ),
                            (
                                Pat::Con("None".into(), vec![], span()),
                                None,
                                Expr::Var("None".into(), span()),
                            ),
                        ],
                        span(),
                    ),
                    where_binds: vec![],
                    span: span(),
                    comments: vec![],
                },
                Decl::TypeSig {
                    name: "unwrap".into(),
                    constraints: vec![],
                    ty: Type::Arrow(
                        Box::new(Type::App(
                            Box::new(Type::Con("Result".into(), span())),
                            Box::new(Type::Con("I32".into(), span())),
                            span(),
                        )),
                        Box::new(Type::Con("I32".into(), span())),
                        span(),
                    ),
                    span: span(),
                    comments: vec![],
                },
                Decl::FunDecl {
                    name: "unwrap".into(),
                    params: vec![Pat::Var("value".into(), span())],
                    body: Expr::Case(
                        Box::new(Expr::Var("value".into(), span())),
                        vec![
                            (
                                Pat::Con("Ok".into(), vec![Pat::Var("x".into(), span())], span()),
                                None,
                                Expr::Var("x".into(), span()),
                            ),
                            (
                                Pat::Con("Err".into(), vec![Pat::Wild(span())], span()),
                                None,
                                Expr::Lit(Lit::Int(0), span()),
                            ),
                        ],
                        span(),
                    ),
                    where_binds: vec![],
                    span: span(),
                    comments: vec![],
                },
            ],
        };

        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(!sa.has_errors());
    }
}
