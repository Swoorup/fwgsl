//! AST → HIR lowering for shadml.
//!
//! Converts the parser's AST into a type-annotated HIR by re-running
//! type inference (mirroring the semantic analyzer) while simultaneously
//! building HIR nodes.

use std::collections::HashMap;

use shadml_hir::*;
use shadml_parser::parser::*;
use shadml_semantic::helpers::*;
use shadml_typechecker::*;

/// Lowers a parsed AST `Program` into an `HirProgram`.
///
/// Requires a `SemanticAnalyzer` that has already run `analyze()` so that
/// constructor and data-type information is available.
/// Bitfield field info used during AST lowering for construction/update.
#[derive(Debug, Clone)]
pub struct BitfieldFieldMeta {
    pub offset: u32,
    pub width: u32,
}

pub struct AstLowering {
    pub env: TypeEnv,
    pub engine: InferEngine,
    pub constructors: HashMap<String, ConstructorInfo>,
    pub data_types: HashMap<String, shadml_semantic::DataTypeInfo>,
    pub type_aliases: HashMap<String, Ty>,
    pub traits: HashMap<String, shadml_semantic::TraitInfo>,
    pub impls: Vec<shadml_semantic::ImplInfo>,
    pub builtin_externs: HashMap<String, Vec<shadml_semantic::BuiltinExternInfo>>,
    pub builtin_impls: Vec<shadml_semantic::BuiltinImplInfo>,
    pub inferred_predicates: Vec<Predicate>,
    pub active_constraints_stack: Vec<Vec<Predicate>>,
    /// Map from bitfield type name → ordered list of (field_name, meta).
    /// Populated during `lower_program` before expressions are lowered.
    pub bitfield_fields: HashMap<String, Vec<(String, BitfieldFieldMeta)>>,
}

#[derive(Debug, Clone)]
struct PendingSpecialization {
    original_name: String,
    concrete_name: String,
    subst: HashMap<TyVarId, Ty>,
}

#[derive(Debug, Clone)]
struct AbiInfo {
    param_tys: Vec<Ty>,
    flat_head_ty: Ty,
}

#[derive(Debug, Clone)]
enum TupleValue {
    Scalar(HirExpr, Ty),
    Tuple(Vec<TupleValue>, Ty),
}

impl TupleValue {
    fn into_scalar(self) -> Option<HirExpr> {
        match self {
            TupleValue::Scalar(expr, _) => Some(expr),
            TupleValue::Tuple(_, _) => None,
        }
    }
}

/// Convert a parser-level address space to the HIR enum.
/// Eliminates the string indirection that used to exist in HIR bindings.
fn lower_address_space(
    aspace: shadml_parser::parser::BindingAddressSpace,
) -> shadml_hir::BindingAddressSpace {
    match aspace {
        shadml_parser::parser::BindingAddressSpace::Uniform => {
            shadml_hir::BindingAddressSpace::Uniform
        }
        shadml_parser::parser::BindingAddressSpace::StorageRead => {
            shadml_hir::BindingAddressSpace::StorageRead
        }
        shadml_parser::parser::BindingAddressSpace::StorageReadWrite => {
            shadml_hir::BindingAddressSpace::StorageReadWrite
        }
        shadml_parser::parser::BindingAddressSpace::Immediate => {
            shadml_hir::BindingAddressSpace::Immediate
        }
        shadml_parser::parser::BindingAddressSpace::Opaque => {
            shadml_hir::BindingAddressSpace::Opaque
        }
    }
}

impl AstLowering {
    /// Create a new lowering context from a completed semantic analyzer.
    pub fn new(sa: &shadml_semantic::SemanticAnalyzer) -> Self {
        let mut engine = InferEngine::new();
        if let Some(max_var_id) = sa.env.max_var_id() {
            engine.reserve_above(max_var_id + 1);
        }
        Self {
            env: sa.env.clone(),
            engine,
            constructors: sa.constructors.clone(),
            data_types: sa.data_types.clone(),
            type_aliases: sa.type_aliases.clone(),
            traits: sa.traits.clone(),
            impls: sa.impls.clone(),
            builtin_externs: sa.builtin_externs.clone(),
            builtin_impls: sa.builtin_impls.clone(),
            inferred_predicates: Vec::new(),
            active_constraints_stack: Vec::new(),
            bitfield_fields: HashMap::new(),
        }
    }

    /// Finalize a type by applying the substitution and resolving any
    /// associated type projections (e.g., `F32.Output` → `F32`).
    fn finalize_resolve(&self, ty: &Ty) -> Ty {
        let finalized = self.engine.finalize(ty);
        shadml_semantic::resolve_assoc_projections_with_impls(
            &finalized,
            &self.impls,
            &self.builtin_impls,
        )
    }

    /// Lower the entire program.
    pub fn lower_program(&mut self, program: &Program) -> HirProgram {
        // Flatten CfgDecl nodes so we see declarations from both branches.
        let all_decls = Decl::flatten_cfg_decls(&program.decls);

        // Pass 1: register data types (re-populate env with constructor types)
        for decl in &all_decls {
            if let Decl::DataDecl {
                name,
                type_params,
                constructors: cons,
                span: _,
                ..
            } = decl
            {
                let mut type_scope = self.new_type_var_scope(type_params);
                let scheme_vars = scope_vars(&type_scope);
                let result_ty = apply_type_params(name, type_params, &type_scope);
                for con in cons {
                    let con_ty = match &con.fields {
                        ConFields::Empty => result_ty.clone(),
                        ConFields::Positional(fields) => {
                            let mut ty = result_ty.clone();
                            for field in fields.iter().rev() {
                                let ft =
                                    self.convert_syntax_type_with_scope(field, &mut type_scope);
                                ty = Ty::arrow(ft, ty);
                            }
                            ty
                        }
                        ConFields::Record(fields) => {
                            let mut ty = result_ty.clone();
                            for f in fields.iter().rev() {
                                let ft =
                                    self.convert_syntax_type_with_scope(&f.ty, &mut type_scope);
                                ty = Ty::arrow(ft, ty);
                            }
                            ty
                        }
                    };
                    self.env
                        .insert(con.name.clone(), Scheme::poly(scheme_vars.clone(), con_ty));
                }
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
                let inferred_ty = self.convert_syntax_type_sig_scheme(constraints, ty);
                self.env.insert(name.clone(), inferred_ty);
            }
            if let Decl::ConstDecl { name, ty, .. } = decl {
                let inferred_ty = self.convert_syntax_type_scheme(ty);
                self.env.insert(name.clone(), inferred_ty);
            }
            if let Decl::ExternDecl { name, ty, .. } | Decl::BuiltinExternDecl { name, ty, .. } =
                decl
            {
                let inferred_ty = self.convert_syntax_type_scheme(ty);
                self.env.insert(name.clone(), inferred_ty);
            }
        }

        // Collect comments from TypeSig decls so they can be attached to the
        // corresponding FunDecl (type signatures don't produce output themselves).
        let mut sig_comments: HashMap<String, Vec<String>> = HashMap::new();
        for decl in &all_decls {
            if let Decl::TypeSig { name, comments, .. } = decl {
                if !comments.is_empty() {
                    sig_comments.insert(name.clone(), comments.clone());
                }
            }
        }

        let mut functions = Vec::new();
        let mut data_types = Vec::new();
        let mut entry_points = Vec::new();
        let mut bindings = Vec::new();
        let mut bitfields = Vec::new();
        let mut constants = Vec::new();
        let mut render_blocks = Vec::new();

        for decl in &all_decls {
            match decl {
                Decl::FunDecl {
                    name,
                    params,
                    body,
                    where_binds,
                    span,
                    comments,
                } => {
                    let merged_comments = if comments.is_empty() {
                        sig_comments.get(name).cloned().unwrap_or_default()
                    } else {
                        let mut c = sig_comments.get(name).cloned().unwrap_or_default();
                        c.extend(comments.iter().cloned());
                        c
                    };
                    if let Some(f) =
                        self.lower_fun_decl(name, params, body, where_binds, *span, merged_comments)
                    {
                        functions.push(f);
                    }
                }
                Decl::EntryPoint {
                    attributes,
                    name,
                    params,
                    body,
                    span,
                    comments,
                } => {
                    if let Some(ep) = self.lower_entry_point(
                        attributes,
                        name,
                        params,
                        body,
                        *span,
                        comments.clone(),
                    ) {
                        entry_points.push(ep);
                    }
                }
                Decl::DataDecl {
                    name,
                    type_params,
                    constructors,
                    ..
                } => {
                    data_types.push(self.lower_data_decl(name, type_params, constructors));
                }
                Decl::BindingDecl {
                    name,
                    ty,
                    address_space,
                    group,
                    binding,
                    ..
                } => {
                    let scheme = self.convert_syntax_type_scheme(ty);
                    bindings.push(shadml_hir::HirBinding {
                        name: name.clone(),
                        ty: scheme.ty,
                        address_space: lower_address_space(*address_space),
                        group: *group,
                        binding: *binding,
                    });
                }
                Decl::BitfieldDecl {
                    name,
                    base_ty,
                    fields,
                    span,
                    ..
                } => {
                    let base_scheme = self.convert_syntax_type_scheme(base_ty);
                    let mut offset = 0u32;
                    let mut bf_meta = Vec::new();
                    let hir_fields: Vec<shadml_hir::HirBitfieldField> = fields
                        .iter()
                        .map(|f| {
                            use shadml_parser::parser::BitfieldFieldKind;
                            let (width, field_type) = match &f.kind {
                                BitfieldFieldKind::Bare(w) => (*w, None),
                                BitfieldFieldKind::Bool => (1, Some("Bool".to_string())),
                                BitfieldFieldKind::Typed { ty, width } => {
                                    // Validate width against type if it's an enum
                                    if let Some(dt_info) = self.data_types.get(ty.as_str()) {
                                        let count = dt_info.constructors.len() as u32;
                                        let min_bits = if count <= 1 {
                                            1
                                        } else {
                                            (count as f64).log2().ceil() as u32
                                        };
                                        if *width < min_bits {
                                            self.engine.diagnostics.push(
                                                shadml_diagnostics::Diagnostic::error(format!(
                                                    "bitfield field '{}' needs at least {} bits for type '{}' ({} variants), but only {} specified",
                                                    f.name, min_bits, ty, count, width
                                                ))
                                                .with_label(shadml_diagnostics::Label::primary(f.span, "insufficient bit width"))
                                                .with_help(format!("use at least {} bits", min_bits)),
                                            );
                                        }
                                    }
                                    (*width, Some(ty.clone()))
                                }
                                BitfieldFieldKind::EnumInferred(type_name) => {
                                    // Look up the enum type to determine bit width
                                    if let Some(dt_info) = self.data_types.get(type_name.as_str()) {
                                        let count = dt_info.constructors.len() as u32;
                                        let bits = if count <= 1 {
                                            1
                                        } else {
                                            (count as f64).log2().ceil() as u32
                                        };
                                        (bits, Some(type_name.clone()))
                                    } else {
                                        // Unknown type — default to 1 bit
                                        self.engine.diagnostics.push(
                                            shadml_diagnostics::Diagnostic::error(format!(
                                                "unknown type '{}' in bitfield field '{}'",
                                                type_name, f.name
                                            ))
                                            .with_label(shadml_diagnostics::Label::primary(f.span, "unknown type")),
                                        );
                                        (1, None)
                                    }
                                }
                            };
                            bf_meta.push((f.name.clone(), BitfieldFieldMeta { offset, width }));
                            let hf = shadml_hir::HirBitfieldField {
                                name: f.name.clone(),
                                offset,
                                width,
                                field_type,
                            };
                            offset += width;
                            hf
                        })
                        .collect();
                    // Validate total bit width doesn't exceed base type
                    let max_bits: u32 = match &base_scheme.ty {
                        Ty::Con(n) if n == ty_name::U32 || n == ty_name::I32 => 32,
                        Ty::Con(n) if n == "U16" => 16,
                        Ty::Con(n) if n == "U8" => 8,
                        _ => 32,
                    };
                    if offset > max_bits {
                        self.engine.diagnostics.push(
                            shadml_diagnostics::Diagnostic::error(format!(
                                "bitfield '{}' uses {} bits, but base type allows only {}",
                                name, offset, max_bits
                            ))
                            .with_label(shadml_diagnostics::Label::primary(*span, "too many bits"))
                            .with_help("reduce the number of fields or use a wider base type"),
                        );
                    }
                    self.bitfield_fields.insert(name.clone(), bf_meta);
                    bitfields.push(shadml_hir::HirBitfield {
                        name: name.clone(),
                        base_ty: base_scheme.ty,
                        fields: hir_fields,
                    });
                }
                Decl::ConstDecl {
                    name,
                    ty,
                    value,
                    span,
                    ..
                } => {
                    let scheme = self.convert_syntax_type_scheme(ty);
                    let mut local_env = self.env.clone();
                    let (hir_expr, _val_ty) = self.lower_expr(value, &mut local_env);
                    constants.push(shadml_hir::HirConst {
                        name: name.clone(),
                        ty: scheme.ty,
                        value: hir_expr,
                        span: *span,
                    });
                }
                Decl::TypeSig { .. }
                | Decl::BuiltinTypeDecl { .. }
                | Decl::TypeAlias { .. }
                | Decl::ExternDecl { .. }
                | Decl::BuiltinExternDecl { .. }
                | Decl::BuiltinImplDecl { .. } => {}
                Decl::ModuleDecl { .. } | Decl::ImportDecl { .. } => {
                    // Module/import declarations are handled at the module resolution level.
                }
                Decl::CfgDecl { .. } => {
                    // CfgDecl nodes are flattened by flatten_cfg_decls above — unreachable here.
                }
                Decl::RenderBlock {
                    name,
                    bindings: rb_bindings,
                    entries: rb_entries,
                    span: rspan,
                    comments: _,
                } => {
                    // Collect binding info for the render block metadata
                    let rb_hir_bindings: Vec<HirBinding> = rb_bindings
                        .iter()
                        .map(|b| {
                            if let Decl::BindingDecl {
                                name: bname,
                                ty: bty,
                                address_space,
                                group,
                                binding,
                                ..
                            } = b
                            {
                                let scheme = self.convert_syntax_type_scheme(bty);
                                HirBinding {
                                    name: bname.clone(),
                                    ty: scheme.ty,
                                    address_space: lower_address_space(*address_space),
                                    group: *group,
                                    binding: *binding,
                                }
                            } else {
                                panic!("Expected BindingDecl in render block bindings")
                            }
                        })
                        .collect();

                    // Find vertex and fragment entry names
                    let vertex_entry = rb_entries
                        .iter()
                        .find_map(|e| {
                            if let Decl::EntryPoint {
                                attributes, name, ..
                            } = e
                            {
                                if attributes.iter().any(|a| a.name == "vertex") {
                                    return Some(name.clone());
                                }
                            }
                            None
                        })
                        .unwrap_or_default();
                    let fragment_entry = rb_entries
                        .iter()
                        .find_map(|e| {
                            if let Decl::EntryPoint {
                                attributes, name, ..
                            } = e
                            {
                                if attributes.iter().any(|a| a.name == "fragment") {
                                    return Some(name.clone());
                                }
                            }
                            None
                        })
                        .unwrap_or_default();

                    render_blocks.push(HirRenderBlock {
                        name: name.clone(),
                        bindings: rb_hir_bindings,
                        vertex_entry,
                        fragment_entry,
                        span: *rspan,
                    });

                    // Also process bindings and entries from the render block
                    // into the main program (they need to be type-checked and lowered)
                    for rb_decl in rb_bindings.iter().chain(rb_entries.iter()) {
                        match rb_decl {
                            Decl::BindingDecl {
                                name: bname,
                                ty: bty,
                                address_space,
                                group,
                                binding,
                                ..
                            } => {
                                let scheme = self.convert_syntax_type_scheme(bty);
                                bindings.push(HirBinding {
                                    name: bname.clone(),
                                    ty: scheme.ty,
                                    address_space: lower_address_space(*address_space),
                                    group: *group,
                                    binding: *binding,
                                });
                            }
                            Decl::EntryPoint {
                                attributes,
                                name: ename,
                                params,
                                body,
                                span: espan,
                                comments: ecomments,
                            } => {
                                if let Some(ep) = self.lower_entry_point(
                                    attributes,
                                    ename,
                                    params,
                                    body,
                                    *espan,
                                    ecomments.clone(),
                                ) {
                                    entry_points.push(ep);
                                }
                            }
                            _ => {
                                // TypeSig and other declarations inside render blocks
                                // are already handled at module scope by the semantic analyzer.
                            }
                        }
                    }
                }
                Decl::TraitDecl { .. } => {
                    // Trait declarations are type-level only — no HIR output.
                }
                Decl::ImplDecl {
                    trait_name,
                    tys,
                    associated_types,
                    methods,
                    span: _,
                    comments,
                } => {
                    let impl_tys: Vec<Ty> = tys
                        .iter()
                        .map(|ty| self.convert_syntax_type_scheme(ty).ty)
                        .collect();
                    let type_suffix = impl_tys
                        .iter()
                        .map(shadml_semantic::format_type_suffix)
                        .collect::<Vec<_>>()
                        .join("__");

                    if let Some(tname) = trait_name {
                        // Trait impl: look up trait method types
                        let mut method_info: Vec<(String, Ty)> = Vec::new();
                        // Collect associated type bindings from this impl's decl
                        let assoc_bindings: HashMap<String, Ty> = associated_types
                            .iter()
                            .map(|at| {
                                (
                                    at.name.clone(),
                                    normalize_type_aliases(
                                        &self.convert_syntax_type_scheme(&at.ty).ty,
                                    ),
                                )
                            })
                            .collect();
                        if let Some(trait_info) = self.traits.get(tname) {
                            for m in methods {
                                let logical_name = match (tname.as_str(), m.name.as_str()) {
                                    ("Neg", "-") => "negate".to_owned(),
                                    ("BitNot", "~") => "bitnot".to_owned(),
                                    ("Shr", ">>") => "shr".to_owned(),
                                    _ => m.name.clone(),
                                };
                                let mangled = shadml_semantic::mangle_instance_method(
                                    &logical_name,
                                    &type_suffix,
                                );
                                for (tmethod_name, tmethod_ty) in &trait_info.methods {
                                    if tmethod_name == &logical_name {
                                        let concrete_ty = shadml_semantic::replace_trait_vars(
                                            tmethod_ty,
                                            &trait_info.var_ids,
                                            &impl_tys,
                                        );
                                        let concrete_ty =
                                            shadml_semantic::resolve_assoc_projections(
                                                &concrete_ty,
                                                &assoc_bindings,
                                            );
                                        method_info.push((mangled.clone(), concrete_ty));
                                    }
                                }
                            }
                        }
                        for (i, m) in methods.iter().enumerate() {
                            if let Some((mangled, concrete_ty)) = method_info.get(i) {
                                let logical_name = match (tname.as_str(), m.name.as_str()) {
                                    ("Neg", "-") => "negate".to_owned(),
                                    ("BitNot", "~") => "bitnot".to_owned(),
                                    ("Shr", ">>") => "shr".to_owned(),
                                    _ => m.name.clone(),
                                };
                                if let Some(f) = self.lower_impl_method(
                                    &logical_name,
                                    mangled,
                                    &m.params,
                                    &m.body,
                                    concrete_ty,
                                    m.span,
                                    comments.clone(),
                                    false,
                                ) {
                                    functions.push(f);
                                }
                            }
                        }
                    } else {
                        // Standalone impl: lower each method as a regular function
                        // with either an explicit method signature or the impl
                        // type as the first parameter type.
                        for m in methods {
                            let mangled =
                                shadml_semantic::mangle_instance_method(&m.name, &type_suffix);
                            let lowered = if let Some(method_ty) = &m.ty {
                                let concrete_ty = self.convert_syntax_type_scheme(method_ty).ty;
                                self.lower_impl_method(
                                    &m.name,
                                    &mangled,
                                    &m.params,
                                    &m.body,
                                    &concrete_ty,
                                    m.span,
                                    comments.clone(),
                                    true,
                                )
                            } else {
                                self.lower_standalone_impl_method(
                                    &m.name,
                                    &mangled,
                                    &m.params,
                                    &m.body,
                                    &impl_tys[0],
                                    m.span,
                                    comments.clone(),
                                )
                            };
                            if let Some(f) = lowered {
                                // Register the inferred function type so subsequent
                                // call sites (method-call syntax, pipelines) resolve
                                // the correct return type.
                                let mut fun_ty = f.return_ty.clone();
                                for (_, pty) in f.params.iter().rev() {
                                    fun_ty = Ty::arrow(pty.clone(), fun_ty);
                                }
                                let scheme = Scheme::mono(fun_ty);
                                self.env.insert(mangled.clone(), scheme);
                                functions.push(f);
                            }
                        }
                    }
                }
            }
        }

        let program = self.monomorphize_generic_functions(HirProgram {
            functions,
            data_types,
            entry_points,
            bindings,
            bitfields,
            constants,
            render_blocks,
        });
        self.eliminate_tuple_abi(program)
    }
}

mod decl;
mod expr;
mod helpers;
mod monomorphize;
mod pattern;
mod ty;

impl AstLowering {
    pub fn has_errors(&self) -> bool {
        self.engine.diagnostics.has_errors()
    }

    pub fn diagnostics(&self) -> &shadml_diagnostics::DiagnosticSink {
        &self.engine.diagnostics
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shadml_parser::parser::Parser;
    use shadml_semantic::SemanticAnalyzer;
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

    fn lower_source(source: &str) -> (AstLowering, HirProgram) {
        let mut parser = Parser::new(source);
        let mut program = parser.parse_program();
        assert!(
            !parser.diagnostics().has_errors(),
            "parse errors: {:?}",
            parser.diagnostics().iter().collect::<Vec<_>>()
        );
        with_prelude(&mut program);

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);
        assert!(
            !sa.has_errors(),
            "semantic errors: {:?}",
            sa.diagnostics().iter().collect::<Vec<_>>()
        );

        let mut lowering = AstLowering::new(&sa);
        let hir = lowering.lower_program(&program);
        (lowering, hir)
    }

    #[test]
    fn test_lower_add_function() {
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

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);
        assert!(!sa.has_errors());

        let mut lowering = AstLowering::new(&sa);
        let hir = lowering.lower_program(&program);

        assert_eq!(hir.functions.len(), 1);
        let f = &hir.functions[0];
        assert_eq!(f.name, "add");
        assert_eq!(f.params.len(), 2);
        assert!(matches!(f.body, HirExpr::BinOp(BinOp::Add, _, _, _, _)));
    }

    #[test]
    fn test_lower_empty_program() {
        let mut program = Program { decls: vec![] };
        with_prelude(&mut program);
        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);
        let mut lowering = AstLowering::new(&sa);
        let hir = lowering.lower_program(&program);
        // Prelude contributes data types but no user functions or entry points
        assert!(hir.entry_points.is_empty());
    }

    #[test]
    fn test_lower_where_clause() {
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
            }],
        };
        with_prelude(&mut program);

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);
        assert!(!sa.has_errors());

        let mut lowering = AstLowering::new(&sa);
        let hir = lowering.lower_program(&program);

        assert_eq!(hir.functions.len(), 1);
        let f = &hir.functions[0];
        match &f.body {
            HirExpr::Let(binds, body, _, _) => {
                assert_eq!(binds.len(), 1);
                assert_eq!(binds[0].0, "y");
                assert!(matches!(
                    body.as_ref(),
                    HirExpr::BinOp(BinOp::Add, _, _, _, _)
                ));
            }
            other => panic!("expected HirExpr::Let, got {:?}", other),
        }
    }

    #[test]
    fn test_lower_data_type() {
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

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);
        let mut lowering = AstLowering::new(&sa);
        let hir = lowering.lower_program(&program);

        let dt = hir
            .data_types
            .iter()
            .find(|dt| dt.name == "Color")
            .expect("Color data type");
        assert_eq!(dt.name, "Color");
        assert_eq!(dt.constructors.len(), 3);
        assert_eq!(dt.constructors[0].name, "Red");
        assert_eq!(dt.constructors[0].tag, 0);
        assert_eq!(dt.constructors[1].name, "Green");
        assert_eq!(dt.constructors[1].tag, 1);
        assert_eq!(dt.constructors[2].name, "Blue");
        assert_eq!(dt.constructors[2].tag, 2);
    }

    #[test]
    fn test_where_bound_length_of_vector_subtraction_concretizes_types() {
        let source = include_str!("../../../examples/shadorial/02-uniforms.shadml");
        let (lowering, hir) = lower_source(source);
        assert!(
            !lowering.has_errors(),
            "lowering errors: {:?}",
            lowering.diagnostics().iter().collect::<Vec<_>>()
        );

        let shade = hir
            .functions
            .iter()
            .find(|f| f.name == "shade")
            .expect("shade function");

        let HirExpr::Let(binds, _, _, _) = &shade.body else {
            panic!("expected where-clause to lower to let-bindings");
        };

        let dist_expr = binds
            .iter()
            .find(|(name, _)| name == "dist")
            .map(|(_, expr)| expr)
            .expect("dist binding");
        assert_eq!(dist_expr.ty(), &Ty::f32());

        let HirExpr::App(_, arg, _, _) = dist_expr else {
            panic!("expected dist to be a length application");
        };
        assert_eq!(arg.ty(), &vector_ty(2, Ty::f32()));
    }

    #[test]
    fn test_unary_negation_remains_concrete_for_vector_length() {
        let source = r#"
shade : Vec 2 F32 -> Vec 4 F32
shade fragCoord = vec4 0.0 0.0 (length (-fragCoord)) 1.0
"#;
        let (lowering, hir) = lower_source(source);
        assert!(
            !lowering.has_errors(),
            "lowering errors: {:?}",
            lowering.diagnostics().iter().collect::<Vec<_>>()
        );

        let shade = hir
            .functions
            .iter()
            .find(|f| f.name == "shade")
            .expect("shade function");
        assert_eq!(shade.return_ty, vector_ty(4, Ty::f32()));
    }
}
