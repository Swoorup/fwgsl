mod catalog;

use std::collections::{HashMap, HashSet};

pub use catalog::{
    all_completion_specs, completion_item_from_spec, lookup_completion_spec, spec_matches_context,
    CompletionContext, CompletionSpec,
};
use lsp_types::{
    CompletionItem, CompletionItemKind, Documentation, GotoDefinitionResponse, Hover,
    HoverContents, Location, MarkupContent, MarkupKind, Position, Range, Url,
};
use shadml_parser::lexer::Token;
use shadml_parser::parser::{Attribute, ConFields, Decl, DoStmt, Expr, Pat, Program, Type};
use shadml_parser::{lex, Parser};
use shadml_semantic::SemanticAnalyzer;
use shadml_span::Span;
use shadml_syntax::SyntaxKind;
use shadml_typechecker::{
    format_scheme_surface, format_ty_surface_inferred, InferEngine, Scheme,
};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Namespace {
    Value,
    Type,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum OccurrenceRole {
    Definition,
    Reference,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum SymbolKind {
    Function,
    EntryPoint,
    Parameter,
    LocalBinding,
    PatternBinding,
    BuiltinType,
    DataType,
    TypeAlias,
    AssociatedType,
    Constructor,
    TypeParameter,
    RecordField,
}

#[derive(Clone, Debug)]
struct Symbol {
    id: usize,
    name: String,
    namespace: Namespace,
    kind: SymbolKind,
    primary_span: Span,
    definition_spans: Vec<Span>,
    scope_span: Span,
    scope_depth: usize,
    visible_from: u32,
    container: Option<String>,
}

struct NewSymbol {
    name: String,
    namespace: Namespace,
    kind: SymbolKind,
    span: Span,
    scope_span: Span,
    scope_depth: usize,
    visible_from: u32,
    container: Option<String>,
}

#[derive(Clone, Debug)]
struct Occurrence {
    symbol_id: usize,
    span: Span,
    role: OccurrenceRole,
}

#[derive(Default)]
struct DocumentIndex {
    symbols: Vec<Symbol>,
    occurrences: Vec<Occurrence>,
}

struct DocumentState<'a> {
    source: &'a str,
    analyzer: SemanticAnalyzer,
    index: DocumentIndex,
    symbol_types: HashMap<Span, String>,
    explicit_signatures: HashMap<String, String>,
    /// Doc comments extracted from declarations, keyed by symbol name.
    doc_comments: HashMap<String, String>,
    /// Record field types, keyed by field name (for hover display).
    field_types: HashMap<String, String>,
}

struct IdeState<'a> {
    source: &'a str,
    analyzer: SemanticAnalyzer,
    index: DocumentIndex,
    symbol_types: HashMap<Span, String>,
    explicit_signatures: HashMap<String, String>,
    doc_comments: HashMap<String, String>,
    field_types: HashMap<String, String>,
    prelude: Option<DocumentState<'static>>,
}

#[derive(Clone)]
struct ScopeFrame {
    span: Span,
    container: Option<String>,
    value_defs: HashMap<String, usize>,
    type_defs: HashMap<String, usize>,
}

impl ScopeFrame {
    fn new(span: Span, container: Option<String>) -> Self {
        Self {
            span,
            container,
            value_defs: HashMap::new(),
            type_defs: HashMap::new(),
        }
    }
}

impl DocumentIndex {
    fn push_symbol(&mut self, symbol: NewSymbol) -> usize {
        let id = self.symbols.len();
        self.symbols.push(Symbol {
            id,
            name: symbol.name,
            namespace: symbol.namespace,
            kind: symbol.kind,
            primary_span: symbol.span,
            definition_spans: vec![symbol.span],
            scope_span: symbol.scope_span,
            scope_depth: symbol.scope_depth,
            visible_from: symbol.visible_from,
            container: symbol.container,
        });
        self.push_occurrence(id, symbol.span, OccurrenceRole::Definition);
        id
    }

    fn add_definition_span(&mut self, symbol_id: usize, span: Span) {
        let symbol = &mut self.symbols[symbol_id];
        let is_new = !symbol.definition_spans.contains(&span);
        if is_new {
            symbol.definition_spans.push(span);
        }
        if symbol.primary_span.start > span.start {
            symbol.primary_span = span;
        }
        if is_new {
            self.push_occurrence(symbol_id, span, OccurrenceRole::Definition);
        }
    }

    fn push_occurrence(&mut self, symbol_id: usize, span: Span, role: OccurrenceRole) {
        self.occurrences.push(Occurrence {
            symbol_id,
            span,
            role,
        });
    }

    fn symbol_at_offset(&self, offset: u32) -> Option<&Occurrence> {
        self.occurrences
            .iter()
            .find(|occurrence| occurrence.span.start <= offset && offset < occurrence.span.end)
    }

    fn visible_symbols(
        &self,
        offset: u32,
        context: CompletionContext,
    ) -> impl Iterator<Item = &Symbol> {
        self.symbols.iter().filter(move |symbol| {
            if symbol.visible_from > offset {
                return false;
            }
            if !(symbol.scope_span.start <= offset && offset <= symbol.scope_span.end) {
                return false;
            }
            match context {
                CompletionContext::Attribute => false,
                CompletionContext::Type => symbol.namespace == Namespace::Type,
                CompletionContext::Value => true,
            }
        })
    }
}

struct IndexBuilder<'a> {
    source: &'a str,
    tokens: Vec<Token>,
    index: DocumentIndex,
    top_level_values: HashMap<String, usize>,
    top_level_types: HashMap<String, usize>,
    impl_method_symbols: HashMap<String, Vec<usize>>,
    /// Record field name → symbol ID, for resolving field access and field init.
    field_symbols: HashMap<String, usize>,
}

impl<'a> IndexBuilder<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            tokens: lex(source),
            index: DocumentIndex::default(),
            top_level_values: HashMap::new(),
            top_level_types: HashMap::new(),
            impl_method_symbols: HashMap::new(),
            field_symbols: HashMap::new(),
        }
    }

    fn build(mut self, program: &Program) -> DocumentIndex {
        let whole_file = Span::new(0, self.source.len() as u32);
        self.collect_top_level(program, whole_file);
        self.walk_program(program, whole_file);
        self.index
    }

    fn collect_top_level(&mut self, program: &Program, whole_file: Span) {
        let all_decls = Decl::flatten_cfg_decls(&program.decls);
        for decl in &all_decls {
            match decl {
                Decl::TypeSig { name, span, .. } => {
                    let name_span = self.first_name_span(name, *span).unwrap_or(*span);
                    let symbol_id = self.top_level_values.get(name).copied().unwrap_or_else(|| {
                        let id = self.index.push_symbol(NewSymbol {
                            name: name.clone(),
                            namespace: Namespace::Value,
                            kind: SymbolKind::Function,
                            span: name_span,
                            scope_span: whole_file,
                            scope_depth: 0,
                            visible_from: 0,
                            container: Some(name.clone()),
                        });
                        self.top_level_values.insert(name.clone(), id);
                        id
                    });
                    self.index.add_definition_span(symbol_id, name_span);
                }
                Decl::FunDecl { name, span, .. }
                | Decl::ExternDecl { name, span, .. }
                | Decl::BuiltinExternDecl { name, span, .. } => {
                    let name_span = self.first_name_span(name, *span).unwrap_or(*span);
                    let symbol_id = self.top_level_values.get(name).copied().unwrap_or_else(|| {
                        let id = self.index.push_symbol(NewSymbol {
                            name: name.clone(),
                            namespace: Namespace::Value,
                            kind: SymbolKind::Function,
                            span: name_span,
                            scope_span: whole_file,
                            scope_depth: 0,
                            visible_from: 0,
                            container: Some(name.clone()),
                        });
                        self.top_level_values.insert(name.clone(), id);
                        id
                    });
                    self.index.add_definition_span(symbol_id, name_span);
                }
                Decl::EntryPoint { name, span, .. } => {
                    let name_span = self.first_name_span(name, *span).unwrap_or(*span);
                    let symbol_id = self.top_level_values.get(name).copied().unwrap_or_else(|| {
                        let id = self.index.push_symbol(NewSymbol {
                            name: name.clone(),
                            namespace: Namespace::Value,
                            kind: SymbolKind::EntryPoint,
                            span: name_span,
                            scope_span: whole_file,
                            scope_depth: 0,
                            visible_from: 0,
                            container: Some(name.clone()),
                        });
                        self.top_level_values.insert(name.clone(), id);
                        id
                    });
                    self.index.symbols[symbol_id].kind = SymbolKind::EntryPoint;
                    self.index.add_definition_span(symbol_id, name_span);
                }
                Decl::BuiltinTypeDecl {
                    name, span, arity, ..
                } => {
                    let name_span = self.first_name_span(name, *span).unwrap_or(*span);
                    let symbol_id = self.top_level_types.get(name).copied().unwrap_or_else(|| {
                        let id = self.index.push_symbol(NewSymbol {
                            name: name.clone(),
                            namespace: Namespace::Type,
                            kind: SymbolKind::BuiltinType,
                            span: name_span,
                            scope_span: whole_file,
                            scope_depth: 0,
                            visible_from: 0,
                            container: Some(format!("builtin-type/{}", arity)),
                        });
                        self.top_level_types.insert(name.clone(), id);
                        id
                    });
                    self.index.symbols[symbol_id].kind = SymbolKind::BuiltinType;
                    self.index.add_definition_span(symbol_id, name_span);
                }
                Decl::DataDecl {
                    name,
                    constructors,
                    span,
                    ..
                } => {
                    let type_name_span = self.first_name_span(name, *span).unwrap_or(*span);
                    let type_id = self.top_level_types.get(name).copied().unwrap_or_else(|| {
                        let id = self.index.push_symbol(NewSymbol {
                            name: name.clone(),
                            namespace: Namespace::Type,
                            kind: SymbolKind::DataType,
                            span: type_name_span,
                            scope_span: whole_file,
                            scope_depth: 0,
                            visible_from: 0,
                            container: Some(name.clone()),
                        });
                        self.top_level_types.insert(name.clone(), id);
                        id
                    });
                    self.index.symbols[type_id].kind = SymbolKind::DataType;
                    self.index.add_definition_span(type_id, type_name_span);

                    for constructor in constructors {
                        let constructor_span = self
                            .first_name_span(&constructor.name, constructor.span)
                            .unwrap_or(constructor.span);
                        let constructor_id = self.index.push_symbol(NewSymbol {
                            name: constructor.name.clone(),
                            namespace: Namespace::Value,
                            kind: SymbolKind::Constructor,
                            span: constructor_span,
                            scope_span: whole_file,
                            scope_depth: 0,
                            visible_from: 0,
                            container: Some(name.clone()),
                        });
                        self.top_level_values
                            .insert(constructor.name.clone(), constructor_id);
                    }
                }
                Decl::TypeAlias { name, span, .. } => {
                    let name_span = self.first_name_span(name, *span).unwrap_or(*span);
                    let symbol_id = self.top_level_types.get(name).copied().unwrap_or_else(|| {
                        let id = self.index.push_symbol(NewSymbol {
                            name: name.clone(),
                            namespace: Namespace::Type,
                            kind: SymbolKind::TypeAlias,
                            span: name_span,
                            scope_span: whole_file,
                            scope_depth: 0,
                            visible_from: 0,
                            container: Some(name.clone()),
                        });
                        self.top_level_types.insert(name.clone(), id);
                        id
                    });
                    self.index.symbols[symbol_id].kind = SymbolKind::TypeAlias;
                    self.index.add_definition_span(symbol_id, name_span);
                }
                Decl::BindingDecl { name, span, .. } => {
                    let name_span = self.first_name_span(name, *span).unwrap_or(*span);
                    let symbol_id = self.top_level_values.get(name).copied().unwrap_or_else(|| {
                        let id = self.index.push_symbol(NewSymbol {
                            name: name.clone(),
                            namespace: Namespace::Value,
                            kind: SymbolKind::Function,
                            span: name_span,
                            scope_span: whole_file,
                            scope_depth: 0,
                            visible_from: 0,
                            container: Some(name.clone()),
                        });
                        self.top_level_values.insert(name.clone(), id);
                        id
                    });
                    self.index.add_definition_span(symbol_id, name_span);
                }
                Decl::BitfieldDecl { name, span, .. } => {
                    let name_span = self.first_name_span(name, *span).unwrap_or(*span);
                    let symbol_id = self.top_level_types.get(name).copied().unwrap_or_else(|| {
                        let id = self.index.push_symbol(NewSymbol {
                            name: name.clone(),
                            namespace: Namespace::Type,
                            kind: SymbolKind::TypeAlias,
                            span: name_span,
                            scope_span: whole_file,
                            scope_depth: 0,
                            visible_from: 0,
                            container: Some(name.clone()),
                        });
                        self.top_level_types.insert(name.clone(), id);
                        id
                    });
                    self.index.add_definition_span(symbol_id, name_span);
                }
                Decl::ConstDecl { name, span, .. } => {
                    let name_span = self.first_name_span(name, *span).unwrap_or(*span);
                    let symbol_id = self.top_level_values.get(name).copied().unwrap_or_else(|| {
                        let id = self.index.push_symbol(NewSymbol {
                            name: name.clone(),
                            namespace: Namespace::Value,
                            kind: SymbolKind::Function,
                            span: name_span,
                            scope_span: whole_file,
                            scope_depth: 0,
                            visible_from: 0,
                            container: Some(name.clone()),
                        });
                        self.top_level_values.insert(name.clone(), id);
                        id
                    });
                    self.index.add_definition_span(symbol_id, name_span);
                }
                Decl::TraitDecl {
                    name,
                    span,
                    methods,
                    associated_types,
                    ..
                } => {
                    let name_span = self.first_name_span(name, *span).unwrap_or(*span);
                    let symbol_id = self.index.push_symbol(NewSymbol {
                        name: name.clone(),
                        namespace: Namespace::Type,
                        kind: SymbolKind::TypeAlias,
                        span: name_span,
                        scope_span: whole_file,
                        scope_depth: 0,
                        visible_from: 0,
                        container: Some(name.clone()),
                    });
                    self.top_level_types.insert(name.clone(), symbol_id);
                    for at in associated_types {
                        let at_span = self.first_name_span(&at.name, at.span).unwrap_or(at.span);
                        let at_id = self.index.push_symbol(NewSymbol {
                            name: at.name.clone(),
                            namespace: Namespace::Type,
                            kind: SymbolKind::AssociatedType,
                            span: at_span,
                            scope_span: whole_file,
                            scope_depth: 0,
                            visible_from: 0,
                            container: Some(name.clone()),
                        });
                        self.top_level_types.insert(at.name.clone(), at_id);
                    }
                    for m in methods {
                        let mspan = self.first_name_span(&m.name, m.span).unwrap_or(m.span);
                        let mid = self.index.push_symbol(NewSymbol {
                            name: m.name.clone(),
                            namespace: Namespace::Value,
                            kind: SymbolKind::Function,
                            span: mspan,
                            scope_span: whole_file,
                            scope_depth: 0,
                            visible_from: 0,
                            container: Some(name.clone()),
                        });
                        self.top_level_values.insert(m.name.clone(), mid);
                    }
                }
                Decl::ImplDecl {
                    trait_name,
                    tys,
                    methods,
                    span,
                    associated_types,
                    ..
                } => {
                    let container = match trait_name {
                        Some(trait_name) => format!(
                            "impl {} {}",
                            trait_name,
                            tys.iter().map(format_type).collect::<Vec<_>>().join(" ")
                        ),
                        None => format!("impl {}", format_type(&tys[0])),
                    };
                    for at in associated_types {
                        let at_span = self.first_name_span(&at.name, at.span).unwrap_or(at.span);
                        let at_id = self.index.push_symbol(NewSymbol {
                            name: at.name.clone(),
                            namespace: Namespace::Type,
                            kind: SymbolKind::AssociatedType,
                            span: at_span,
                            scope_span: *span,
                            scope_depth: 0,
                            visible_from: 0,
                            container: Some(container.clone()),
                        });
                        self.top_level_types.insert(at.name.clone(), at_id);
                    }
                    for m in methods {
                        let mspan = self.first_name_span(&m.name, m.span).unwrap_or(m.span);
                        let mid = self.index.push_symbol(NewSymbol {
                            name: m.name.clone(),
                            namespace: Namespace::Value,
                            kind: SymbolKind::Function,
                            span: mspan,
                            scope_span: *span,
                            scope_depth: 0,
                            visible_from: 0,
                            container: Some(container.clone()),
                        });
                        self.impl_method_symbols
                            .entry(m.name.clone())
                            .or_default()
                            .push(mid);
                    }
                }
                Decl::BuiltinImplDecl {
                    trait_name,
                    tys,
                    methods,
                    span,
                    associated_types,
                    ..
                } => {
                    let container = format!(
                        "builtin impl {} {}",
                        trait_name,
                        tys.iter().map(format_type).collect::<Vec<_>>().join(" ")
                    );
                    for at in associated_types {
                        let at_span = self.first_name_span(&at.name, at.span).unwrap_or(at.span);
                        let at_id = self.index.push_symbol(NewSymbol {
                            name: at.name.clone(),
                            namespace: Namespace::Type,
                            kind: SymbolKind::AssociatedType,
                            span: at_span,
                            scope_span: *span,
                            scope_depth: 0,
                            visible_from: 0,
                            container: Some(container.clone()),
                        });
                        self.top_level_types.insert(at.name.clone(), at_id);
                    }
                    for m in methods {
                        let mspan = self.first_name_span(&m.name, m.span).unwrap_or(m.span);
                        let mid = self.index.push_symbol(NewSymbol {
                            name: m.name.clone(),
                            namespace: Namespace::Value,
                            kind: SymbolKind::Function,
                            span: mspan,
                            scope_span: *span,
                            scope_depth: 0,
                            visible_from: 0,
                            container: Some(container.clone()),
                        });
                        self.impl_method_symbols
                            .entry(m.name.clone())
                            .or_default()
                            .push(mid);
                    }
                }
                Decl::ModuleDecl { .. } | Decl::ImportDecl { .. } => {
                    // Module/import declarations don't define symbols.
                }
                Decl::CfgDecl { .. } => {
                    // CfgDecl nodes are flattened above — unreachable here.
                }
            }
        }
    }

    fn walk_program(&mut self, program: &Program, whole_file: Span) {
        let mut frames = vec![ScopeFrame::new(whole_file, None)];
        frames[0].value_defs = self.top_level_values.clone();
        frames[0].type_defs = self.top_level_types.clone();

        let all_decls = Decl::flatten_cfg_decls(&program.decls);
        for decl in &all_decls {
            self.walk_decl(decl, &mut frames);
        }
    }

    fn walk_decl(&mut self, decl: &Decl, frames: &mut Vec<ScopeFrame>) {
        match decl {
            Decl::TypeSig { ty, .. } => {
                self.walk_type(ty, frames);
            }
            Decl::FunDecl {
                name,
                params,
                body,
                where_binds,
                span,
                ..
            } => {
                self.walk_callable(name, params, body, where_binds, *span, frames);
            }
            Decl::EntryPoint {
                attributes,
                name,
                params,
                body,
                span,
                ..
            } => {
                for attribute in attributes {
                    self.walk_attribute(attribute);
                }
                self.walk_callable(name, params, body, &[], *span, frames);
            }
            Decl::BuiltinTypeDecl { .. } => {}
            Decl::DataDecl {
                name,
                type_params,
                constructors,
                span,
                ..
            } => {
                frames.push(ScopeFrame::new(*span, Some(name.clone())));
                for type_param in type_params {
                    let type_param_span = self.first_name_span(type_param, *span).unwrap_or(*span);
                    let symbol_id = self.index.push_symbol(NewSymbol {
                        name: type_param.clone(),
                        namespace: Namespace::Type,
                        kind: SymbolKind::TypeParameter,
                        span: type_param_span,
                        scope_span: *span,
                        scope_depth: frames.len() - 1,
                        visible_from: span.start,
                        container: Some(name.clone()),
                    });
                    frames
                        .last_mut()
                        .expect("type scope")
                        .type_defs
                        .insert(type_param.clone(), symbol_id);
                }
                for constructor in constructors {
                    match &constructor.fields {
                        shadml_parser::parser::ConFields::Positional(fields) => {
                            for field in fields {
                                self.walk_type(field, frames);
                            }
                        }
                        shadml_parser::parser::ConFields::Record(fields) => {
                            for f in fields {
                                self.walk_type(&f.ty, frames);
                                // Register field name as a symbol for hover
                                let field_span = self
                                    .first_name_span(&f.name, constructor.span)
                                    .unwrap_or(constructor.span);
                                let fid = self.index.push_symbol(NewSymbol {
                                    name: f.name.clone(),
                                    namespace: Namespace::Value,
                                    kind: SymbolKind::RecordField,
                                    span: field_span,
                                    scope_span: *span,
                                    scope_depth: frames.len(),
                                    visible_from: span.start,
                                    container: Some(name.clone()),
                                });
                                self.field_symbols.insert(f.name.clone(), fid);
                            }
                        }
                        shadml_parser::parser::ConFields::Empty => {}
                    }
                }
                frames.pop();
            }
            Decl::TypeAlias {
                name,
                params,
                ty,
                span,
                ..
            } => {
                frames.push(ScopeFrame::new(*span, Some(name.clone())));
                for type_param in params {
                    let type_param_span = self.first_name_span(type_param, *span).unwrap_or(*span);
                    let symbol_id = self.index.push_symbol(NewSymbol {
                        name: type_param.clone(),
                        namespace: Namespace::Type,
                        kind: SymbolKind::TypeParameter,
                        span: type_param_span,
                        scope_span: *span,
                        scope_depth: frames.len() - 1,
                        visible_from: span.start,
                        container: Some(name.clone()),
                    });
                    frames
                        .last_mut()
                        .expect("type scope")
                        .type_defs
                        .insert(type_param.clone(), symbol_id);
                }
                self.walk_type(ty, frames);
                frames.pop();
            }
            Decl::BindingDecl { ty, .. } => {
                self.walk_type(ty, frames);
            }
            Decl::BitfieldDecl {
                name,
                base_ty,
                fields,
                span,
                ..
            } => {
                self.walk_type(base_ty, frames);
                // Register each bitfield field as a symbol for hover/goto-def
                for f in fields {
                    let field_span = self.first_name_span(&f.name, *span).unwrap_or(f.span);
                    let fid = self.index.push_symbol(NewSymbol {
                        name: f.name.clone(),
                        namespace: Namespace::Value,
                        kind: SymbolKind::RecordField,
                        span: field_span,
                        scope_span: *span,
                        scope_depth: frames.len(),
                        visible_from: span.start,
                        container: Some(name.clone()),
                    });
                    self.field_symbols.insert(f.name.clone(), fid);
                }
            }
            Decl::ConstDecl { ty, value, .. } => {
                self.walk_type(ty, frames);
                self.walk_expr(value, frames);
            }
            Decl::TraitDecl {
                methods, associated_types, ..
            } => {
                for at in associated_types {
                    if let Some(at_id) = self.top_level_types.get(&at.name).copied() {
                        self.index
                            .push_occurrence(at_id, at.span, OccurrenceRole::Definition);
                    }
                }
                for m in methods {
                    self.walk_type(&m.ty, frames);
                }
            }
            Decl::ImplDecl {
                tys,
                methods,
                associated_types,
                ..
            } => {
                for ty in tys {
                    self.walk_type(ty, frames);
                }
                for at in associated_types {
                    self.walk_type(&at.ty, frames);
                }
                for m in methods {
                    if let Some(method_ty) = &m.ty {
                        self.walk_type(method_ty, frames);
                    }
                    self.walk_callable(&m.name, &m.params, &m.body, &[], m.span, frames);
                }
            }
            Decl::BuiltinImplDecl {
                tys, associated_types, ..
            } => {
                for ty in tys {
                    self.walk_type(ty, frames);
                }
                for at in associated_types {
                    self.walk_type(&at.ty, frames);
                }
            }
            Decl::ExternDecl { ty, .. } | Decl::BuiltinExternDecl { ty, .. } => {
                self.walk_type(ty, frames);
            }
            Decl::ModuleDecl { .. } | Decl::ImportDecl { .. } => {}
            Decl::CfgDecl { .. } => {
                // CfgDecl nodes are flattened by walk_program — unreachable here.
            }
        }
    }

    fn walk_callable(
        &mut self,
        name: &str,
        params: &[Pat],
        body: &Expr,
        where_binds: &[shadml_parser::parser::LocalBind],
        span: Span,
        frames: &mut Vec<ScopeFrame>,
    ) {
        let container = Some(name.to_owned());
        frames.push(ScopeFrame::new(span, container.clone()));
        for param in params {
            self.define_pattern(param, span.start, frames, SymbolKind::Parameter);
        }

        let where_depth = frames.len() - 1;
        for bind in where_binds {
            let symbol_id = self.index.push_symbol(NewSymbol {
                name: bind.name.clone(),
                namespace: Namespace::Value,
                kind: SymbolKind::LocalBinding,
                span: bind.name_span,
                scope_span: span,
                scope_depth: where_depth,
                visible_from: span.start,
                container: container.clone(),
            });
            frames[where_depth]
                .value_defs
                .insert(bind.name.clone(), symbol_id);
        }

        for bind in where_binds {
            self.walk_expr(&bind.expr, frames);
        }
        self.walk_expr(body, frames);
        frames.pop();
    }

    fn walk_attribute(&mut self, _attribute: &Attribute) {}

    fn walk_expr(&mut self, expr: &Expr, frames: &mut Vec<ScopeFrame>) {
        match expr {
            Expr::Lit(_, _) | Expr::OpSection(_, _) => {}
            Expr::Var(name, span) | Expr::Con(name, span) => {
                if let Some(symbol_id) = self.resolve_value(name, frames) {
                    self.index
                        .push_occurrence(symbol_id, *span, OccurrenceRole::Reference);
                }
            }
            Expr::App(left, right, _) => {
                self.walk_expr(left, frames);
                self.walk_expr(right, frames);
            }
            Expr::Infix(left, _, right, _) => {
                self.walk_expr(left, frames);
                self.walk_expr(right, frames);
            }
            Expr::Lambda(params, body, span) => {
                let container = frames.last().and_then(|frame| frame.container.clone());
                frames.push(ScopeFrame::new(*span, container));
                for param in params {
                    self.define_pattern(param, span.start, frames, SymbolKind::Parameter);
                }
                self.walk_expr(body, frames);
                frames.pop();
            }
            Expr::Let(bindings, body, span) => {
                let container = frames.last().and_then(|frame| frame.container.clone());
                frames.push(ScopeFrame::new(*span, container));
                let depth = frames.len() - 1;
                for bind in bindings {
                    let symbol_id = self.index.push_symbol(NewSymbol {
                        name: bind.name.clone(),
                        namespace: Namespace::Value,
                        kind: SymbolKind::LocalBinding,
                        span: bind.name_span,
                        scope_span: *span,
                        scope_depth: depth,
                        visible_from: span.start,
                        container: frames[depth].container.clone(),
                    });
                    frames[depth].value_defs.insert(bind.name.clone(), symbol_id);
                }
                for bind in bindings {
                    self.walk_expr(&bind.expr, frames);
                }
                self.walk_expr(body, frames);
                frames.pop();
            }
            Expr::Case(scrutinee, arms, _) => {
                self.walk_expr(scrutinee, frames);
                for (pattern, guard, body) in arms {
                    let container = frames.last().and_then(|frame| frame.container.clone());
                    frames.push(ScopeFrame::new(body.span(), container));
                    self.define_pattern(
                        pattern,
                        body.span().start,
                        frames,
                        SymbolKind::PatternBinding,
                    );
                    if let Some(guard_expr) = guard {
                        self.walk_expr(guard_expr, frames);
                    }
                    self.walk_expr(body, frames);
                    frames.pop();
                }
            }
            Expr::If(condition, then_branch, else_branch, _) => {
                self.walk_expr(condition, frames);
                self.walk_expr(then_branch, frames);
                self.walk_expr(else_branch, frames);
            }
            Expr::Paren(inner, _)
            | Expr::Neg(inner, _)
            | Expr::Not(inner, _)
            | Expr::BitNot(inner, _) => {
                self.walk_expr(inner, frames);
            }
            Expr::Tuple(items, _) | Expr::VecLit(items, _) => {
                for item in items {
                    self.walk_expr(item, frames);
                }
            }
            Expr::Record(_, fields, span) => {
                for (field_name, value) in fields {
                    // Register field name as a reference to the record field symbol
                    if let Some(&symbol_id) = self.field_symbols.get(field_name) {
                        if let Some(name_span) = self.first_name_span(field_name, *span) {
                            self.index.push_occurrence(
                                symbol_id,
                                name_span,
                                OccurrenceRole::Reference,
                            );
                        }
                    }
                    self.walk_expr(value, frames);
                }
            }
            Expr::FieldAccess(base, field_name, span) => {
                self.walk_expr(base, frames);
                // Register field name as a reference to the record field symbol
                if let Some(&symbol_id) = self.field_symbols.get(field_name) {
                    // Find the field name token after the `.` within the expression span
                    if let Some(name_span) = self.last_name_span_before(field_name, *span, span.end)
                    {
                        self.index
                            .push_occurrence(symbol_id, name_span, OccurrenceRole::Reference);
                    }
                } else if let Some([symbol_id]) =
                    self.impl_method_symbols.get(field_name).map(Vec::as_slice)
                {
                    if let Some(name_span) = self.last_name_span_before(field_name, *span, span.end)
                    {
                        self.index.push_occurrence(
                            *symbol_id,
                            name_span,
                            OccurrenceRole::Reference,
                        );
                    }
                }
            }
            Expr::Index(base, index, _) => {
                self.walk_expr(base, frames);
                self.walk_expr(index, frames);
            }
            Expr::Do(statements, span) => {
                let container = frames.last().and_then(|frame| frame.container.clone());
                frames.push(ScopeFrame::new(*span, container));
                let depth = frames.len() - 1;
                for statement in statements {
                    match statement {
                        DoStmt::Bind(bind) | DoStmt::Let(bind) => {
                            self.walk_expr(&bind.expr, frames);
                            let symbol_id = self.index.push_symbol(NewSymbol {
                                name: bind.name.clone(),
                                namespace: Namespace::Value,
                                kind: SymbolKind::LocalBinding,
                                span: bind.name_span,
                                scope_span: *span,
                                scope_depth: depth,
                                visible_from: bind.span.start,
                                container: frames[depth].container.clone(),
                            });
                            frames[depth].value_defs.insert(bind.name.clone(), symbol_id);
                        }
                        DoStmt::Expr(value, _) => self.walk_expr(value, frames),
                    }
                }
                frames.pop();
            }
            Expr::Loop(loop_name, bindings, body, span) => {
                let container = frames.last().and_then(|frame| frame.container.clone());
                frames.push(ScopeFrame::new(*span, container));
                let depth = frames.len() - 1;
                // Register loop name as a local binding
                let loop_name_span = *span; // approximate
                let loop_sym = self.index.push_symbol(NewSymbol {
                    name: loop_name.clone(),
                    namespace: Namespace::Value,
                    kind: SymbolKind::LocalBinding,
                    span: loop_name_span,
                    scope_span: *span,
                    scope_depth: depth,
                    visible_from: span.start,
                    container: frames[depth].container.clone(),
                });
                frames[depth].value_defs.insert(loop_name.clone(), loop_sym);
                for bind in bindings {
                    self.walk_expr(&bind.expr, frames);
                    let symbol_id = self.index.push_symbol(NewSymbol {
                        name: bind.name.clone(),
                        namespace: Namespace::Value,
                        kind: SymbolKind::LocalBinding,
                        span: bind.name_span,
                        scope_span: *span,
                        scope_depth: depth,
                        visible_from: span.start,
                        container: frames[depth].container.clone(),
                    });
                    frames[depth]
                        .value_defs
                        .insert(bind.name.clone(), symbol_id);
                }
                self.walk_expr(body, frames);
                frames.pop();
            }
            Expr::RecordUpdate(base, fields, span) => {
                self.walk_expr(base, frames);
                for (field_name, value) in fields {
                    if let Some(&symbol_id) = self.field_symbols.get(field_name) {
                        if let Some(name_span) = self.first_name_span(field_name, *span) {
                            self.index.push_occurrence(
                                symbol_id,
                                name_span,
                                OccurrenceRole::Reference,
                            );
                        }
                    }
                    self.walk_expr(value, frames);
                }
            }
        }
    }

    fn walk_type(&mut self, ty: &Type, frames: &mut Vec<ScopeFrame>) {
        match ty {
            Type::Con(name, span) | Type::Var(name, span) => {
                if let Some(symbol_id) = self.resolve_type(name, frames) {
                    self.index
                        .push_occurrence(symbol_id, *span, OccurrenceRole::Reference);
                }
            }
            Type::App(left, right, _) | Type::Arrow(left, right, _) => {
                self.walk_type(left, frames);
                self.walk_type(right, frames);
            }
            Type::Paren(inner, _) => self.walk_type(inner, frames),
            Type::Tuple(items, _) => {
                for item in items {
                    self.walk_type(item, frames);
                }
            }
            Type::Proj(base, name, span) => {
                self.walk_type(base, frames);
                // Try to resolve the projected name as an associated type
                if let Some(symbol_id) = self.top_level_types.get(name).copied() {
                    let symbol = &self.index.symbols[symbol_id];
                    if symbol.kind == SymbolKind::AssociatedType {
                        // Compute the span of just the name token (after the dot)
                        if let Some(name_span) =
                            self.first_name_span(name, *span)
                        {
                            self.index
                                .push_occurrence(symbol_id, name_span, OccurrenceRole::Reference);
                        }
                    }
                }
            }
            Type::Nat(_, _) | Type::Unit(_) | Type::Self_(_) => {}
        }
    }

    fn define_pattern(
        &mut self,
        pattern: &Pat,
        visible_from: u32,
        frames: &mut Vec<ScopeFrame>,
        kind: SymbolKind,
    ) {
        match pattern {
            Pat::Wild(_) | Pat::Lit(_, _) => {}
            Pat::Var(name, span) => {
                let depth = frames.len() - 1;
                let symbol_id = self.index.push_symbol(NewSymbol {
                    name: name.clone(),
                    namespace: Namespace::Value,
                    kind,
                    span: *span,
                    scope_span: frames[depth].span,
                    scope_depth: depth,
                    visible_from,
                    container: frames[depth].container.clone(),
                });
                frames[depth].value_defs.insert(name.clone(), symbol_id);
            }
            Pat::Con(name, fields, span) => {
                if let Some(symbol_id) = self.resolve_value(name, frames) {
                    self.index
                        .push_occurrence(symbol_id, *span, OccurrenceRole::Reference);
                }
                for field in fields {
                    self.define_pattern(field, visible_from, frames, kind);
                }
            }
            Pat::Paren(inner, _) => self.define_pattern(inner, visible_from, frames, kind),
            Pat::Tuple(items, _) => {
                for item in items {
                    self.define_pattern(item, visible_from, frames, kind);
                }
            }
            Pat::Record(name, fields, _, span) => {
                if let Some(symbol_id) = self.resolve_value(name, frames) {
                    self.index
                        .push_occurrence(symbol_id, *span, OccurrenceRole::Reference);
                }
                for (field_name, field) in fields {
                    if let Some(field_pattern) = field {
                        self.define_pattern(field_pattern, visible_from, frames, kind);
                    } else if let Some(name_span) = self.first_name_span(field_name, *span) {
                        let depth = frames.len() - 1;
                        let symbol_id = self.index.push_symbol(NewSymbol {
                            name: field_name.clone(),
                            namespace: Namespace::Value,
                            kind,
                            span: name_span,
                            scope_span: frames[depth].span,
                            scope_depth: depth,
                            visible_from,
                            container: frames[depth].container.clone(),
                        });
                        frames[depth]
                            .value_defs
                            .insert(field_name.clone(), symbol_id);
                        if let Some(&field_symbol_id) = self.field_symbols.get(field_name) {
                            self.index.push_occurrence(
                                field_symbol_id,
                                name_span,
                                OccurrenceRole::Reference,
                            );
                        }
                    }
                }
            }
            Pat::As(name, inner, span) => {
                let depth = frames.len() - 1;
                let symbol_id = self.index.push_symbol(NewSymbol {
                    name: name.clone(),
                    namespace: Namespace::Value,
                    kind,
                    span: *span,
                    scope_span: frames[depth].span,
                    scope_depth: depth,
                    visible_from,
                    container: frames[depth].container.clone(),
                });
                frames[depth].value_defs.insert(name.clone(), symbol_id);
                self.define_pattern(inner, visible_from, frames, kind);
            }
            Pat::Or(alternatives, _) => {
                for alt in alternatives {
                    self.define_pattern(alt, visible_from, frames, kind);
                }
            }
        }
    }

    fn resolve_value(&self, name: &str, frames: &[ScopeFrame]) -> Option<usize> {
        for frame in frames.iter().rev() {
            if let Some(symbol_id) = frame.value_defs.get(name) {
                return Some(*symbol_id);
            }
        }
        self.top_level_values.get(name).copied()
    }

    fn resolve_type(&self, name: &str, frames: &[ScopeFrame]) -> Option<usize> {
        for frame in frames.iter().rev() {
            if let Some(symbol_id) = frame.type_defs.get(name) {
                return Some(*symbol_id);
            }
        }
        self.top_level_types.get(name).copied()
    }

    fn first_name_span(&self, name: &str, within: Span) -> Option<Span> {
        self.tokens
            .iter()
            .find(|token| {
                matches!(token.kind, SyntaxKind::Ident | SyntaxKind::UpperIdent)
                    && within.start <= token.span.start
                    && token.span.end <= within.end
                    && token.text(self.source) == name
            })
            .map(|token| token.span)
    }

    fn last_name_span_before(&self, name: &str, within: Span, before: u32) -> Option<Span> {
        self.tokens
            .iter()
            .rev()
            .find(|token| {
                matches!(token.kind, SyntaxKind::Ident | SyntaxKind::UpperIdent)
                    && within.start <= token.span.start
                    && token.span.end <= before
                    && token.text(self.source) == name
            })
            .map(|token| token.span)
    }
}

pub fn build_completions(source: &str, pos: Position) -> Vec<CompletionItem> {
    build_completions_with_prelude_flag(source, pos, false)
}

pub fn build_completions_with_prelude_flag(
    source: &str,
    pos: Position,
    is_compiler_prelude: bool,
) -> Vec<CompletionItem> {
    let prefix = completion_prefix(source, pos);
    let context = completion_context(source, pos, &prefix);
    let is_member_context = is_member_completion_context(source, pos, &prefix);
    let offset = position_to_offset(source, pos).unwrap_or(source.len()) as u32;
    let state = build_ide_state(source, is_compiler_prelude);

    let mut items = Vec::new();
    let mut seen = HashSet::new();

    for spec in all_completion_specs() {
        if !spec_matches_context(spec, context) {
            continue;
        }
        if !matches_prefix(spec.label, &prefix) {
            continue;
        }
        let token_kind = if context == CompletionContext::Type {
            SyntaxKind::UpperIdent
        } else {
            SyntaxKind::Ident
        };
        if context != CompletionContext::Attribute
            && prelude_symbol(&state, spec.label, token_kind).is_some()
        {
            continue;
        }
        seen.insert(spec.label.to_owned());
        items.push(completion_item_from_spec(spec));
    }

    if context != CompletionContext::Attribute {
        for (label, scheme) in state.analyzer.env.iter() {
            if state.analyzer.is_internal_impl_method_name(label)
                || seen.contains(label)
                || !matches_prefix(label, &prefix)
                || !is_word_completion(label)
            {
                continue;
            }
            items.push(generic_builtin_completion_item(
                label,
                scheme,
                &state.analyzer.engine,
                builtin_completion_kind(label),
            ));
            seen.insert(label.to_owned());
        }
    }

    for symbol in prelude_completion_symbols(&state, context)
        .filter(|symbol| matches_prefix(&symbol.name, &prefix))
    {
        if seen.contains(&symbol.name) {
            continue;
        }
        let kind = completion_kind_for_symbol(symbol);
        let documentation = prelude_symbol_markdown(&state, symbol)
            .unwrap_or_else(|| generic_builtin_markdown_text(&symbol.name, ""));
        let detail = prelude_symbol_detail(&state, symbol).unwrap_or_else(|| "builtin".to_owned());
        items.push(CompletionItem {
            label: symbol.name.clone(),
            kind: Some(kind),
            detail: Some(detail),
            documentation: Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: documentation,
            })),
            sort_text: Some(format!("03-prelude-{}", symbol.name)),
            filter_text: Some(symbol.name.clone()),
            ..Default::default()
        });
        seen.insert(symbol.name.clone());
    }

    if is_member_context {
        for impl_info in &state.analyzer.impls {
            for (method_name, mangled_name) in &impl_info.methods {
                if seen.contains(method_name) || !matches_prefix(method_name, &prefix) {
                    continue;
                }
                let detail = state
                    .analyzer
                    .env
                    .lookup(mangled_name)
                    .map(|scheme| {
                        format!("method : {}", format_scheme(&state.analyzer.engine, scheme))
                    })
                    .unwrap_or_else(|| "method".to_owned());
                items.push(CompletionItem {
                    label: method_name.clone(),
                    kind: Some(CompletionItemKind::METHOD),
                    detail: Some(detail),
                    sort_text: Some(format!("00-member-{}", method_name)),
                    filter_text: Some(method_name.clone()),
                    ..Default::default()
                });
                seen.insert(method_name.clone());
            }
        }
    }

    let mut visible_symbols = state
        .index
        .visible_symbols(offset, context)
        .filter(|symbol| matches_prefix(&symbol.name, &prefix))
        .collect::<Vec<_>>();

    visible_symbols.sort_by(|left, right| {
        right
            .scope_depth
            .cmp(&left.scope_depth)
            .then_with(|| right.visible_from.cmp(&left.visible_from))
            .then_with(|| left.name.cmp(&right.name))
    });

    let mut chosen = HashMap::<String, &Symbol>::new();
    for symbol in visible_symbols {
        chosen.entry(symbol.name.clone()).or_insert(symbol);
    }

    for symbol in chosen.into_values() {
        let kind = completion_kind_for_symbol(symbol);
        let documentation = symbol_markdown(&state, symbol);
        let detail = symbol_detail(&state, symbol);
        items.retain(|item| item.label != symbol.name);
        items.push(CompletionItem {
            label: symbol.name.clone(),
            kind: Some(kind),
            detail: Some(detail),
            documentation: Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: documentation,
            })),
            sort_text: Some(format!(
                "00-{}-{}",
                99usize.saturating_sub(symbol.scope_depth),
                symbol.name
            )),
            filter_text: Some(symbol.name.clone()),
            ..Default::default()
        });
    }

    items.sort_by(|left, right| {
        left.sort_text
            .as_deref()
            .cmp(&right.sort_text.as_deref())
            .then_with(|| left.label.cmp(&right.label))
    });
    items
}

pub fn build_hover(source: &str, pos: Position) -> Option<Hover> {
    build_hover_with_prelude_flag(source, pos, false)
}

pub fn build_hover_with_prelude_flag(
    source: &str,
    pos: Position,
    is_compiler_prelude: bool,
) -> Option<Hover> {
    let offset = position_to_offset(source, pos)? as u32;
    let tokens = lex(source);
    let (tok_index, tok) = tokens
        .iter()
        .enumerate()
        .find(|(_, token)| token.span.start <= offset && offset < token.span.end)?;
    let range = span_to_range(source, tok.span);
    let state = build_ide_state(source, is_compiler_prelude);

    match tok.kind {
        SyntaxKind::Ident | SyntaxKind::UpperIdent => {
            let name = tok.text(source);

            if previous_non_trivia_token(&tokens, tok_index)
                .is_some_and(|prev| prev.kind == SyntaxKind::At)
            {
                if let Some(spec) = lookup_completion_spec(name, CompletionContext::Attribute) {
                    return Some(spec_hover(&state, spec, range, Some(name)));
                }
            }

            if let Some(occurrence) = state.index.symbol_at_offset(offset) {
                let symbol = &state.index.symbols[occurrence.symbol_id];
                return Some(Hover {
                    contents: HoverContents::Markup(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: symbol_markdown(&state, symbol),
                    }),
                    range: Some(range),
                });
            }

            if let Some(symbol) = prelude_symbol(&state, name, tok.kind) {
                if let Some(markdown) = prelude_symbol_markdown(&state, symbol) {
                    return Some(Hover {
                        contents: HoverContents::Markup(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: markdown,
                        }),
                        range: Some(range),
                    });
                }
            }

            if let Some(spec) = lookup_non_attribute_spec(name) {
                return Some(spec_hover(&state, spec, range, Some(name)));
            }

            if let Some(markdown) = generic_builtin_markdown(&state, name) {
                return Some(Hover {
                    contents: HoverContents::Markup(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: markdown,
                    }),
                    range: Some(range),
                });
            }

            None
        }
        kind if kind.is_keyword() => {
            let keyword = tok.text(source);
            lookup_non_attribute_spec(keyword).map(|spec| spec_hover(&state, spec, range, None))
        }
        SyntaxKind::At => Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: "Use `@` to introduce a WGSL attribute such as `@compute`, `@vertex`, or `@workgroup_size(...)`.".to_owned(),
            }),
            range: Some(range),
        }),
        _ => None,
    }
}

pub fn build_goto_definition(
    uri: &Url,
    source: &str,
    pos: Position,
) -> Option<GotoDefinitionResponse> {
    build_goto_definition_with_prelude_flag(uri, source, pos, false)
}

pub fn build_goto_definition_with_prelude_flag(
    uri: &Url,
    source: &str,
    pos: Position,
    is_compiler_prelude: bool,
) -> Option<GotoDefinitionResponse> {
    let offset = position_to_offset(source, pos)? as u32;
    let tokens = lex(source);
    let state = build_ide_state(source, is_compiler_prelude);
    if let Some(occurrence) = state.index.symbol_at_offset(offset) {
        let symbol = &state.index.symbols[occurrence.symbol_id];
        if occurrence.role == OccurrenceRole::Definition && symbol.kind == SymbolKind::PatternBinding
        {
            if let Some(field_occurrence) = state.index.occurrences.iter().find(|candidate| {
                candidate.span == occurrence.span
                    && candidate.role == OccurrenceRole::Reference
                    && state.index.symbols[candidate.symbol_id].kind == SymbolKind::RecordField
            }) {
                let field_symbol = &state.index.symbols[field_occurrence.symbol_id];
                let locations = field_symbol
                    .definition_spans
                    .iter()
                    .copied()
                    .map(|span| Location {
                        uri: uri.clone(),
                        range: span_to_range(source, span),
                    })
                    .collect::<Vec<_>>();
                return match locations.as_slice() {
                    [] => None,
                    [single] => Some(GotoDefinitionResponse::Scalar(single.clone())),
                    _ => Some(GotoDefinitionResponse::Array(locations)),
                };
            }
        }
        let mut definition_spans = symbol.definition_spans.clone();
        definition_spans.sort_by(|left, right| {
            left.start
                .cmp(&right.start)
                .then_with(|| left.end.cmp(&right.end))
        });
        definition_spans.dedup();
        let locations = definition_spans
            .into_iter()
            .map(|span| Location {
                uri: uri.clone(),
                range: span_to_range(source, span),
            })
            .collect::<Vec<_>>();

        return match locations.as_slice() {
            [] => None,
            [single] => Some(GotoDefinitionResponse::Scalar(single.clone())),
            _ => Some(GotoDefinitionResponse::Array(locations)),
        };
    }

    let token = tokens.iter().find(|token| {
        matches!(token.kind, SyntaxKind::Ident | SyntaxKind::UpperIdent)
            && token.span.start <= offset
            && offset < token.span.end
    })?;
    let symbol = prelude_symbol(&state, token.text(source), token.kind)?;
    let prelude_source = shadml_parser::prelude_source();
    let prelude_uri = {
        #[cfg(not(target_arch = "wasm32"))]
        {
            Url::from_file_path(shadml_parser::prelude::prelude_path()).ok()?
        }
        #[cfg(target_arch = "wasm32")]
        {
            Url::parse("inmemory://shadml/prelude.shadml").ok()?
        }
    };
    let locations = symbol
        .definition_spans
        .iter()
        .copied()
        .map(|span| Location {
            uri: prelude_uri.clone(),
            range: span_to_range(prelude_source, span),
        })
        .collect::<Vec<_>>();
    match locations.as_slice() {
        [] => None,
        [single] => Some(GotoDefinitionResponse::Scalar(single.clone())),
        _ => Some(GotoDefinitionResponse::Array(locations)),
    }
}

pub fn build_references(
    uri: &Url,
    source: &str,
    pos: Position,
    include_declaration: bool,
) -> Option<Vec<Location>> {
    build_references_with_prelude_flag(uri, source, pos, include_declaration, false)
}

pub fn build_references_with_prelude_flag(
    uri: &Url,
    source: &str,
    pos: Position,
    include_declaration: bool,
    is_compiler_prelude: bool,
) -> Option<Vec<Location>> {
    let offset = position_to_offset(source, pos)? as u32;
    let state = build_ide_state(source, is_compiler_prelude);
    let occurrence = state.index.symbol_at_offset(offset)?;
    let mut locations = state
        .index
        .occurrences
        .iter()
        .filter(|candidate| candidate.symbol_id == occurrence.symbol_id)
        .filter(|candidate| include_declaration || candidate.role != OccurrenceRole::Definition)
        .map(|candidate| Location {
            uri: uri.clone(),
            range: span_to_range(source, candidate.span),
        })
        .collect::<Vec<_>>();

    locations.sort_by(|left, right| {
        left.range
            .start
            .line
            .cmp(&right.range.start.line)
            .then_with(|| left.range.start.character.cmp(&right.range.start.character))
    });
    locations.dedup_by(|left, right| left.range == right.range);

    if locations.is_empty() {
        None
    } else {
        Some(locations)
    }
}

fn build_ide_state(source: &str, is_compiler_prelude: bool) -> IdeState<'_> {
    let mut parser = Parser::new(source);
    let user_program = parser.parse_program();

    // Prepend prelude declarations for type environment (semantic analysis only)
    let full_program = if is_compiler_prelude || source == shadml_parser::prelude_source() {
        user_program.clone()
    } else {
        let prelude = shadml_parser::prelude_program();
        let mut combined = prelude.decls.clone();
        combined.extend(user_program.decls.iter().cloned());
        Program { decls: combined }
    };
    let document = build_document_state(source, &user_program, &full_program);
    let prelude = if is_compiler_prelude || source == shadml_parser::prelude_source() {
        None
    } else {
        let prelude_program = shadml_parser::prelude_program();
        Some(build_document_state(
            shadml_parser::prelude_source(),
            prelude_program,
            prelude_program,
        ))
    };

    IdeState {
        source: document.source,
        analyzer: document.analyzer,
        index: document.index,
        symbol_types: document.symbol_types,
        explicit_signatures: document.explicit_signatures,
        doc_comments: document.doc_comments,
        field_types: document.field_types,
        prelude,
    }
}

fn build_document_state<'a>(
    source: &'a str,
    user_program: &Program,
    full_program: &Program,
) -> DocumentState<'a> {
    let mut analyzer = SemanticAnalyzer::new();
    analyzer.analyze(full_program);

    DocumentState {
        source,
        index: IndexBuilder::new(source).build(user_program),
        symbol_types: collect_symbol_types(user_program, &analyzer),
        explicit_signatures: extract_explicit_signatures(user_program, source),
        doc_comments: extract_doc_comments(user_program),
        field_types: extract_field_types(user_program, source),
        analyzer,
    }
}

fn extract_explicit_signatures(program: &Program, source: &str) -> HashMap<String, String> {
    let mut signatures = HashMap::new();
    for decl in &program.decls {
        match decl {
            Decl::TypeSig {
                name,
                constraints,
                ty,
                ..
            } => {
                let rendered = if constraints.is_empty() {
                    ty.span().source_text(source).to_string()
                } else {
                    let constraints = constraints
                        .iter()
                        .map(|constraint| constraint.span.source_text(source))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{constraints} => {}", ty.span().source_text(source))
                };
                signatures.insert(name.clone(), rendered);
            }
            Decl::ConstDecl { name, ty, .. }
            | Decl::ExternDecl { name, ty, .. }
            | Decl::BuiltinExternDecl { name, ty, .. }
            | Decl::BindingDecl { name, ty, .. } => {
                signatures.insert(name.clone(), ty.span().source_text(source).to_string());
            }
            _ => {}
        }
    }
    signatures
}

/// Extract `-- |` doc comments from declaration comments and sub-items.
/// Returns a map from name to doc string.
fn extract_doc_comments(program: &Program) -> HashMap<String, String> {
    let mut docs = HashMap::new();

    for decl in &program.decls {
        // Extract doc from the leading comments of the declaration
        if let Some(doc) = extract_doc_from_comments(decl.comments()) {
            let name = match decl {
                Decl::TypeSig { name, .. }
                | Decl::FunDecl { name, .. }
                | Decl::DataDecl { name, .. }
                | Decl::EntryPoint { name, .. }
                | Decl::BuiltinTypeDecl { name, .. }
                | Decl::TypeAlias { name, .. }
                | Decl::BindingDecl { name, .. }
                | Decl::BitfieldDecl { name, .. }
                | Decl::ConstDecl { name, .. }
                | Decl::TraitDecl { name, .. }
                | Decl::ExternDecl { name, .. }
                | Decl::BuiltinExternDecl { name, .. }
                | Decl::ModuleDecl { name, .. } => name.clone(),
                Decl::ImplDecl { trait_name, .. } => trait_name.clone().unwrap_or_default(),
                Decl::BuiltinImplDecl { trait_name, .. } => trait_name.clone(),
                Decl::ImportDecl { module_path, .. } => module_path.clone(),
                Decl::CfgDecl { .. } => continue,
            };
            if !name.is_empty() {
                docs.entry(name).or_insert(doc);
            }
        }

        // Extract docs from sub-items
        match decl {
            Decl::DataDecl { constructors, .. } => {
                for con in constructors {
                    if let Some(doc) = &con.doc {
                        docs.entry(con.name.clone()).or_insert(doc.clone());
                    }
                    if let ConFields::Record(fields) = &con.fields {
                        for field in fields {
                            if let Some(doc) = &field.doc {
                                docs.entry(field.name.clone()).or_insert(doc.clone());
                            }
                        }
                    }
                }
            }
            Decl::BitfieldDecl { fields, .. } => {
                for field in fields {
                    if let Some(doc) = &field.doc {
                        docs.entry(field.name.clone()).or_insert(doc.clone());
                    }
                }
            }
            Decl::TraitDecl { methods, .. } => {
                for method in methods {
                    if let Some(doc) = &method.doc {
                        docs.entry(method.name.clone()).or_insert(doc.clone());
                    }
                }
            }
            Decl::BuiltinImplDecl { methods, .. } => {
                for method in methods {
                    if let Some(doc) = &method.doc {
                        docs.entry(method.name.clone()).or_insert(doc.clone());
                    }
                }
            }
            _ => {}
        }
    }

    docs
}

/// Extract record field type annotations from source text, keyed by field name.
fn extract_field_types(program: &Program, source: &str) -> HashMap<String, String> {
    let mut types = HashMap::new();
    for decl in &program.decls {
        if let Decl::DataDecl { constructors, .. } = decl {
            for con in constructors {
                if let ConFields::Record(fields) = &con.fields {
                    for field in fields {
                        let ty_str = field.ty.span().source_text(source).to_string();
                        types.insert(field.name.clone(), ty_str);
                    }
                }
            }
        }
        if let Decl::BitfieldDecl { fields, .. } = decl {
            for field in fields {
                let ty_str = match &field.kind {
                    shadml_parser::parser::BitfieldFieldKind::Bare(w) => format!("{} bits", w),
                    shadml_parser::parser::BitfieldFieldKind::Typed { ty, width } => {
                        format!("{} : {} bits", ty, width)
                    }
                    shadml_parser::parser::BitfieldFieldKind::Bool => "Bool".to_owned(),
                    shadml_parser::parser::BitfieldFieldKind::EnumInferred(ty) => ty.clone(),
                };
                types.insert(field.name.clone(), ty_str);
            }
        }
    }
    types
}

/// Extract doc text from a list of comment strings.
/// Doc comments start with ` | ` (after `--` prefix has been stripped).
fn extract_doc_from_comments(comments: &[String]) -> Option<String> {
    let doc_lines: Vec<&str> = comments
        .iter()
        .filter_map(|c| {
            if let Some(rest) = c.strip_prefix(" | ") {
                Some(rest.trim())
            } else if c.trim() == "|" {
                Some("")
            } else {
                None
            }
        })
        .collect();

    if doc_lines.is_empty() {
        None
    } else {
        Some(doc_lines.join("\n"))
    }
}

fn completion_prefix(source: &str, pos: Position) -> String {
    let offset = position_to_offset(source, pos).unwrap_or(source.len());
    let mut start = offset;

    while start > 0 {
        let Some(ch) = source[..start].chars().next_back() else {
            break;
        };
        if !is_completion_word_char(ch) {
            break;
        }
        start -= ch.len_utf8();
    }

    source[start..offset].to_owned()
}

fn completion_context(source: &str, pos: Position, prefix: &str) -> CompletionContext {
    let offset = position_to_offset(source, pos).unwrap_or(source.len());
    let before_cursor = &source[..offset];
    let before_prefix = &before_cursor[..before_cursor.len().saturating_sub(prefix.len())];

    if before_prefix
        .chars()
        .rev()
        .find(|ch| !ch.is_whitespace())
        .is_some_and(|ch| ch == '@')
    {
        return CompletionContext::Attribute;
    }

    if prefix.chars().next().is_some_and(char::is_uppercase) {
        return CompletionContext::Type;
    }

    let line_start = before_cursor.rfind('\n').map_or(0, |index| index + 1);
    let line = &before_cursor[line_start..];
    let last_colon = line.rfind(':');
    let last_equals = line.rfind('=');

    if last_colon.is_some() && last_equals.is_none_or(|equals| last_colon > Some(equals)) {
        return CompletionContext::Type;
    }

    CompletionContext::Value
}

fn is_member_completion_context(source: &str, pos: Position, prefix: &str) -> bool {
    let offset = position_to_offset(source, pos).unwrap_or(source.len());
    let before_cursor = &source[..offset];
    let before_prefix = &before_cursor[..before_cursor.len().saturating_sub(prefix.len())];
    before_prefix
        .chars()
        .rev()
        .find(|ch| !ch.is_whitespace())
        .is_some_and(|ch| ch == '.')
}

fn is_completion_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '\'' | '$')
}

fn matches_prefix(candidate: &str, prefix: &str) -> bool {
    prefix.is_empty() || candidate.starts_with(prefix)
}

fn is_word_completion(label: &str) -> bool {
    label
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '\'' | '$'))
}

fn completion_kind_for_symbol(symbol: &Symbol) -> CompletionItemKind {
    match symbol.kind {
        SymbolKind::Function | SymbolKind::EntryPoint => CompletionItemKind::FUNCTION,
        SymbolKind::Parameter | SymbolKind::LocalBinding | SymbolKind::PatternBinding => {
            CompletionItemKind::VARIABLE
        }
        SymbolKind::Constructor => CompletionItemKind::CONSTRUCTOR,
        SymbolKind::BuiltinType
        | SymbolKind::DataType
        | SymbolKind::TypeAlias
        | SymbolKind::AssociatedType
        | SymbolKind::TypeParameter => {
            CompletionItemKind::TYPE_PARAMETER
        }
        SymbolKind::RecordField => CompletionItemKind::FIELD,
    }
}

fn builtin_completion_kind(label: &str) -> CompletionItemKind {
    if label.chars().next().is_some_and(char::is_lowercase) {
        CompletionItemKind::FUNCTION
    } else {
        CompletionItemKind::VARIABLE
    }
}

fn generic_builtin_completion_item(
    label: &str,
    scheme: &Scheme,
    engine: &InferEngine,
    kind: CompletionItemKind,
) -> CompletionItem {
    let signature = format_scheme(engine, scheme);
    CompletionItem {
        label: label.to_owned(),
        kind: Some(kind),
        detail: Some(format!("builtin : {}", signature)),
        documentation: Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: generic_builtin_markdown_text(label, &signature),
        })),
        sort_text: Some(format!("04-{}", label)),
        filter_text: Some(label.to_owned()),
        ..Default::default()
    }
}

fn generic_builtin_markdown(state: &IdeState<'_>, label: &str) -> Option<String> {
    state.analyzer.env.lookup(label).map(|scheme| {
        generic_builtin_markdown_text(label, &format_scheme(&state.analyzer.engine, scheme))
    })
}

fn generic_builtin_markdown_text(label: &str, signature: &str) -> String {
    let mut sections = vec![format!("```shadml\n{} : {}\n```", label, signature)];
    sections.push("Builtin prelude symbol.".to_owned());
    sections.push("Available everywhere without an explicit import.".to_owned());
    sections.join("\n\n")
}

fn prelude_symbol<'a>(
    state: &'a IdeState<'_>,
    name: &str,
    token_kind: SyntaxKind,
) -> Option<&'a Symbol> {
    let prelude = state.prelude.as_ref()?;
    let matches = prelude
        .index
        .symbols
        .iter()
        .filter(|symbol| symbol.name == name)
        .collect::<Vec<_>>();
    if matches.is_empty() {
        return None;
    }
    if prelude.analyzer.constructors.contains_key(name) {
        return matches
            .iter()
            .copied()
            .find(|symbol| symbol.kind == SymbolKind::Constructor)
            .or_else(|| matches.first().copied());
    }
    if token_kind == SyntaxKind::UpperIdent {
        return matches
            .iter()
            .copied()
            .find(|symbol| symbol.namespace == Namespace::Type)
            .or_else(|| matches.first().copied());
    }
    matches.first().copied()
}

fn prelude_completion_symbols<'a>(
    state: &'a IdeState<'_>,
    context: CompletionContext,
) -> impl Iterator<Item = &'a Symbol> {
    state
        .prelude
        .iter()
        .flat_map(move |prelude| prelude.index.visible_symbols(0, context))
}

fn prelude_symbol_markdown(state: &IdeState<'_>, symbol: &Symbol) -> Option<String> {
    let prelude = state.prelude.as_ref()?;
    Some(document_symbol_markdown(prelude, symbol))
}

fn prelude_symbol_detail(state: &IdeState<'_>, symbol: &Symbol) -> Option<String> {
    let prelude = state.prelude.as_ref()?;
    Some(document_symbol_detail(prelude, symbol))
}

fn document_symbol_detail(state: &DocumentState<'_>, symbol: &Symbol) -> String {
    match symbol.kind {
        SymbolKind::Function => state
            .analyzer
            .env
            .lookup(&symbol.name)
            .map(|scheme| format!("binding : {}", format_scheme(&state.analyzer.engine, scheme)))
            .unwrap_or_else(|| "binding".to_owned()),
        SymbolKind::EntryPoint => state
            .analyzer
            .env
            .lookup(&symbol.name)
            .map(|scheme| {
                format!("entry point : {}", format_scheme(&state.analyzer.engine, scheme))
            })
            .unwrap_or_else(|| "entry point".to_owned()),
        SymbolKind::Parameter => state
            .symbol_types
            .get(&symbol.primary_span)
            .map(|ty| format!("parameter : {}", ty))
            .unwrap_or_else(|| "parameter".to_owned()),
        SymbolKind::LocalBinding | SymbolKind::PatternBinding => state
            .symbol_types
            .get(&symbol.primary_span)
            .map(|ty| format!("local : {}", ty))
            .unwrap_or_else(|| "local binding".to_owned()),
        SymbolKind::Constructor => state
            .analyzer
            .env
            .lookup(&symbol.name)
            .map(|scheme| {
                format!(
                    "constructor : {}",
                    format_scheme(&state.analyzer.engine, scheme)
                )
            })
            .unwrap_or_else(|| "constructor".to_owned()),
        SymbolKind::BuiltinType => match state
            .analyzer
            .builtin_types
            .get(&symbol.name)
            .copied()
            .unwrap_or(0)
        {
            0 => "builtin type".to_owned(),
            arity => format!("builtin type constructor (arity {})", arity),
        },
        SymbolKind::DataType => "data type".to_owned(),
        SymbolKind::TypeAlias => "type alias".to_owned(),
        SymbolKind::AssociatedType => "associated type".to_owned(),
        SymbolKind::TypeParameter => "type parameter".to_owned(),
        SymbolKind::RecordField => "record field".to_owned(),
    }
}

fn document_symbol_markdown(state: &DocumentState<'_>, symbol: &Symbol) -> String {
    let mut sections = Vec::new();

    if let Some(signature) = document_symbol_signature(state, symbol) {
        sections.push(format!("```shadml\n{} : {}\n```", symbol.name, signature));
    } else if let Some(excerpt) = document_declaration_excerpt(state, symbol) {
        sections.push(format!("```shadml\n{}\n```", excerpt));
    } else {
        sections.push(format!("**`{}`**", symbol.name));
    }

    if let Some(doc) = state.doc_comments.get(&symbol.name) {
        sections.push(doc.clone());
    } else {
        sections.push(document_symbol_summary(state, symbol));
    }

    let references = state
        .index
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.symbol_id == symbol.id)
        .count();
    sections.push(format!(
        "{} reference{} in this document.",
        references,
        if references == 1 { "" } else { "s" }
    ));

    sections.join("\n\n")
}

fn document_symbol_signature(state: &DocumentState<'_>, symbol: &Symbol) -> Option<String> {
    match symbol.kind {
        SymbolKind::BuiltinType
        | SymbolKind::DataType
        | SymbolKind::TypeAlias
        | SymbolKind::AssociatedType
        | SymbolKind::TypeParameter => None,
        SymbolKind::RecordField => state.field_types.get(&symbol.name).cloned(),
        SymbolKind::Parameter | SymbolKind::LocalBinding | SymbolKind::PatternBinding => {
            state.symbol_types.get(&symbol.primary_span).cloned()
        }
        _ => state
            .explicit_signatures
            .get(&symbol.name)
            .cloned()
            .or_else(|| {
                state
                    .analyzer
                    .env
                    .lookup(&symbol.name)
                    .map(|scheme| format_scheme(&state.analyzer.engine, scheme))
            }),
    }
}

fn document_symbol_summary(state: &DocumentState<'_>, symbol: &Symbol) -> String {
    match symbol.kind {
        SymbolKind::Function => "Top-level binding from this document.".to_owned(),
        SymbolKind::EntryPoint => "Shader entry point from this document.".to_owned(),
        SymbolKind::Parameter => match &symbol.container {
            Some(container) => format!("Parameter of `{}`.", container),
            None => "Function parameter.".to_owned(),
        },
        SymbolKind::LocalBinding => match &symbol.container {
            Some(container) => format!("Local binding inside `{}`.", container),
            None => "Local binding.".to_owned(),
        },
        SymbolKind::PatternBinding => match &symbol.container {
            Some(container) => format!("Pattern-bound name inside `{}`.", container),
            None => "Pattern-bound name.".to_owned(),
        },
        SymbolKind::BuiltinType => match state.analyzer.builtin_types.get(&symbol.name).copied() {
            Some(0) => "Builtin prelude type.".to_owned(),
            Some(arity) => format!("Builtin prelude type constructor with arity {}.", arity),
            None => "Builtin prelude type.".to_owned(),
        },
        SymbolKind::DataType => {
            if let Some(data_type) = state.analyzer.data_types.get(&symbol.name) {
                if data_type.constructors.is_empty() {
                    "User-defined data type.".to_owned()
                } else {
                    format!(
                        "User-defined data type with constructors: {}.",
                        data_type.constructors.join(", ")
                    )
                }
            } else {
                "User-defined data type.".to_owned()
            }
        }
        SymbolKind::TypeAlias => "Named type alias from this document.".to_owned(),
        SymbolKind::AssociatedType => match &symbol.container {
            Some(container) => format!("Associated type of trait `{}`.", container),
            None => "Associated type.".to_owned(),
        },
        SymbolKind::Constructor => state
            .analyzer
            .constructors
            .get(&symbol.name)
            .map(|constructor| format!("Constructor for `{}`.", constructor.type_name))
            .unwrap_or_else(|| "Data constructor.".to_owned()),
        SymbolKind::TypeParameter => match &symbol.container {
            Some(container) => format!("Type parameter scoped to `{}`.", container),
            None => "Type parameter.".to_owned(),
        },
        SymbolKind::RecordField => match &symbol.container {
            Some(container) => format!("Field of `{}`.", container),
            None => "Record field.".to_owned(),
        },
    }
}

fn document_declaration_excerpt(state: &DocumentState<'_>, symbol: &Symbol) -> Option<String> {
    let line_index = compute_line_starts(state.source)
        .binary_search(&symbol.primary_span.start)
        .unwrap_or_else(|index| index.saturating_sub(1));
    state
        .source
        .lines()
        .nth(line_index)
        .map(|line| line.trim().to_owned())
        .filter(|line| !line.is_empty())
}

fn symbol_detail(state: &IdeState<'_>, symbol: &Symbol) -> String {
    match symbol.kind {
        SymbolKind::Function => state
            .analyzer
            .env
            .lookup(&symbol.name)
            .map(|scheme| {
                format!(
                    "binding : {}",
                    format_scheme(&state.analyzer.engine, scheme)
                )
            })
            .unwrap_or_else(|| "binding".to_owned()),
        SymbolKind::EntryPoint => state
            .analyzer
            .env
            .lookup(&symbol.name)
            .map(|scheme| {
                format!(
                    "entry point : {}",
                    format_scheme(&state.analyzer.engine, scheme)
                )
            })
            .unwrap_or_else(|| "entry point".to_owned()),
        SymbolKind::Parameter => state
            .symbol_types
            .get(&symbol.primary_span)
            .map(|ty| format!("parameter : {}", ty))
            .unwrap_or_else(|| "parameter".to_owned()),
        SymbolKind::LocalBinding | SymbolKind::PatternBinding => state
            .symbol_types
            .get(&symbol.primary_span)
            .map(|ty| format!("local : {}", ty))
            .unwrap_or_else(|| "local binding".to_owned()),
        SymbolKind::Constructor => state
            .analyzer
            .env
            .lookup(&symbol.name)
            .map(|scheme| {
                format!(
                    "constructor : {}",
                    format_scheme(&state.analyzer.engine, scheme)
                )
            })
            .unwrap_or_else(|| "constructor".to_owned()),
        SymbolKind::BuiltinType => match state.analyzer.builtin_types.get(&symbol.name).copied() {
            Some(0) => "builtin type".to_owned(),
            Some(arity) => format!("builtin type constructor (arity {})", arity),
            None => "builtin type".to_owned(),
        },
        SymbolKind::DataType => "data type".to_owned(),
        SymbolKind::TypeAlias => "type alias".to_owned(),
        SymbolKind::AssociatedType => "associated type".to_owned(),
        SymbolKind::TypeParameter => "type parameter".to_owned(),
        SymbolKind::RecordField => "record field".to_owned(),
    }
}

fn symbol_markdown(state: &IdeState<'_>, symbol: &Symbol) -> String {
    let mut sections = Vec::new();

    if let Some(signature) = symbol_signature(state, symbol) {
        sections.push(format!("```shadml\n{} : {}\n```", symbol.name, signature));
    } else if let Some(excerpt) = declaration_excerpt(state, symbol) {
        sections.push(format!("```shadml\n{}\n```", excerpt));
    } else {
        sections.push(format!("**`{}`**", symbol.name));
    }

    if let Some(doc) = state.doc_comments.get(&symbol.name) {
        sections.push(doc.clone());
    } else {
        sections.push(symbol_summary(state, symbol));
    }

    let references = state
        .index
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.symbol_id == symbol.id)
        .count();
    sections.push(format!(
        "{} reference{} in this document.",
        references,
        if references == 1 { "" } else { "s" }
    ));

    sections.join("\n\n")
}

fn symbol_signature(state: &IdeState<'_>, symbol: &Symbol) -> Option<String> {
    match symbol.kind {
        SymbolKind::BuiltinType
        | SymbolKind::DataType
        | SymbolKind::TypeAlias
        | SymbolKind::AssociatedType
        | SymbolKind::TypeParameter => None,
        SymbolKind::RecordField => state.field_types.get(&symbol.name).cloned(),
        SymbolKind::Parameter | SymbolKind::LocalBinding | SymbolKind::PatternBinding => {
            state.symbol_types.get(&symbol.primary_span).cloned()
        }
        _ => state
            .explicit_signatures
            .get(&symbol.name)
            .cloned()
            .or_else(|| {
                state
                    .analyzer
                    .env
                    .lookup(&symbol.name)
                    .map(|scheme| format_scheme(&state.analyzer.engine, scheme))
            }),
    }
}

fn symbol_summary(state: &IdeState<'_>, symbol: &Symbol) -> String {
    match symbol.kind {
        SymbolKind::Function => "Top-level binding from this document.".to_owned(),
        SymbolKind::EntryPoint => "Shader entry point from this document.".to_owned(),
        SymbolKind::Parameter => match &symbol.container {
            Some(container) => format!("Parameter of `{}`.", container),
            None => "Function parameter.".to_owned(),
        },
        SymbolKind::LocalBinding => match &symbol.container {
            Some(container) => format!("Local binding inside `{}`.", container),
            None => "Local binding.".to_owned(),
        },
        SymbolKind::PatternBinding => match &symbol.container {
            Some(container) => format!("Pattern-bound name inside `{}`.", container),
            None => "Pattern-bound name.".to_owned(),
        },
        SymbolKind::BuiltinType => match state.analyzer.builtin_types.get(&symbol.name).copied() {
            Some(0) => "Builtin prelude type.".to_owned(),
            Some(arity) => format!("Builtin prelude type constructor with arity {}.", arity),
            None => "Builtin prelude type.".to_owned(),
        },
        SymbolKind::DataType => {
            if let Some(data_type) = state.analyzer.data_types.get(&symbol.name) {
                if data_type.constructors.is_empty() {
                    "User-defined data type.".to_owned()
                } else {
                    format!(
                        "User-defined data type with constructors: {}.",
                        data_type.constructors.join(", ")
                    )
                }
            } else {
                "User-defined data type.".to_owned()
            }
        }
        SymbolKind::TypeAlias => "Named type alias from this document.".to_owned(),
        SymbolKind::AssociatedType => match &symbol.container {
            Some(container) => format!("Associated type of trait `{}`.", container),
            None => "Associated type.".to_owned(),
        },
        SymbolKind::Constructor => state
            .analyzer
            .constructors
            .get(&symbol.name)
            .map(|constructor| format!("Constructor for `{}`.", constructor.type_name))
            .unwrap_or_else(|| "Data constructor.".to_owned()),
        SymbolKind::TypeParameter => match &symbol.container {
            Some(container) => format!("Type parameter scoped to `{}`.", container),
            None => "Type parameter.".to_owned(),
        },
        SymbolKind::RecordField => match &symbol.container {
            Some(container) => format!("Field of `{}`.", container),
            None => "Record field.".to_owned(),
        },
    }
}

fn declaration_excerpt(state: &IdeState<'_>, symbol: &Symbol) -> Option<String> {
    let line_index = compute_line_starts(state.source)
        .binary_search(&symbol.primary_span.start)
        .unwrap_or_else(|index| index.saturating_sub(1));
    state
        .source
        .lines()
        .nth(line_index)
        .map(|line| line.trim().to_owned())
        .filter(|line| !line.is_empty())
}

fn previous_non_trivia_token(tokens: &[Token], index: usize) -> Option<&Token> {
    tokens[..index]
        .iter()
        .rev()
        .find(|token| !token.kind.is_trivia())
}

fn lookup_non_attribute_spec(label: &str) -> Option<&'static CompletionSpec> {
    lookup_completion_spec(label, CompletionContext::Value)
        .or_else(|| lookup_completion_spec(label, CompletionContext::Type))
}

fn spec_hover(
    state: &IdeState<'_>,
    spec: &CompletionSpec,
    range: Range,
    symbol_name: Option<&str>,
) -> Hover {
    let signature = symbol_name
        .and_then(|name| state.analyzer.env.lookup(name))
        .map(|scheme| format_scheme(&state.analyzer.engine, scheme));
    let mut sections = Vec::new();
    if let Some(signature) = signature {
        sections.push(format!("```shadml\n{} : {}\n```", spec.label, signature));
    }
    sections.push(format!("**`{}`**", spec.label));
    sections.push(format!("_{}_", spec.detail));
    sections.push(spec.documentation.to_owned());

    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: sections.join("\n\n"),
        }),
        range: Some(range),
    }
}

fn format_scheme(engine: &InferEngine, scheme: &Scheme) -> String {
    format_scheme_surface(scheme, Some(&engine.subst))
}

fn format_type(ty: &Type) -> String {
    match ty {
        Type::Con(name, _) | Type::Var(name, _) => name.clone(),
        Type::Nat(n, _) => n.to_string(),
        Type::App(f, a, _) => format!("{} {}", format_type(f), format_type(a)),
        Type::Arrow(a, b, _) => format!("{} -> {}", format_type(a), format_type(b)),
        Type::Paren(inner, _) => format!("({})", format_type(inner)),
        Type::Tuple(items, _) => {
            let rendered = items.iter().map(format_type).collect::<Vec<_>>().join(", ");
            format!("({})", rendered)
        }
        Type::Unit(_) => "()".to_owned(),
        Type::Proj(base, name, _) => format!("{}.{}", format_type(base), name),
        Type::Self_(_) => "Self".to_owned(),
    }
}

fn format_ty(engine: &InferEngine, ty: &shadml_typechecker::Ty) -> String {
    format_ty_surface_inferred(ty, Some(&engine.subst))
}

fn collect_symbol_types(
    program: &Program,
    analyzer: &SemanticAnalyzer,
) -> HashMap<Span, String> {
    let mut types = analyzer
        .local_binding_schemes
        .iter()
        .map(|(span, scheme)| (*span, format_scheme(&analyzer.engine, scheme)))
        .collect::<HashMap<_, _>>();
    let all_decls = Decl::flatten_cfg_decls(&program.decls);
    let mut impl_infos = analyzer.impls.iter();
    for decl in &all_decls {
        match decl {
            Decl::FunDecl {
                name,
                params,
                span,
                ..
            } => {
                if let Some(scheme) = analyzer.env.lookup(name) {
                    let mut cursor = analyzer.engine.finalize(&scheme.ty);
                    for pat in params {
                        if let shadml_typechecker::Ty::Arrow(from, to) = cursor {
                            collect_pattern_types(pat, from.as_ref(), analyzer, &mut types);
                            cursor = (*to).clone();
                        } else {
                            let _ = span;
                            break;
                        }
                    }
                }
            }
            Decl::EntryPoint {
                name,
                params,
                span,
                ..
            } => {
                if let Some(scheme) = analyzer.env.lookup(name) {
                    let mut cursor = analyzer.engine.finalize(&scheme.ty);
                    for pat in params {
                        if let shadml_typechecker::Ty::Arrow(from, to) = cursor {
                            collect_pattern_types(pat, from.as_ref(), analyzer, &mut types);
                            cursor = (*to).clone();
                        } else {
                            let _ = span;
                            break;
                        }
                    }
                }
            }
            Decl::ImplDecl { methods, .. } => {
                let Some(impl_info) = impl_infos.next() else {
                    continue;
                };
                for method in methods {
                    let Some(mangled) = impl_info.methods.get(&method.name) else {
                        continue;
                    };
                    if let Some(scheme) = analyzer.env.lookup(&mangled) {
                        let mut cursor = analyzer.engine.finalize(&scheme.ty);
                        for pat in &method.params {
                            if let shadml_typechecker::Ty::Arrow(from, to) = cursor {
                                collect_pattern_types(pat, from.as_ref(), analyzer, &mut types);
                                cursor = (*to).clone();
                            } else {
                                break;
                            }
                        }
                    }
                }
            }
            Decl::BuiltinImplDecl { .. } => {}
            _ => {}
        }
    }
    types
}

fn collect_pattern_types(
    pattern: &Pat,
    ty: &shadml_typechecker::Ty,
    analyzer: &SemanticAnalyzer,
    types: &mut HashMap<Span, String>,
) {
    match pattern {
        Pat::Var(_, span) => {
            types.insert(*span, format_ty(&analyzer.engine, ty));
        }
        Pat::Paren(inner, _) => collect_pattern_types(inner, ty, analyzer, types),
        Pat::Tuple(items, _) => {
            if let shadml_typechecker::Ty::Tuple(elem_tys) = analyzer.engine.finalize(ty) {
                for (item, elem_ty) in items.iter().zip(elem_tys.iter()) {
                    collect_pattern_types(item, elem_ty, analyzer, types);
                }
            }
        }
        Pat::Record(con_name, fields, _, _) => {
            if let Some(con_info) = analyzer.constructors.get(con_name) {
                if let shadml_typechecker::ConstructorFields::Record(con_fields) = &con_info.fields
                {
                    for (field_name, maybe_pat) in fields {
                        if let Some((_, field_ty)) =
                            con_fields.iter().find(|(n, _)| n == field_name)
                        {
                            if let Some(pat) = maybe_pat {
                                collect_pattern_types(pat, field_ty, analyzer, types);
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

pub fn compute_line_starts(source: &str) -> Vec<u32> {
    let mut starts = vec![0u32];
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' {
            starts.push((i + 1) as u32);
        }
    }
    starts
}

pub fn offset_to_line_col(line_starts: &[u32], offset: u32) -> (u32, u32) {
    let line = match line_starts.binary_search(&offset) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    };
    let col = offset - line_starts[line];
    (line as u32, col)
}

pub fn position_to_offset(source: &str, pos: Position) -> Option<usize> {
    let line_starts = compute_line_starts(source);
    let line = pos.line as usize;
    if line >= line_starts.len() {
        return None;
    }
    let line_start = line_starts[line] as usize;
    let offset = line_start + pos.character as usize;
    if offset <= source.len() {
        Some(offset)
    } else {
        None
    }
}

pub fn span_to_range(source: &str, span: Span) -> Range {
    let line_starts = compute_line_starts(source);
    let (start_line, start_col) = offset_to_line_col(&line_starts, span.start);
    let (end_line, end_col) = offset_to_line_col(&line_starts, span.end);
    Range::new(
        Position::new(start_line, start_col),
        Position::new(end_line, end_col),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nth_position(source: &str, needle: &str, nth: usize) -> Position {
        let offset = source
            .match_indices(needle)
            .nth(nth)
            .map(|(offset, _)| offset)
            .unwrap_or_else(|| panic!("missing occurrence {nth} of `{needle}`"));
        let line_starts = compute_line_starts(source);
        let (line, col) = offset_to_line_col(&line_starts, offset as u32);
        Position::new(line, col)
    }

    fn hover_markdown(source: &str, pos: Position) -> String {
        let hover = build_hover(source, pos).unwrap();
        match hover.contents {
            HoverContents::Markup(markup) => markup.value,
            other => panic!("expected markup hover, got {other:?}"),
        }
    }

    #[test]
    fn completions_include_generic_prefixed_builtin_docs() {
        let items = build_completions("le", Position::new(0, 2));
        let item = items.iter().find(|item| item.label == "length").unwrap();
        assert!(item
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("builtin"));
        match &item.documentation {
            Some(Documentation::MarkupContent(markup)) => {
                assert!(markup.value.contains("length"));
                assert!(markup.value.contains("Builtin prelude symbol"));
            }
            other => panic!("expected builtin docs, got {other:?}"),
        }
    }

    #[test]
    fn member_completions_prefer_source_impl_method_name() {
        let source = r#"
impl F32 where
  half : F32 -> F32
  half x = x

value = 1.0.ha
"#;
        let items = build_completions(source, Position::new(5, 14));
        assert!(items.iter().any(|item| item.label == "half"));
        assert!(!items.iter().any(|item| item.label == "half_F32"));
    }

    #[test]
    fn hover_on_prefixed_builtin_includes_signature() {
        let source = "main x = length x";
        let hover = build_hover(source, Position::new(0, 10)).unwrap();
        match hover.contents {
            HoverContents::Markup(markup) => {
                assert!(markup.value.contains("length"));
                assert!(markup.value.contains("```shadml"));
                assert!(markup.value.contains("length :"));
            }
            other => panic!("expected markup hover, got {other:?}"),
        }
    }

    #[test]
    fn hover_on_prelude_function_uses_haddock_doc_from_prelude() {
        let source = "main x = normalize x";
        let markup = hover_markdown(source, Position::new(0, 9));
        assert!(markup.contains("normalize"));
        assert!(markup.contains("Scale a vector to unit length."));
    }

    #[test]
    fn hover_on_prelude_builtin_type_uses_haddock_doc_from_prelude() {
        let source = "value : F32\nvalue = 1.0";
        let markup = hover_markdown(source, Position::new(0, 8));
        assert!(markup.contains("builtin type F32"));
        assert!(markup.contains("32-bit floating point scalar."));
    }

    #[test]
    fn goto_definition_returns_signature_and_implementation() {
        let source = "double : I32 -> I32\ndouble x = x * 2\nmain = double 2";
        let uri = Url::parse("file:///test.shadml").unwrap();
        let response = build_goto_definition(&uri, source, Position::new(2, 9)).unwrap();
        match response {
            GotoDefinitionResponse::Array(locations) => {
                assert_eq!(locations.len(), 2);
                assert_eq!(locations[0].range.start.line, 0);
                assert_eq!(locations[1].range.start.line, 1);
            }
            other => panic!("expected multiple locations, got {other:?}"),
        }
    }

    #[test]
    fn goto_definition_for_prelude_function_jumps_to_prelude_decl() {
        let source = "main x = normalize x";
        let uri = Url::parse("file:///test.shadml").unwrap();
        let response = build_goto_definition(&uri, source, Position::new(0, 9)).unwrap();
        let location = match response {
            GotoDefinitionResponse::Scalar(location) => location,
            other => panic!("expected scalar location, got {other:?}"),
        };
        assert!(location.uri.path().ends_with("/prelude/prelude.shadml"));
        let prelude_source = shadml_parser::prelude_source();
        let start = position_to_offset(prelude_source, location.range.start).unwrap();
        assert!(prelude_source[start..].starts_with("normalize"));
    }

    #[test]
    fn goto_definition_for_member_impl_method() {
        let source = r#"impl F32 where
  half : F32 -> F32
  half x = x

value = 1.0.half"#;
        let uri = Url::parse("file:///test.shadml").unwrap();
        let response = build_goto_definition(&uri, source, Position::new(4, 13)).unwrap();
        match response {
            GotoDefinitionResponse::Array(locations) => {
                assert!(locations.iter().any(|loc| loc.range.start.line == 1));
                assert!(locations.iter().any(|loc| loc.range.start.line == 2));
            }
            GotoDefinitionResponse::Scalar(loc) => {
                assert!(loc.range.start.line == 1 || loc.range.start.line == 2);
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[test]
    fn goto_definition_for_record_pattern_binding_then_field() {
        let source = r#"data ParticleState
  = Active { position : F32, life : F32 }
  | Dead

lifeValue particle =
  match particle
    | Active { life, .. } -> life
    | Dead -> 0.0"#;
        let uri = Url::parse("file:///test.shadml").unwrap();

        let body_response = build_goto_definition(&uri, source, Position::new(6, 29)).unwrap();
        let body_target = match body_response {
            GotoDefinitionResponse::Scalar(loc) => loc,
            GotoDefinitionResponse::Array(mut locs) => {
                locs.sort_by_key(|loc| (loc.range.start.line, loc.range.start.character));
                locs[0].clone()
            }
            other => panic!("unexpected response: {other:?}"),
        };
        assert_eq!(body_target.range.start.line, 6);

        let pattern_response = build_goto_definition(&uri, source, Position::new(6, 15)).unwrap();
        let pattern_target = match pattern_response {
            GotoDefinitionResponse::Scalar(loc) => loc,
            GotoDefinitionResponse::Array(mut locs) => {
                locs.sort_by_key(|loc| (loc.range.start.line, loc.range.start.character));
                locs[0].clone()
            }
            other => panic!("unexpected response: {other:?}"),
        };
        assert_eq!(pattern_target.range.start.line, 1);
    }

    #[test]
    fn hover_on_impl_parameter_includes_type() {
        let source = r#"trait Light a where
  illumination : a -> Vec<3, F32> -> Vec<3, F32> -> Vec<3, F32>

data SpotLight = SpotLight

impl Light SpotLight where
  illumination light worldPos normal =
    let lambert = dot normal worldPos
    in lambert"#;
        let hover = build_hover(source, Position::new(7, 24)).unwrap();
        match hover.contents {
            HoverContents::Markup(markup) => {
                assert!(markup.value.contains("normal"));
                assert!(markup.value.contains("Vec"));
                assert!(markup.value.contains("F32"));
            }
            other => panic!("expected markup hover, got {other:?}"),
        }
    }

    #[test]
    fn hover_on_slang_generics_worldpos_uses_inferred_surface_type() {
        let source = include_str!("../../../examples/slang-generics.shadml");
        let markup = hover_markdown(source, nth_position(source, "worldPos", 5));
        assert!(markup.contains("worldPos : Vec<3, F32>"), "{markup}");
    }

    #[test]
    fn hover_on_slang_generics_lambert_shows_inferred_scalar_type() {
        let source = include_str!("../../../examples/slang-generics.shadml");
        let markup = hover_markdown(source, nth_position(source, "lambert", 4));
        assert!(markup.contains("lambert : F32"), "{markup}");
    }

    #[test]
    fn hover_on_slang_generics_atten_shows_finalized_scalar_type() {
        let source = include_str!("../../../examples/slang-generics.shadml");
        let markup = hover_markdown(source, nth_position(source, "atten", 3));
        assert!(markup.contains("atten : F32"), "{markup}");
    }

    #[test]
    fn hover_on_slang_generics_light_is_canonical_type_variable() {
        let source = include_str!("../../../examples/slang-generics.shadml");
        let anchor = source.match_indices("color light").last().unwrap().0 + "color ".len();
        let line_starts = compute_line_starts(source);
        let (line, col) = offset_to_line_col(&line_starts, anchor as u32);
        let markup = hover_markdown(source, Position::new(line, col));
        assert!(markup.contains("light : a"), "{markup}");
        assert!(!markup.contains("t138"), "{markup}");
    }

    #[test]
    fn hover_on_slang_generics_light_direction_field_keeps_closing_bracket() {
        let source = include_str!("../../../examples/slang-generics.shadml");
        let markup = hover_markdown(source, nth_position(source, "lightDirection", 0));
        assert!(markup.contains("lightDirection : Vec<3, F32>"), "{markup}");
    }

    #[test]
    fn explicit_signature_preserves_angle_style_surface_text() {
        let source = r#"
lighting : Light a => a -> Vec<3, F32> -> Vec<3, F32> -> Vec<3, F32>
lighting light worldPos normal = vec3 1.0 1.0 1.0
"#;
        let markup = hover_markdown(source, nth_position(source, "lighting", 1));
        assert!(
            markup.contains("lighting : Light a => a -> Vec<3, F32> -> Vec<3, F32> -> Vec<3, F32>"),
            "{markup}"
        );
    }

    #[test]
    fn explicit_signature_preserves_space_application_style_surface_text() {
        let source = r#"
lighting : Light a => a -> Vec 3 F32 -> Vec 3 F32 -> Vec 3 F32
lighting light worldPos normal = vec3 1.0 1.0 1.0
"#;
        let markup = hover_markdown(source, nth_position(source, "lighting", 1));
        assert!(
            markup.contains("lighting : Light a => a -> Vec 3 F32 -> Vec 3 F32 -> Vec 3 F32"),
            "{markup}"
        );
    }

    #[test]
    fn goto_definition_for_impl_parameter_reference() {
        let source = r#"trait Light a where
  illumination : a -> Vec<3, F32> -> Vec<3, F32> -> Vec<3, F32>

data SpotLight = SpotLight

impl Light SpotLight where
  illumination light worldPos normal =
    let lambert = dot normal worldPos
    in lambert"#;
        let uri = Url::parse("file:///test.shadml").unwrap();
        let response = build_goto_definition(&uri, source, Position::new(7, 24)).unwrap();
        match response {
            GotoDefinitionResponse::Scalar(loc) => {
                assert_eq!(loc.range.start.line, 6);
            }
            GotoDefinitionResponse::Array(locs) => {
                assert!(locs.iter().any(|loc| loc.range.start.line == 6));
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[test]
    fn references_include_definition_and_uses() {
        let source = "double : I32 -> I32\ndouble x = x * 2\nmain = double (double 2)";
        let uri = Url::parse("file:///test.shadml").unwrap();
        let references = build_references(&uri, source, Position::new(2, 9), true).unwrap();
        assert_eq!(references.len(), 4);
        assert_eq!(references[0].range.start.line, 0);
        assert_eq!(references[1].range.start.line, 1);
        assert_eq!(references[2].range.start.line, 2);
        assert_eq!(references[3].range.start.line, 2);
    }

    #[test]
    fn goto_definition_for_function_ref_in_let_body() {
        let source = r#"data Particle = Particle {
  x  : F32,
  y  : F32,
  vx : F32,
  vy : F32,
}

applyVelocity : F32 -> Particle -> Particle
applyVelocity dt p = p { x = p.x + p.vx * dt, y = p.y + p.vy * dt }

nudgeX : F32 -> Particle -> Particle
nudgeX dx p = p { x = p.x + dx }

step : F32 -> F32 -> Particle -> Particle
step dt dx p =
  let p2 = applyVelocity dt p
  in nudgeX dx p2"#;
        let uri = Url::parse("file:///test.shadml").unwrap();
        // line 15 (0-indexed): "  let p2 = applyVelocity dt p"
        let result = build_goto_definition(&uri, source, Position::new(15, 12));
        assert!(
            result.is_some(),
            "goto-def for applyVelocity should return a result"
        );
        match result.unwrap() {
            GotoDefinitionResponse::Array(locs) => {
                assert!(
                    locs.iter().any(|l| l.range.start.line == 7),
                    "should include type sig line"
                );
                assert!(
                    locs.iter().any(|l| l.range.start.line == 8),
                    "should include fun decl line"
                );
            }
            GotoDefinitionResponse::Scalar(loc) => {
                assert!(
                    loc.range.start.line == 7 || loc.range.start.line == 8,
                    "expected line 7 or 8, got line {}",
                    loc.range.start.line
                );
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[test]
    fn goto_definition_for_constructor_in_match() {
        let source = "data Color = Red | Green | Blue\n\ndescribe : Color -> I32\ndescribe c =\n  match c\n    | Red -> 1\n    | Green -> 2\n    | Blue -> 3";
        let uri = Url::parse("file:///test.shadml").unwrap();
        // line 5 (0-indexed): "    | Red -> 1", cursor on "Red" at col 6
        let result = build_goto_definition(&uri, source, Position::new(5, 6));
        assert!(
            result.is_some(),
            "goto-def for Red constructor in match should return a result"
        );
        match result.unwrap() {
            GotoDefinitionResponse::Scalar(loc) => {
                assert_eq!(
                    loc.range.start.line, 0,
                    "Red should point to data decl line"
                );
            }
            GotoDefinitionResponse::Array(locs) => {
                assert!(
                    locs.iter().any(|l| l.range.start.line == 0),
                    "should include data decl line"
                );
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[test]
    fn hover_shows_doc_comment() {
        let source = "-- | Adds two numbers together.\nadd : I32 -> I32 -> I32\nadd x y = x + y";
        // Hover on "add" in the function body (line 2, col 0)
        let hover = build_hover(source, Position::new(2, 0));
        assert!(hover.is_some(), "hover should return a result");
        if let Some(Hover {
            contents: HoverContents::Markup(markup),
            ..
        }) = hover
        {
            assert!(
                markup.value.contains("Adds two numbers together."),
                "hover should contain doc comment, got: {}",
                markup.value
            );
        } else {
            panic!("expected Markup hover contents");
        }
    }

    #[test]
    fn hover_on_record_field_shows_type_and_doc() {
        let source = "data Point = Point {\n  -- | X coordinate.\n  x : F32,\n  y : F32, -- ^ Y coordinate.\n}";
        // Hover on "x" field declaration (line 2, col 2)
        let hover = build_hover(source, Position::new(2, 2));
        assert!(
            hover.is_some(),
            "hover should return a result for record field"
        );
        if let Some(Hover {
            contents: HoverContents::Markup(markup),
            ..
        }) = hover
        {
            assert!(
                markup.value.contains("F32"),
                "hover should show field type, got: {}",
                markup.value
            );
            assert!(
                markup.value.contains("X coordinate."),
                "hover should show field doc comment, got: {}",
                markup.value
            );
        } else {
            panic!("expected Markup hover contents");
        }
        // Hover on "y" field with trailing doc (line 3, col 2)
        let hover_y = build_hover(source, Position::new(3, 2));
        assert!(
            hover_y.is_some(),
            "hover should return a result for field y"
        );
        if let Some(Hover {
            contents: HoverContents::Markup(markup),
            ..
        }) = hover_y
        {
            assert!(
                markup.value.contains("Y coordinate."),
                "hover should show trailing doc comment for y, got: {}",
                markup.value
            );
        } else {
            panic!("expected Markup hover contents");
        }
    }

    #[test]
    fn hover_on_field_access_shows_field_info() {
        let source =
            "data Point = Point {\n  -- | X coordinate.\n  x : F32,\n  y : F32,\n}\ngetX p = p.x";
        // Hover on "x" in "p.x" (line 5, col 11)
        let hover = build_hover(source, Position::new(5, 11));
        assert!(
            hover.is_some(),
            "hover should return a result for field access"
        );
        if let Some(Hover {
            contents: HoverContents::Markup(markup),
            ..
        }) = hover
        {
            assert!(
                markup.value.contains("X coordinate."),
                "hover on .x should show field doc, got: {}",
                markup.value
            );
            assert!(
                markup.value.contains("F32"),
                "hover on .x should show field type, got: {}",
                markup.value
            );
        } else {
            panic!("expected Markup hover contents");
        }
    }
}
