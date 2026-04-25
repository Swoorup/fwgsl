//! Semantic analysis for shadml.
//!
//! Performs name resolution and type inference over the parser's AST.
//! Collects data type definitions, constructor info, type signatures,
//! and infers types for function bodies using Algorithm W (HM inference).

use std::collections::{HashMap, HashSet};
use std::fmt;

use shadml_diagnostics::{Diagnostic, DiagnosticSink, Label};
use shadml_parser::parser::*;
use shadml_span::Span;
use shadml_typechecker::*;

pub mod helpers;
pub use helpers::*;

/// The kind of a type-level name, used for duplicate detection across the
/// unified type namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TypeNameKind {
    DataType,
    Trait,
    Alias,
    BuiltinType,
    Bitfield,
}

impl fmt::Display for TypeNameKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TypeNameKind::DataType => write!(f, "data type"),
            TypeNameKind::Trait => write!(f, "trait"),
            TypeNameKind::Alias => write!(f, "type alias"),
            TypeNameKind::BuiltinType => write!(f, "builtin type"),
            TypeNameKind::Bitfield => write!(f, "bitfield"),
        }
    }
}

/// Information about a trait declaration.
#[derive(Debug, Clone)]
pub struct TraitInfo {
    pub name: String,
    /// The type variables the trait is parameterised over.
    pub vars: Vec<String>,
    pub var_ids: Vec<TyVarId>,
    /// Associated type names declared by the trait (e.g., ["Output"]).
    pub associated_types: Vec<String>,
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
    /// Associated type bindings: "Output" -> F32, etc.
    pub associated_type_bindings: HashMap<String, Ty>,
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
    /// Associated type bindings: "Output" -> F32, etc.
    pub associated_type_bindings: HashMap<String, Ty>,
    pub methods: HashMap<String, BuiltinLowering>,
}

/// The semantic analyzer: collects definitions and performs type inference.
#[derive(Clone)]
pub struct SemanticAnalyzer {
    pub env: TypeEnv,
    pub engine: InferEngine,
    pub constructors: HashMap<String, ConstructorInfo>,
    pub data_types: HashMap<String, DataTypeInfo>,
    pub expr_types: HashMap<Span, Ty>,
    pub local_binding_schemes: HashMap<Span, Scheme>,
    /// User-defined type aliases (e.g. `alias Float2 = Vec<2, F32>`).
    /// Maps alias name → expanded Ty so they can be resolved during type conversion.
    pub type_aliases: HashMap<String, Ty>,
    /// Source-declared builtin type constructors: name -> arity.
    pub builtin_types: HashMap<String, usize>,
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
    /// Registry of all type-level names (data types, traits, aliases, builtin types, bitfields).
    /// Used to detect duplicates across the unified type namespace.
    type_names: HashMap<String, TypeNameKind>,
    /// Mangled method names from all impls, pre-built for O(1) lookup.
    impl_method_names: HashSet<String>,
    /// Set of top-level binding names that are const-eligible
    /// (explicit `@const`, `const` declarations, or auto-promoted zero-param functions).
    pub const_bindings: HashSet<String>,
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
            expr_types: HashMap::new(),
            local_binding_schemes: HashMap::new(),
            type_aliases: HashMap::new(),
            builtin_types: HashMap::new(),
            traits: HashMap::new(),
            impls: Vec::new(),
            builtin_externs: HashMap::new(),
            builtin_impls: Vec::new(),
            inferred_predicates: Vec::new(),
            bitfield_field_names: HashMap::new(),
            type_names: HashMap::new(),
            impl_method_names: HashSet::new(),
            const_bindings: HashSet::new(),
        }
    }

    pub fn analyze(&mut self, program: &Program) {
        // Flatten CfgDecl nodes so we see declarations from both branches.
        // This ensures the semantic analyzer registers names from all conditional
        // compilation paths (the compiler narrows later via evaluate_features).
        let all_decls = Decl::flatten_cfg_decls(&program.decls);

        // Pass 1: predeclare type names so aliases and data declarations in the
        // same module can refer to each other regardless of source order.
        // Also check for duplicate type-level names across the unified namespace.
        // Render-block entries are included transparently.
        for decl in all_entries_including_render_blocks(&all_decls) {
            match decl {
                Decl::BuiltinTypeDecl {
                    name, arity, span, ..
                } => {
                    if let Some(existing) = self.type_names.get(name) {
                        self.engine.diagnostics.push(
                            Diagnostic::error(format!(
                                "Duplicate type name '{}': already declared as {}",
                                name, existing
                            ))
                            .with_label(Label::primary(*span, "duplicate type name")),
                        );
                    } else {
                        self.type_names
                            .insert(name.clone(), TypeNameKind::BuiltinType);
                        self.builtin_types.insert(name.clone(), *arity);
                    }
                }
                Decl::DataDecl {
                    name,
                    type_params,
                    span,
                    ..
                } => {
                    if let Some(existing) = self.type_names.get(name) {
                        self.engine.diagnostics.push(
                            Diagnostic::error(format!(
                                "Duplicate type name '{}': already declared as {}",
                                name, existing
                            ))
                            .with_label(Label::primary(*span, "duplicate type name"))
                            .with_help("type names must be unique across data types, traits, aliases, builtin types, and bitfields"),
                        );
                    } else {
                        self.type_names.insert(name.clone(), TypeNameKind::DataType);
                        self.data_types.insert(
                            name.clone(),
                            DataTypeInfo {
                                name: name.clone(),
                                type_params: type_params.clone(),
                                constructors: vec![],
                            },
                        );
                    }
                }
                Decl::BitfieldDecl { name, span, .. } => {
                    if let Some(existing) = self.type_names.get(name) {
                        self.engine.diagnostics.push(
                            Diagnostic::error(format!(
                                "Duplicate type name '{}': already declared as {}",
                                name, existing
                            ))
                            .with_label(Label::primary(*span, "duplicate type name")),
                        );
                    } else {
                        self.type_names.insert(name.clone(), TypeNameKind::Bitfield);
                        self.bitfield_field_names.entry(name.clone()).or_default();
                    }
                }
                _ => {}
            }
        }

        // Pass 1b: collect type aliases (treated as synonyms for semantic purposes)
        for decl in &all_decls {
            if let Decl::TypeAlias { name, ty, span, .. } = decl {
                if let Some(existing) = self.type_names.get(name) {
                    self.engine.diagnostics.push(
                        Diagnostic::error(format!(
                            "Duplicate type name '{}': already declared as {}",
                            name, existing
                        ))
                        .with_label(Label::primary(*span, "duplicate type name")),
                    );
                } else {
                    let alias_ty = self.convert_syntax_type(ty);
                    self.type_names.insert(name.clone(), TypeNameKind::Alias);
                    // Store the expanded type for alias resolution during type conversion
                    self.type_aliases.insert(name.clone(), alias_ty.ty.clone());
                    // Register the alias name as a type constructor
                    self.env.insert(name.clone(), alias_ty);
                }
            }
        }

        // Pass 1c: collect data types and constructors
        // Render-block entries are included transparently.
        for decl in all_entries_including_render_blocks(&all_decls) {
            if let Decl::DataDecl {
                name,
                type_params,
                constructors,
                span,
                ..
            } = decl
            {
                // Only register constructors if this name wasn't flagged as a duplicate
                if self.type_names.get(name) == Some(&TypeNameKind::DataType) {
                    self.register_data_type(name, type_params, constructors, *span);
                }
            }
        }

        // Pass 1d: collect bitfield metadata
        // Note: bitfield names were already registered in type_names during Pass 1.
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
                // Only update bitfield_field_names if this name wasn't flagged as a duplicate
                if self.type_names.get(name) == Some(&TypeNameKind::Bitfield) {
                    self.bitfield_field_names.insert(name.clone(), field_names);
                }
            }
        }

        // Pass 1e: pre-collect trait names and associated type names
        // (needed by Pass 2 to convert Type::Proj in type signatures)
        for decl in &all_decls {
            if let Decl::TraitDecl {
                name,
                vars,
                associated_types,
                span,
                ..
            } = decl
            {
                if let Some(existing) = self.type_names.get(name) {
                    self.engine.diagnostics.push(
                        Diagnostic::error(format!(
                            "Duplicate type name '{}': already declared as {}",
                            name, existing
                        ))
                        .with_label(Label::primary(*span, "duplicate type name")),
                    );
                } else {
                    self.type_names.insert(name.clone(), TypeNameKind::Trait);
                    let var_ids: Vec<TyVarId> = vars
                        .iter()
                        .map(|_| fresh_var_id(&mut self.engine))
                        .collect();
                    let mut seen_assoc: HashSet<String> = HashSet::new();
                    let mut assoc_type_names: Vec<String> = Vec::new();
                    for at in associated_types {
                        if seen_assoc.contains(&at.name) {
                            self.engine.diagnostics.push(
                                Diagnostic::error(format!(
                                    "Duplicate associated type '{}' in trait '{}'",
                                    at.name, name
                                ))
                                .with_label(Label::primary(
                                    at.span,
                                    "duplicate associated type declaration",
                                )),
                            );
                        } else {
                            seen_assoc.insert(at.name.clone());
                            assoc_type_names.push(at.name.clone());
                        }
                    }
                    self.traits.insert(
                        name.clone(),
                        TraitInfo {
                            name: name.clone(),
                            vars: vars.clone(),
                            var_ids,
                            associated_types: assoc_type_names,
                            methods: vec![],
                        },
                    );
                }
            }
        }

        // Pass 2: collect type signatures
        // Render-block bindings and entries are included transparently.
        for decl in all_decls_and_render_block_contents(&all_decls) {
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
                name, ty, lowering, ..
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
                name,
                vars,
                associated_types,
                methods,
                ..
            } = decl
            {
                // Skip if this trait name was flagged as a duplicate in Pass 1e
                if self.type_names.get(name) != Some(&TypeNameKind::Trait) {
                    continue;
                }
                let var_ids: Vec<TyVarId> = vars
                    .iter()
                    .map(|_| fresh_var_id(&mut self.engine))
                    .collect();
                // Always create constraint context when in a trait body so that
                // `Self` can resolve to the first type parameter and associated
                // type projections like `Self.Output` can be handled.
                let constraint_contexts: Vec<(String, Vec<Ty>)> =
                    vec![(name.clone(), var_ids.iter().copied().map(Ty::Var).collect())];
                let mut trait_methods = Vec::new();
                let mut seen_trait_methods: HashSet<String> = HashSet::new();
                for m in methods {
                    let canonical_name = canonical_trait_method_name(name, &m.name);
                    if seen_trait_methods.contains(&canonical_name) {
                        self.engine.diagnostics.push(
                            Diagnostic::error(format!(
                                "Duplicate method '{}' in trait '{}'",
                                m.name, name
                            ))
                            .with_label(Label::primary(m.span, "duplicate method declaration")),
                        );
                        continue;
                    }
                    seen_trait_methods.insert(canonical_name.clone());
                    let mut scope: HashMap<String, TyVarId> =
                        vars.iter().cloned().zip(var_ids.iter().copied()).collect();
                    let method_ty = self.convert_syntax_type_with_scope_assoc(
                        &m.ty,
                        &mut scope,
                        &constraint_contexts,
                    );
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
                        associated_types: associated_types
                            .iter()
                            .map(|at| at.name.clone())
                            .collect(),
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
                associated_types,
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
                // Collect associated type bindings from AST
                let mut seen_assoc: HashSet<String> = HashSet::new();
                let mut assoc_type_bindings: HashMap<String, Ty> = HashMap::new();
                for at in associated_types {
                    if seen_assoc.contains(&at.name) {
                        self.engine.diagnostics.push(
                            Diagnostic::error(format!(
                                "Duplicate associated type definition '{}' in impl",
                                at.name
                            ))
                            .with_label(Label::primary(
                                at.span,
                                "duplicate associated type definition",
                            )),
                        );
                    } else {
                        seen_assoc.insert(at.name.clone());
                        assoc_type_bindings.insert(
                            at.name.clone(),
                            normalize_type_aliases(&self.convert_syntax_type(&at.ty).ty),
                        );
                    }
                }
                let mut impl_methods = HashMap::new();
                let mut seen_methods: HashSet<String> = HashSet::new();
                for m in methods {
                    let logical_name = trait_name
                        .as_deref()
                        .map(|tname| canonical_trait_method_name(tname, &m.name))
                        .unwrap_or_else(|| m.name.clone());
                    if seen_methods.contains(&logical_name) {
                        self.engine.diagnostics.push(
                            Diagnostic::error(format!("Duplicate method '{}' in impl", m.name))
                                .with_label(Label::primary(m.span, "duplicate method definition")),
                        );
                        continue;
                    }
                    seen_methods.insert(logical_name.clone());
                    let mangled = mangle_instance_method(&logical_name, &type_suffix);
                    impl_methods.insert(logical_name.clone(), mangled.clone());

                    if let Some(tname) = trait_name {
                        // Trait impl: look up trait method signature and substitute
                        if let Some(trait_info) = self.traits.get(tname).cloned() {
                            for (tmethod_name, tmethod_ty) in trait_info.methods {
                                if tmethod_name == logical_name {
                                    let concrete_ty = replace_trait_vars(
                                        &tmethod_ty,
                                        &trait_info.var_ids,
                                        &impl_tys,
                                    );
                                    let concrete_ty = resolve_assoc_projections(
                                        &concrete_ty,
                                        &assoc_type_bindings,
                                    );
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
                            let expected_scheme =
                                self.standalone_impl_method_scheme(&impl_tys[0], m.params.len());
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
                            .filter(|&(method_name, _)| !impl_methods.contains_key(method_name))
                            .map(|(method_name, _)| method_name.clone())
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

                        // Validate that method definitions belong to the trait
                        let unknown_methods: Vec<&String> = impl_methods
                            .keys()
                            .filter(|name| !trait_info.methods.iter().any(|(m, _)| m == *name))
                            .collect();
                        if !unknown_methods.is_empty() {
                            self.engine.diagnostics.push(
                                Diagnostic::error(format!(
                                    "'{}' is not a method of trait '{}'",
                                    unknown_methods
                                        .iter()
                                        .map(|s| s.as_str())
                                        .collect::<Vec<_>>()
                                        .join("', '"),
                                    tname
                                ))
                                .with_label(Label::primary(*span, "unknown method definition"))
                                .with_help("remove it from the impl, or declare it in the trait"),
                            );
                        }

                        // Validate that all trait associated types have bindings.
                        let missing_assoc_types: Vec<&str> = trait_info
                            .associated_types
                            .iter()
                            .filter(|name| !assoc_type_bindings.contains_key(name.as_str()))
                            .map(String::as_str)
                            .collect();
                        if !missing_assoc_types.is_empty() {
                            self.engine.diagnostics.push(
                                Diagnostic::error(format!(
                                    "Incomplete implementation of trait '{}' for type '{}': missing associated type(s): {}",
                                    tname,
                                    format_impl_head(&impl_tys),
                                    missing_assoc_types.join(", ")
                                ))
                                .with_label(Label::primary(*span, "missing associated type definition"))
                                .with_help(format!(
                                    "add `type {} = ...` to the impl block",
                                    missing_assoc_types.join(", type ")
                                )),
                            );
                        }
                        // Validate that associated type definitions belong to the trait
                        let unknown_assoc_types: Vec<&str> = assoc_type_bindings
                            .keys()
                            .filter(|name| !trait_info.associated_types.iter().any(|n| n == *name))
                            .map(String::as_str)
                            .collect();
                        if !unknown_assoc_types.is_empty() {
                            self.engine.diagnostics.push(
                                Diagnostic::error(format!(
                                    "'{}' is not an associated type of trait '{}'",
                                    unknown_assoc_types.join("', '"), tname
                                ))
                                .with_label(Label::primary(*span, "unknown associated type definition"))
                                .with_help(format!(
                                    "remove `type {} = ...` from the impl, or declare it in the trait",
                                    unknown_assoc_types.join(", type ")
                                )),
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
                let new_impl = ImplInfo {
                    trait_name: trait_name.clone(),
                    tys: impl_tys,
                    associated_type_bindings: assoc_type_bindings,
                    methods: impl_methods,
                };
                for mangled in new_impl.methods.values() {
                    self.impl_method_names.insert(mangled.clone());
                }
                self.impls.push(new_impl);
            }
            if let Decl::BuiltinImplDecl {
                trait_name,
                tys,
                associated_types,
                methods,
                span,
                ..
            } = decl
            {
                let impl_tys: Vec<Ty> = tys
                    .iter()
                    .map(|ty| normalize_type_aliases(&self.convert_syntax_type(ty).ty))
                    .collect();
                // Collect associated type bindings from AST
                let mut seen_assoc: HashSet<String> = HashSet::new();
                let mut assoc_type_bindings: HashMap<String, Ty> = HashMap::new();
                for at in associated_types {
                    if seen_assoc.contains(&at.name) {
                        self.engine.diagnostics.push(
                            Diagnostic::error(format!(
                                "Duplicate associated type definition '{}' in impl",
                                at.name
                            ))
                            .with_label(Label::primary(
                                at.span,
                                "duplicate associated type definition",
                            )),
                        );
                    } else {
                        seen_assoc.insert(at.name.clone());
                        assoc_type_bindings.insert(
                            at.name.clone(),
                            normalize_type_aliases(&self.convert_syntax_type(&at.ty).ty),
                        );
                    }
                }
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
                    // Validate that method definitions belong to the trait
                    for method in methods {
                        let logical_name = canonical_trait_method_name(trait_name, &method.name);
                        if !trait_info.methods.iter().any(|(m, _)| m == &logical_name) {
                            self.engine.diagnostics.push(
                                Diagnostic::error(format!(
                                    "'{}' is not a method of trait '{}'",
                                    method.name, trait_name
                                ))
                                .with_label(Label::primary(
                                    method.span,
                                    "unknown method definition",
                                ))
                                .with_help(
                                    "remove it from the builtin impl, or declare it in the trait",
                                ),
                            );
                        }
                    }
                    // Validate that all trait associated types have bindings.
                    let missing_assoc_types: Vec<&str> = trait_info
                        .associated_types
                        .iter()
                        .filter(|name| !assoc_type_bindings.contains_key(name.as_str()))
                        .map(String::as_str)
                        .collect();
                    if !missing_assoc_types.is_empty() {
                        self.engine.diagnostics.push(
                            Diagnostic::error(format!(
                                "Incomplete builtin implementation of trait '{}' for type '{}': missing associated type(s): {}",
                                trait_name,
                                format_impl_head(&impl_tys),
                                missing_assoc_types.join(", ")
                            ))
                            .with_label(Label::primary(*span, "missing associated type definition"))
                            .with_help(format!(
                                "add `type {} = ...` to the builtin impl block",
                                missing_assoc_types.join(", type ")
                            )),
                        );
                    }
                    // Validate that associated type definitions belong to the trait
                    let unknown_assoc_types: Vec<&str> = assoc_type_bindings
                        .keys()
                        .filter(|name| !trait_info.associated_types.iter().any(|n| n == *name))
                        .map(String::as_str)
                        .collect();
                    if !unknown_assoc_types.is_empty() {
                        self.engine.diagnostics.push(
                            Diagnostic::error(format!(
                                "'{}' is not an associated type of trait '{}'",
                                unknown_assoc_types.join("', '"), trait_name
                            ))
                            .with_label(Label::primary(*span, "unknown associated type definition in builtin impl"))
                            .with_help(format!(
                                "remove `type {} = ...` from the builtin impl, or declare it in the trait",
                                unknown_assoc_types.join(", type ")
                            )),
                        );
                    }
                }
                if self
                    .builtin_impls
                    .iter()
                    .any(|existing| existing.trait_name == *trait_name && existing.tys == impl_tys)
                {
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
                    associated_type_bindings: assoc_type_bindings,
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
                    attributes,
                    ..
                } => {
                    self.check_entry_point(
                        name, params, body, *span, attributes, /* is_render_block */ false,
                    );
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
                Decl::RenderBlock { entries, span, .. } => {
                    // Type-check entry points inside the render block
                    for rb_decl in entries {
                        if let Decl::EntryPoint {
                            name,
                            params,
                            body,
                            span: epan,
                            attributes,
                            ..
                        } = rb_decl
                        {
                            self.check_entry_point(
                                name, params, body, *epan, attributes,
                                /* is_render_block */ true,
                            );
                        }
                    }
                    let _ = span;
                }
                _ => {}
            }
        }

        // Pass 4: validate @const attributes
        self.const_bindings = validate_const::compute_const_bindings(program);
        validate_const::validate_const_attributes(
            program,
            &self.const_bindings,
            &mut self.engine.diagnostics,
        );
    }

    pub fn has_errors(&self) -> bool {
        self.engine.diagnostics.has_errors()
    }

    pub fn diagnostics(&self) -> &DiagnosticSink {
        &self.engine.diagnostics
    }
}

impl Default for SemanticAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

mod decl;
mod expr;
mod pattern;
mod types;
pub mod validate_const;

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
                attributes: vec![],
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
                attributes: vec![],
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
                    attributes: vec![],
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
                    attributes: vec![],
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
                associated_types: vec![],
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
                    attributes: vec![],
                },
            ],
        };
        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(sa.has_errors());
        assert!(sa.diagnostics().iter().any(|diag| diag
            .message
            .contains("function `test` has 2 parameters but its type signature expects 1")));
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
                    attributes: vec![],
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
                associated_types: vec![],
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
                    associated_types: vec![],
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
                    attributes: vec![],
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
                    associated_types: vec![],
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
                    attributes: vec![],
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
                    attributes: vec![],
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
                    attributes: vec![],
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
                attributes: vec![],
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
                    vec![LocalBind {
                        name: "x".into(),
                        name_span: span(),
                        expr: Expr::Lit(Lit::Int(42), span()),
                        span: span(),
                    }],
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
                attributes: vec![],
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
                where_binds: vec![LocalBind {
                    name: "y".into(),
                    name_span: span(),
                    expr: Expr::Var("x".into(), span()),
                    span: span(),
                }],
                span: span(),
                comments: vec![],
                attributes: vec![],
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
    fn test_local_binding_schemes_record_finalized_types() {
        let source = include_str!("../../../examples/slang-generics.shadml");
        let mut parser = Parser::new(source);
        let mut program = parser.parse_program();
        with_prelude(&mut program);

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);
        assert!(!sa.has_errors());

        let find_scheme = |name: &str| {
            sa.local_binding_schemes
                .iter()
                .find(|(span, _)| span.source_text(source) == name)
                .map(|(_, scheme)| format_scheme_surface(scheme, Some(&sa.engine.subst)))
                .unwrap_or_else(|| panic!("missing local binding scheme for `{name}`"))
        };

        assert_eq!(find_scheme("dist"), "F32");
        assert_eq!(find_scheme("spotFactor"), "F32");
        assert_eq!(find_scheme("atten"), "F32");
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
                attributes: vec![],
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
                    attributes: vec![],
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
                    attributes: vec![],
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
                    attributes: vec![],
                },
            ],
        };

        with_prelude(&mut program);
        sa.analyze(&program);
        assert!(!sa.has_errors());
    }

    #[test]
    fn test_module_scope_vertex_entry_point_rejected() {
        let source = r#"
@vertex
vsMain : Vec<4, F32> -> Vec<4, F32>
vsMain pos = pos
"#;
        let mut parser = shadml_parser::parser::Parser::new(source);
        let mut program = parser.parse_program();
        with_prelude(&mut program);

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);
        assert!(
            sa.has_errors(),
            "module-scope @vertex entry point should be an error"
        );
        let error_messages: Vec<String> = sa
            .diagnostics()
            .iter()
            .filter(|d| d.severity == shadml_diagnostics::Severity::Error)
            .map(|d| d.message.clone())
            .collect();
        assert!(
            error_messages
                .iter()
                .any(|m| m.contains("must be inside a render block")),
            "expected error about render block, got: {:?}",
            error_messages
        );
    }

    #[test]
    fn test_module_scope_fragment_entry_point_rejected() {
        let source = r#"
@fragment
fsMain : Vec<4, F32> -> Vec<4, F32>
fsMain color = color
"#;
        let mut parser = shadml_parser::parser::Parser::new(source);
        let mut program = parser.parse_program();
        with_prelude(&mut program);

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);
        assert!(
            sa.has_errors(),
            "module-scope @fragment entry point should be an error"
        );
    }

    #[test]
    fn test_module_scope_compute_entry_point_accepted() {
        // Module-scope @compute entry points should NOT trigger the
        // "must be inside a render block" error. Other type errors may
        // exist, but the specific render-block error should be absent.
        let source = r#"
@compute
vsMain : Vec<4, F32> -> Vec<4, F32>
vsMain pos = pos
"#;
        let mut parser = shadml_parser::parser::Parser::new(source);
        let mut program = parser.parse_program();
        with_prelude(&mut program);

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);
        // We don't assert !sa.has_errors() because the program may have
        // other type errors. We specifically check that there is NO error
        // about render blocks.
        let has_render_block_error = sa.diagnostics().iter().any(|d| {
            d.severity == shadml_diagnostics::Severity::Error
                && d.message.contains("must be inside a render block")
        });
        assert!(
            !has_render_block_error,
            "@compute entry points should NOT require a render block"
        );
    }

    #[test]
    fn test_const_attribute_on_literal_passes() {
        let mut sa = SemanticAnalyzer::new();
        let mut program = Program {
            decls: vec![Decl::FunDecl {
                name: "maxLights".into(),
                params: vec![],
                body: Expr::Lit(Lit::Int(64), span()),
                where_binds: vec![],
                span: span(),
                comments: vec![],
                attributes: vec![Attribute {
                    name: "const".into(),
                    args: vec![],
                    span: span(),
                }],
            }],
        };
        with_prelude(&mut program);
        sa.analyze(&program);
        let has_const_error = sa.diagnostics().iter().any(|d| {
            d.severity == shadml_diagnostics::Severity::Error && d.message.contains("@const")
        });
        assert!(
            !has_const_error,
            "@const on literal should not produce an error, got: {:?}",
            sa.diagnostics().iter().collect::<Vec<_>>()
        );
        assert!(sa.const_bindings.contains("maxLights"));
    }

    #[test]
    fn test_const_attribute_on_non_const_binding_fails() {
        let mut sa = SemanticAnalyzer::new();
        let mut program = Program {
            decls: vec![
                Decl::FunDecl {
                    name: "getBlockSize".into(),
                    params: vec![Pat::Var("x".into(), span())],
                    body: Expr::Var("x".into(), span()),
                    where_binds: vec![],
                    span: span(),
                    comments: vec![],
                    attributes: vec![],
                },
                Decl::FunDecl {
                    name: "tableSize".into(),
                    params: vec![],
                    body: Expr::App(
                        Box::new(Expr::Var("getBlockSize".into(), span())),
                        Box::new(Expr::Lit(Lit::Int(16), span())),
                        span(),
                    ),
                    where_binds: vec![],
                    span: span(),
                    comments: vec![],
                    attributes: vec![Attribute {
                        name: "const".into(),
                        args: vec![],
                        span: span(),
                    }],
                },
            ],
        };
        with_prelude(&mut program);
        sa.analyze(&program);
        let has_const_error = sa.diagnostics().iter().any(|d| {
            d.severity == shadml_diagnostics::Severity::Error && d.message.contains("@const")
        });
        assert!(
            has_const_error,
            "@const referencing non-const binding should produce an error, got: {:?}",
            sa.diagnostics().iter().collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_const_attribute_on_function_with_params_fails() {
        let mut sa = SemanticAnalyzer::new();
        let mut program = Program {
            decls: vec![Decl::FunDecl {
                name: "double".into(),
                params: vec![Pat::Var("x".into(), span())],
                body: Expr::Infix(
                    Box::new(Expr::Var("x".into(), span())),
                    "*".into(),
                    Box::new(Expr::Lit(Lit::Int(2), span())),
                    span(),
                ),
                where_binds: vec![],
                span: span(),
                comments: vec![],
                attributes: vec![Attribute {
                    name: "const".into(),
                    args: vec![],
                    span: span(),
                }],
            }],
        };
        with_prelude(&mut program);
        sa.analyze(&program);
        let has_const_error = sa.diagnostics().iter().any(|d| {
            d.severity == shadml_diagnostics::Severity::Error
                && d.message.contains("@const")
                && d.message.contains("parameters")
        });
        assert!(
            has_const_error,
            "@const on function with parameters should produce an error"
        );
    }
}
