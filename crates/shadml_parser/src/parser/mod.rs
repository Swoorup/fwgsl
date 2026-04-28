//! Recursive descent parser for shadml.
//!
//! Produces a simple AST. Expression parsing uses Pratt (precedence climbing).

use std::collections::HashMap;

use shadml_diagnostics::{Diagnostic, DiagnosticSink, Label};
use shadml_span::Span;
use shadml_syntax::SyntaxKind;

use crate::layout::resolve_layout;
use crate::lexer::{is_negative_literal_start, lex, Token};

// ═══════════════════════════════════════════════════════════════════════════
// AST types
// ═══════════════════════════════════════════════════════════════════════════

/// A parsed shadml program.
#[derive(Debug, Clone)]
pub struct Program {
    pub decls: Vec<Decl>,
}

impl Decl {
    /// Get the leading comments attached to this declaration.
    pub fn comments(&self) -> &[String] {
        match self {
            Decl::TypeSig { comments, .. }
            | Decl::FunDecl { comments, .. }
            | Decl::DataDecl { comments, .. }
            | Decl::EntryPoint { comments, .. }
            | Decl::BuiltinTypeDecl { comments, .. }
            | Decl::TypeAlias { comments, .. }
            | Decl::BindingDecl { comments, .. }
            | Decl::BitfieldDecl { comments, .. }
            | Decl::ConstDecl { comments, .. }
            | Decl::TraitDecl { comments, .. }
            | Decl::ImplDecl { comments, .. }
            | Decl::BuiltinExternDecl { comments, .. }
            | Decl::BuiltinImplDecl { comments, .. }
            | Decl::ExternDecl { comments, .. }
            | Decl::ModuleDecl { comments, .. }
            | Decl::ImportDecl { comments, .. }
            | Decl::RenderBlock { comments, .. } => comments,
            Decl::CfgDecl { .. } => &[],
        }
    }

    /// Get a mutable reference to the leading comments.
    pub fn comments_mut(&mut self) -> &mut Vec<String> {
        match self {
            Decl::TypeSig { comments, .. }
            | Decl::FunDecl { comments, .. }
            | Decl::DataDecl { comments, .. }
            | Decl::EntryPoint { comments, .. }
            | Decl::BuiltinTypeDecl { comments, .. }
            | Decl::TypeAlias { comments, .. }
            | Decl::BindingDecl { comments, .. }
            | Decl::BitfieldDecl { comments, .. }
            | Decl::ConstDecl { comments, .. }
            | Decl::TraitDecl { comments, .. }
            | Decl::ImplDecl { comments, .. }
            | Decl::BuiltinExternDecl { comments, .. }
            | Decl::BuiltinImplDecl { comments, .. }
            | Decl::ExternDecl { comments, .. }
            | Decl::ModuleDecl { comments, .. }
            | Decl::ImportDecl { comments, .. }
            | Decl::RenderBlock { comments, .. } => comments,
            Decl::CfgDecl { .. } => {
                // CfgDecl nodes are always expanded by `cfg_eval::evaluate_features`
                // before downstream processing (parsing, lowering, codegen). No phase
                // after cfg-evaluation should ever encounter a bare CfgDecl, so this
                // branch is genuinely unreachable in practice.
                unreachable!("CfgDecl has no comments — it should be expanded before use")
            }
        }
    }

    /// Recursively collect all declarations from a slice, flattening `CfgDecl`
    /// nodes by including declarations from *both* branches. This is used by
    /// the semantic analyzer, AST lowering, and IDE so they see all names
    /// regardless of which feature flags are active.
    pub fn flatten_cfg_decls(decls: &[Decl]) -> Vec<&Decl> {
        let mut result = Vec::new();
        for decl in decls {
            match decl {
                Decl::CfgDecl {
                    then_decls,
                    else_decls,
                    ..
                } => {
                    result.extend(Decl::flatten_cfg_decls(then_decls));
                    result.extend(Decl::flatten_cfg_decls(else_decls));
                }
                _ => result.push(decl),
            }
        }
        result
    }

    /// Get the source span of this declaration.
    pub fn span(&self) -> Span {
        match self {
            Decl::TypeSig { span, .. }
            | Decl::FunDecl { span, .. }
            | Decl::DataDecl { span, .. }
            | Decl::EntryPoint { span, .. }
            | Decl::BuiltinTypeDecl { span, .. }
            | Decl::TypeAlias { span, .. }
            | Decl::BindingDecl { span, .. }
            | Decl::BitfieldDecl { span, .. }
            | Decl::ConstDecl { span, .. }
            | Decl::TraitDecl { span, .. }
            | Decl::ImplDecl { span, .. }
            | Decl::BuiltinExternDecl { span, .. }
            | Decl::BuiltinImplDecl { span, .. }
            | Decl::ExternDecl { span, .. }
            | Decl::ModuleDecl { span, .. }
            | Decl::ImportDecl { span, .. }
            | Decl::CfgDecl { span, .. }
            | Decl::RenderBlock { span, .. } => *span,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Decl {
    TypeSig {
        name: String,
        constraints: Vec<TypeConstraint>,
        ty: Type,
        span: Span,
        comments: Vec<String>,
    },
    FunDecl {
        name: String,
        params: Vec<Pat>,
        body: Expr,
        where_binds: Vec<LocalBind>,
        span: Span,
        comments: Vec<String>,
        attributes: Vec<Attribute>,
    },
    DataDecl {
        name: String,
        type_params: Vec<String>,
        constructors: Vec<ConDecl>,
        span: Span,
        comments: Vec<String>,
    },
    EntryPoint {
        attributes: Vec<Attribute>,
        name: String,
        params: Vec<Pat>,
        body: Expr,
        span: Span,
        comments: Vec<String>,
    },
    BuiltinTypeDecl {
        name: String,
        arity: usize,
        span: Span,
        comments: Vec<String>,
    },
    TypeAlias {
        name: String,
        params: Vec<String>,
        ty: Type,
        span: Span,
        comments: Vec<String>,
    },
    /// Binding declaration:
    ///   `@group(N) @binding(N) uniform name : Type`
    ///   `@group(N) @binding(N) storage name : Type`
    ///   `@group(N) @binding(N) storage(read) name : Type`
    BindingDecl {
        name: String,
        ty: Type,
        address_space: BindingAddressSpace,
        group: u32,
        binding: u32,
        span: Span,
        comments: Vec<String>,
        attributes: Vec<Attribute>,
    },
    BitfieldDecl {
        name: String,
        base_ty: Type,
        constructor_name: String,
        fields: Vec<BitfieldField>,
        span: Span,
        comments: Vec<String>,
    },
    ConstDecl {
        name: String,
        ty: Type,
        value: Expr,
        span: Span,
        comments: Vec<String>,
    },
    /// Trait declaration: `trait Num a where (+) : a -> a -> a ...`
    TraitDecl {
        name: String,
        /// Type variables the trait is parameterised over.
        vars: Vec<String>,
        associated_types: Vec<AssociatedTypeDecl>,
        methods: Vec<TraitMethod>,
        span: Span,
        comments: Vec<String>,
    },
    /// Impl declaration.
    /// Trait impl: `impl Add Fp64 where (+) a b = ...`
    /// Standalone impl: `impl Fp64 where collapse v = ...`
    ImplDecl {
        /// None for standalone impls.
        trait_name: Option<String>,
        /// The concrete types this impl is for. Standalone impls contain one type.
        tys: Vec<Type>,
        associated_types: Vec<AssociatedTypeDef>,
        methods: Vec<ImplMethod>,
        span: Span,
        comments: Vec<String>,
    },
    /// Extern declaration: `extern sin : a -> a`
    /// Declares a built-in name with its type signature (no body).
    ExternDecl {
        name: String,
        ty: Type,
        span: Span,
        comments: Vec<String>,
    },
    /// Builtin extern declaration: `builtin extern sin : F32 -> F32 = intrinsic(sin)`.
    BuiltinExternDecl {
        name: String,
        ty: Type,
        lowering: BuiltinLowering,
        span: Span,
        comments: Vec<String>,
    },
    /// Builtin trait impl declaration.
    BuiltinImplDecl {
        trait_name: String,
        tys: Vec<Type>,
        associated_types: Vec<AssociatedTypeDef>,
        methods: Vec<BuiltinImplMethod>,
        span: Span,
        comments: Vec<String>,
    },
    /// Module header (optional): `module Math.Fp64`
    /// If absent, the module name is derived from the file path.
    ModuleDecl {
        name: String,
        span: Span,
        comments: Vec<String>,
    },
    /// Import declaration:
    ///   `import Math.Fp64`            — import all public names
    ///   `import Math.Fp64 (Fp64, f)`  — import specific names
    ///   `import Math.Fp64 as Fp`      — qualified access: Fp.f
    ///   `import Math.*`               — import all sub-modules
    ///   `import Debug when cfg.debug` — conditional import
    ImportDecl {
        module_path: String,
        kind: ImportKind,
        condition: Option<CfgPredicate>,
        span: Span,
        comments: Vec<String>,
    },
    /// Conditional compilation block:
    ///   `when cfg.debug`           — block form
    ///     decl1
    ///     decl2
    ///   `else`
    ///     decl3
    CfgDecl {
        condition: CfgPredicate,
        then_decls: Vec<Decl>,
        else_decls: Vec<Decl>,
        span: Span,
    },
    /// Render block: `render name { bindings; entry_points }`
    /// Explicitly scopes bindings to a vertex+fragment pipeline pair.
    RenderBlock {
        name: String,
        /// Binding declarations inside the render block.
        bindings: Vec<Decl>,
        /// Entry point declarations inside the render block (with @vertex/@fragment).
        entries: Vec<Decl>,
        span: Span,
        comments: Vec<String>,
    },
}

/// A compile-time feature predicate for conditional compilation.
#[derive(Debug, Clone)]
pub enum CfgPredicate {
    /// `cfg.name` — true if `--feature name` is set.
    Feature(String),
    /// `not pred` — negation.
    Not(Box<CfgPredicate>),
    /// `pred && pred` — conjunction.
    And(Box<CfgPredicate>, Box<CfgPredicate>),
    /// `pred || pred` — disjunction.
    Or(Box<CfgPredicate>, Box<CfgPredicate>),
}

/// How names are imported from a module.
#[derive(Debug, Clone)]
pub enum ImportKind {
    /// `import Foo` — import all public names unqualified.
    All,
    /// `import Foo (bar, baz)` — import only listed names.
    Selective(Vec<String>),
    /// `import Foo as F` — qualified access only.
    Qualified(String),
    /// `import Foo.*` — import all sub-modules under Foo/.
    Wildcard,
}

/// Address space for a binding declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingAddressSpace {
    /// `uniform` — uniform buffer (read-only)
    Uniform,
    /// `storage` — storage buffer (default: read)
    StorageRead,
    /// `storage(read_write)` — storage buffer (read-write)
    StorageReadWrite,
    /// `immediate` — push constants, no @group/@binding
    Immediate,
    /// Opaque resource (texture/sampler) — no address space keyword
    Opaque,
}

/// An associated type declaration inside a `trait` body: `type Output`
#[derive(Debug, Clone)]
pub struct AssociatedTypeDecl {
    pub name: String,
    pub span: Span,
}

/// An associated type definition inside an `impl` or `builtin impl` body: `type Output = F32`
#[derive(Debug, Clone)]
pub struct AssociatedTypeDef {
    pub name: String,
    pub ty: Type,
    pub span: Span,
}

/// A method signature inside a `trait` declaration.
#[derive(Debug, Clone)]
pub struct TraitMethod {
    pub name: String,
    pub ty: Type,
    pub span: Span,
    /// Doc comments attached to this trait method.
    pub doc: Option<String>,
}

/// A method implementation inside an `impl` declaration.
#[derive(Debug, Clone)]
pub struct ImplMethod {
    pub name: String,
    pub ty: Option<Type>,
    pub params: Vec<Pat>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct LocalBind {
    pub name: String,
    pub name_span: Span,
    pub expr: Expr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct BuiltinImplMethod {
    pub name: String,
    pub lowering: BuiltinLowering,
    pub span: Span,
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub enum BuiltinLowering {
    Intrinsic(String),
    NativeBinOp(String),
    NativeUnary(String),
}

/// The type and bit-width specification for a bitfield field.
///
/// Supported forms:
/// - `name : Type : N` — typed field with explicit bit width
/// - `name : Bool`     — Bool is always 1 bit (width implicit)
/// - `name : EnumType` — width inferred from enum variant count
/// - `name : N`        — bare integer width, accessor returns base type (U32)
#[derive(Debug, Clone)]
pub enum BitfieldFieldKind {
    /// Bare integer width (e.g. `selected : 1`). Accessor returns the base type.
    Bare(u32),
    /// Typed field with explicit width (e.g. `capStart : CapStyle : 2` or `roughness : U32 : 5`).
    Typed { ty: String, width: u32 },
    /// Bool field — always 1 bit (e.g. `visible : Bool`).
    Bool,
    /// Enum-typed field with width inferred from variant count (e.g. `capStart : CapStyle`).
    EnumInferred(String),
}

#[derive(Debug, Clone)]
pub struct BitfieldField {
    pub name: String,
    pub kind: BitfieldFieldKind,
    pub span: Span,
    /// Doc comments attached to this bitfield field.
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ConDecl {
    pub name: String,
    pub fields: ConFields,
    /// Optional explicit discriminant value (e.g. `NoCap = 0`)
    pub discriminant: Option<i64>,
    pub span: Span,
    /// Doc comments (`-- |`) attached to this constructor.
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ConFields {
    Positional(Vec<Type>),
    Record(Vec<RecordField>),
    Empty,
}

/// A field in a record type declaration, optionally with attributes.
#[derive(Debug, Clone)]
pub struct RecordField {
    pub name: String,
    pub ty: Type,
    pub attributes: Vec<Attribute>,
    /// Doc comments (`-- |` before or `-- ^` after the field).
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Attribute {
    pub name: String,
    pub args: Vec<AttrArg>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum AttrArg {
    Positional(AttrValue),
    Named(String, AttrValue),
}

#[derive(Debug, Clone)]
pub enum AttrValue {
    Ident(String),
    String(String),
    Int(i64),
    UInt(u64),
    Float(f64),
}

impl AttrValue {
    /// Format as a canonical string for HIR/MIR compatibility.
    /// Strings are wrapped in double quotes; everything else is bare.
    pub fn to_canonical_string(&self) -> String {
        match self {
            AttrValue::Ident(s) => s.clone(),
            AttrValue::String(s) => format!("\"{}\"", s),
            AttrValue::Int(v) => v.to_string(),
            AttrValue::UInt(v) => v.to_string(),
            AttrValue::Float(v) => v.to_string(),
        }
    }
}

impl AttrArg {
    /// Format as a canonical string for HIR/MIR compatibility.
    /// Named arguments use `name = value` syntax.
    pub fn to_canonical_string(&self) -> String {
        match self {
            AttrArg::Positional(v) => v.to_canonical_string(),
            AttrArg::Named(name, v) => format!("{} = {}", name, v.to_canonical_string()),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Expr {
    Lit(Lit, Span),
    Var(String, Span),
    Con(String, Span),
    App(Box<Expr>, Box<Expr>, Span),
    Infix(Box<Expr>, String, Box<Expr>, Span),
    Lambda(Vec<Pat>, Box<Expr>, Span),
    Let(Vec<LocalBind>, Box<Expr>, Span),
    /// Match expression. Arms are `(pattern, optional_guard, body)`.
    Case(Box<Expr>, Vec<(Pat, Option<Expr>, Expr)>, Span),
    If(Box<Expr>, Box<Expr>, Box<Expr>, Span),
    Paren(Box<Expr>, Span),
    Tuple(Vec<Expr>, Span),
    /// Record expression. `Some(name)` for named construction: `Name { f = e, ... }`.
    /// `None` for anonymous: `{ f = e, ... }`.
    Record(Option<String>, Vec<(String, Expr)>, Span),
    FieldAccess(Box<Expr>, String, Span),
    Index(Box<Expr>, Box<Expr>, Span),
    OpSection(String, Span),
    Neg(Box<Expr>, Span),
    Not(Box<Expr>, Span),
    BitNot(Box<Expr>, Span),
    Do(Vec<DoStmt>, Span),
    /// Vec literal: `[a, b, c]` — desugared to vecN constructor call.
    VecLit(Vec<Expr>, Span),
    /// Named loop (tail-recursive): `loop go (i = 0) (acc = 0) in body`
    /// Fields: (loop_name, bindings[(name, init)], body, span)
    Loop(String, Vec<LocalBind>, Box<Expr>, Span),
    /// Record/bitfield functional update: `expr { field = val, ... }`
    RecordUpdate(Box<Expr>, Vec<(String, Expr)>, Span),
}

#[derive(Debug, Clone)]
pub enum DoStmt {
    Bind(LocalBind),
    Expr(Expr, Span),
    Let(LocalBind),
}

#[derive(Debug, Clone)]
pub enum Lit {
    Int(i64),
    UInt(u64),
    Float(f64),
    String(String),
    Char(char),
}

#[derive(Debug, Clone)]
pub enum Pat {
    Wild(Span),
    Var(String, Span),
    Con(String, Vec<Pat>, Span),
    Lit(Lit, Span),
    Paren(Box<Pat>, Span),
    Tuple(Vec<Pat>, Span),
    Record(String, Vec<(String, Option<Pat>)>, bool, Span),
    As(String, Box<Pat>, Span),
    /// Or-pattern: `p1 | p2 | p3` — used for multi-value switch cases.
    Or(Vec<Pat>, Span),
}

#[derive(Debug, Clone)]
pub enum Type {
    Con(String, Span),
    Var(String, Span),
    Nat(u64, Span),
    App(Box<Type>, Box<Type>, Span),
    Arrow(Box<Type>, Box<Type>, Span),
    Paren(Box<Type>, Span),
    Tuple(Vec<Type>, Span),
    Unit(Span),
    /// Type projection: `a.Output` (associated type access)
    Proj(Box<Type>, String, Span),
    /// `Self` keyword in type position (only valid as `Self.Output`)
    Self_(Span),
}

#[derive(Debug, Clone)]
pub struct TypeConstraint {
    pub trait_name: String,
    pub tys: Vec<Type>,
    pub span: Span,
}

// ═══════════════════════════════════════════════════════════════════════════
// Span accessors for AST nodes
// ═══════════════════════════════════════════════════════════════════════════

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Lit(_, s)
            | Expr::Var(_, s)
            | Expr::Con(_, s)
            | Expr::App(_, _, s)
            | Expr::Infix(_, _, _, s)
            | Expr::Lambda(_, _, s)
            | Expr::Let(_, _, s)
            | Expr::Case(_, _, s)
            | Expr::If(_, _, _, s)
            | Expr::Paren(_, s)
            | Expr::Tuple(_, s)
            | Expr::Record(_, _, s)
            | Expr::FieldAccess(_, _, s)
            | Expr::Index(_, _, s)
            | Expr::OpSection(_, s)
            | Expr::Neg(_, s)
            | Expr::Not(_, s)
            | Expr::BitNot(_, s)
            | Expr::Do(_, s)
            | Expr::VecLit(_, s)
            | Expr::Loop(_, _, _, s)
            | Expr::RecordUpdate(_, _, s) => *s,
        }
    }
}

impl Type {
    pub fn span(&self) -> Span {
        match self {
            Type::Con(_, s)
            | Type::Var(_, s)
            | Type::Nat(_, s)
            | Type::App(_, _, s)
            | Type::Arrow(_, _, s)
            | Type::Paren(_, s)
            | Type::Tuple(_, s)
            | Type::Unit(s)
            | Type::Proj(_, _, s)
            | Type::Self_(s) => *s,
        }
    }
}

impl Pat {
    pub fn span(&self) -> Span {
        match self {
            Pat::Wild(s)
            | Pat::Var(_, s)
            | Pat::Con(_, _, s)
            | Pat::Lit(_, s)
            | Pat::Paren(_, s)
            | Pat::Tuple(_, s)
            | Pat::Record(_, _, _, s)
            | Pat::As(_, _, s)
            | Pat::Or(_, s) => *s,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Parser
// ═══════════════════════════════════════════════════════════════════════════

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    fuel: u32,
    diagnostics: DiagnosticSink,
    source: String,
    /// Buffer for extra declarations produced by group block expansion.
    pending_decls: Vec<Decl>,
}

const MAX_FUEL: u32 = 10_000;

/// Context in which a declaration is being parsed.
pub(crate) enum DeclContext {
    /// Top-level module scope — all declaration kinds are allowed.
    ModuleScope,
    /// Inside a `render` block — module-scoped constructs (imports,
    /// nested render blocks, traits, etc.) are disallowed.
    RenderBlock,
}

impl Parser {
    /// Create a new parser from source text. Lexes and resolves layout.
    pub fn new(source: &str) -> Self {
        Self::with_builtin_decls(source, true)
    }

    pub fn with_builtin_decls(source: &str, _allow_builtin_decls: bool) -> Self {
        let raw_tokens = lex(source);
        let tokens = resolve_layout(raw_tokens, source);
        Self {
            tokens,
            pos: 0,
            fuel: MAX_FUEL,
            diagnostics: DiagnosticSink::new(),
            source: source.to_owned(),
            pending_decls: Vec::new(),
        }
    }

    /// Return the diagnostics accumulated during parsing.
    pub fn diagnostics(&self) -> &DiagnosticSink {
        &self.diagnostics
    }

    // -- Navigation helpers ------------------------------------------------

    fn current_token(&self) -> &Token {
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    fn peek(&self) -> SyntaxKind {
        self.current_token().kind
    }

    fn at(&self, kind: SyntaxKind) -> bool {
        self.peek() == kind
    }

    fn at_end(&self) -> bool {
        self.peek() == SyntaxKind::Eof
    }

    fn bump(&mut self) -> Token {
        let tok = self.current_token().clone();
        if !self.at_end() {
            self.pos += 1;
        }
        tok
    }

    fn expect(&mut self, kind: SyntaxKind) -> Token {
        if self.at(kind) {
            self.bump()
        } else {
            let tok = self.current_token().clone();
            self.diagnostics.push(
                Diagnostic::error(format!("expected {}, found {}", kind, tok.kind))
                    .with_label(Label::primary(tok.span, format!("expected {}", kind)))
                    .with_help(
                        "check for missing tokens, unmatched parentheses, or incorrect indentation",
                    ),
            );
            tok
        }
    }

    fn eat(&mut self, kind: SyntaxKind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn skip_trivia(&mut self) {
        while self.peek().is_trivia() {
            self.bump();
        }
    }

    /// Peek at the next non-trivia token kind without consuming.
    fn peek_non_trivia(&self) -> SyntaxKind {
        let mut i = self.pos;
        loop {
            if i >= self.tokens.len() {
                return SyntaxKind::Eof;
            }
            let kind = self.tokens[i].kind;
            if !kind.is_trivia() {
                return kind;
            }
            i += 1;
        }
    }

    /// Peek at the first non-trivia, non-layout token *after* the current token.
    fn peek_after_current(&self) -> SyntaxKind {
        let mut i = self.pos + 1;
        loop {
            if i >= self.tokens.len() {
                return SyntaxKind::Eof;
            }
            let kind = self.tokens[i].kind;
            if !kind.is_trivia()
                && kind != SyntaxKind::LayoutSemicolon
                && kind != SyntaxKind::LayoutBraceOpen
                && kind != SyntaxKind::LayoutBraceClose
            {
                return kind;
            }
            i += 1;
        }
    }

    /// Whether the next non-trivia postfix token is directly attached to `base`
    /// with no intervening whitespace. This lets `f [1, 2]` parse as
    /// application while preserving `arr[i]` indexing.
    fn postfix_is_adjacent(&self, base: Span) -> bool {
        let mut i = self.pos;
        while i < self.tokens.len() && self.tokens[i].kind.is_trivia() {
            i += 1;
        }
        i < self.tokens.len() && span_touches(base, self.tokens[i].span)
    }

    /// Lookahead: check if the token stream has `{ ident = ...` starting at
    /// the current position (for record/bitfield update syntax).
    fn is_record_update_ahead(&self) -> bool {
        let mut i = self.pos;
        // Skip trivia to find `{`
        while i < self.tokens.len() && self.tokens[i].kind.is_trivia() {
            i += 1;
        }
        if i >= self.tokens.len() || self.tokens[i].kind != SyntaxKind::LBrace {
            return false;
        }
        i += 1;
        // Skip trivia to find `ident`
        while i < self.tokens.len() && self.tokens[i].kind.is_trivia() {
            i += 1;
        }
        if i >= self.tokens.len() || self.tokens[i].kind != SyntaxKind::Ident {
            return false;
        }
        i += 1;
        // Skip trivia to find `=`
        while i < self.tokens.len() && self.tokens[i].kind.is_trivia() {
            i += 1;
        }
        i < self.tokens.len() && self.tokens[i].kind == SyntaxKind::Equals
    }

    /// Check whether the current token is `Minus` immediately (no whitespace)
    /// followed by a numeric literal, forming a negative literal like `-0.35`.
    /// Used in the application loop to treat `-0.35` as an atom argument.
    ///
    /// The heuristic: `-` must be glued to the digit (`-0.35`, not `- 0.35`)
    /// AND there must be whitespace before the `-` (so `x-0.5` stays as subtraction).
    fn is_negative_literal_ahead(&self) -> bool {
        is_negative_literal_start(&self.tokens, self.pos)
    }

    /// Compute the 0-based column for a byte offset in the source.
    fn column_of(&self, offset: u32) -> u32 {
        let off = offset as usize;
        // Find the start of the line containing this offset
        let line_start = self.source[..off].rfind('\n').map_or(0, |i| i + 1);
        (off - line_start) as u32
    }

    fn span_from(&self, start: u32) -> Span {
        let end = if self.pos > 0 {
            self.tokens[self.pos - 1].span.end
        } else {
            start
        };
        Span::new(start, end)
    }

    fn current_span(&self) -> Span {
        self.current_token().span
    }

    fn text_of(&self, tok: &Token) -> &str {
        tok.span.source_text(&self.source)
    }

    fn consume_fuel(&mut self) -> bool {
        if self.fuel == 0 {
            return false;
        }
        self.fuel -= 1;
        true
    }

    // -- Layout helpers ----------------------------------------------------

    fn eat_layout_semi(&mut self) -> bool {
        self.skip_trivia();
        self.eat(SyntaxKind::LayoutSemicolon)
    }

    fn at_layout_end(&self) -> bool {
        let k = self.peek_non_trivia();
        matches!(k, SyntaxKind::LayoutBraceClose | SyntaxKind::Eof)
    }

    fn eat_layout_close(&mut self) -> bool {
        self.skip_trivia();
        self.eat(SyntaxKind::LayoutBraceClose)
    }

    // ═════════════════════════════════════════════════════════════════════
    // Top-level: Program
    // ═════════════════════════════════════════════════════════════════════

    pub fn parse_program(&mut self) -> Program {
        let mut decls = Vec::new();
        loop {
            // Drain any buffered decls from group block expansion first,
            // before checking for EOF or consuming layout tokens.
            if !self.pending_decls.is_empty() {
                decls.push(self.pending_decls.remove(0));
                continue;
            }

            self.skip_trivia();
            // Also consume layout tokens between decls at top level
            while self.eat(SyntaxKind::LayoutSemicolon)
                || self.eat(SyntaxKind::LayoutBraceClose)
                || self.eat(SyntaxKind::LayoutBraceOpen)
            {
                self.skip_trivia();
            }

            if self.at_end() {
                break;
            }
            if !self.consume_fuel() {
                break;
            }
            if let Some(decl) = self.parse_decl() {
                decls.push(decl);
            } else {
                // Error recovery: skip one token
                self.bump();
            }
        }

        // Post-pass: attach leading comments to each declaration by scanning
        // backwards through the token stream from each decl's span start.
        self.attach_leading_comments(&mut decls);

        Program { decls }
    }

    /// For each declaration, scan backwards in the token stream from its span
    /// start to collect immediately preceding comment tokens.
    fn attach_leading_comments(&self, decls: &mut [Decl]) {
        for decl in decls.iter_mut() {
            // CfgDecl has no comments field — skip it
            if matches!(decl, Decl::CfgDecl { .. }) {
                continue;
            }
            let decl_start = decl.span().start;
            // Find the first non-layout token at or after the decl start.
            // Layout (virtual) tokens can share the same span.start as the
            // declaration token and appear *before* comments in the stream,
            // which would cause the backward scan to miss preceding comments.
            let tok_idx = match self.tokens.iter().position(|t| {
                t.span.start >= decl_start
                    && !matches!(
                        t.kind,
                        SyntaxKind::LayoutBraceOpen
                            | SyntaxKind::LayoutSemicolon
                            | SyntaxKind::LayoutBraceClose
                    )
            }) {
                Some(i) => i,
                None => continue,
            };
            // Walk backwards, collecting comments, skipping whitespace/newlines/layout
            let mut comments = Vec::new();
            let mut i = tok_idx;
            while i > 0 {
                i -= 1;
                let kind = self.tokens[i].kind;
                if kind == SyntaxKind::LineComment
                    || kind == SyntaxKind::BlockComment
                    || kind == SyntaxKind::DocComment
                {
                    let text = self.text_of(&self.tokens[i]);
                    let comment = if let Some(stripped) = text.strip_prefix("--") {
                        stripped.to_string()
                    } else if text.starts_with("{-") && text.ends_with("-}") {
                        text[2..text.len() - 2].to_string()
                    } else {
                        text.to_string()
                    };
                    comments.push(comment);
                } else if kind == SyntaxKind::Whitespace
                    || kind == SyntaxKind::Newline
                    || kind == SyntaxKind::LayoutSemicolon
                    || kind == SyntaxKind::LayoutBraceOpen
                    || kind == SyntaxKind::LayoutBraceClose
                {
                    continue;
                } else {
                    // Hit a real token belonging to the previous decl — stop
                    break;
                }
            }
            comments.reverse();
            *decl.comments_mut() = comments;
        }

        // Second pass: attach doc comments to sub-items inside data/bitfield/trait decls.
        self.attach_sub_item_docs(decls);
    }

    /// Extract the doc string from a `-- |` or `-- ^` comment text.
    /// Returns `Some(text)` with the prefix stripped, or `None` if not a doc comment.
    fn extract_doc_text(comment: &str) -> Option<String> {
        // Comment text has already had `--` prefix stripped, so we look for ` | ` or ` ^ `.
        if let Some(rest) = comment.strip_prefix(" | ") {
            Some(rest.trim().to_string())
        } else if comment == " |" {
            // bare `-- |` with no trailing text
            Some(String::new())
        } else {
            None
        }
    }

    /// Scan the token stream to find doc comments (`-- |` forward, `-- ^` backward)
    /// adjacent to sub-items (constructors, record fields, bitfield fields, trait methods).
    fn attach_sub_item_docs(&self, decls: &mut [Decl]) {
        for decl in decls.iter_mut() {
            match decl {
                Decl::DataDecl { constructors, .. } => {
                    for con in constructors.iter_mut() {
                        // Attach forward doc comment to each constructor
                        con.doc = self.find_leading_doc(con.span.start);
                        // Attach doc comments to record fields
                        if let ConFields::Record(ref mut fields) = con.fields {
                            for field in fields.iter_mut() {
                                // Find the field name Ident token within the constructor span
                                let field_pos = self.find_ident_token(&field.name, con.span);
                                if let Some(pos) = field_pos {
                                    field.doc = self
                                        .find_leading_doc(pos)
                                        .or_else(|| self.find_trailing_doc(field.ty.span().end));
                                }
                            }
                        }
                    }
                }
                Decl::BitfieldDecl { fields, .. } => {
                    for field in fields.iter_mut() {
                        field.doc = self
                            .find_leading_doc(field.span.start)
                            .or_else(|| self.find_trailing_doc(field.span.end));
                    }
                }
                Decl::TraitDecl { methods, .. } => {
                    for method in methods.iter_mut() {
                        method.doc = self.find_leading_doc(method.span.start);
                    }
                }
                Decl::BuiltinImplDecl { methods, .. } => {
                    for method in methods.iter_mut() {
                        method.doc = self.find_leading_doc(method.span.start);
                    }
                }
                _ => {}
            }
        }
    }

    /// Scan backwards from `pos` in the token stream to find a `-- |` doc comment.
    fn find_leading_doc(&self, pos: u32) -> Option<String> {
        // Find the first non-layout token at or after pos, so that virtual
        // layout tokens (LayoutBraceOpen, etc.) with the same span.start
        // don't prevent us from scanning back past them.
        let tok_idx = self.tokens.iter().position(|t| {
            t.span.start >= pos
                && !matches!(
                    t.kind,
                    SyntaxKind::LayoutBraceOpen
                        | SyntaxKind::LayoutSemicolon
                        | SyntaxKind::LayoutBraceClose
                )
        })?;
        let mut i = tok_idx;
        let mut doc_lines = Vec::new();
        while i > 0 {
            i -= 1;
            let kind = self.tokens[i].kind;
            if kind == SyntaxKind::DocComment {
                let text = self.text_of(&self.tokens[i]);
                let stripped = text.strip_prefix("--").unwrap_or(text);
                if let Some(doc) = Self::extract_doc_text(stripped) {
                    doc_lines.push(doc);
                } else {
                    break; // `-- ^` is not a leading doc
                }
            } else if kind == SyntaxKind::Whitespace
                || kind == SyntaxKind::Newline
                || kind == SyntaxKind::LayoutSemicolon
                || kind == SyntaxKind::LayoutBraceOpen
                || kind == SyntaxKind::LayoutBraceClose
            {
                continue;
            } else {
                break;
            }
        }
        if doc_lines.is_empty() {
            None
        } else {
            doc_lines.reverse();
            Some(doc_lines.join("\n"))
        }
    }

    /// Scan forward from `pos` in the token stream to find a `-- ^` doc comment.
    fn find_trailing_doc(&self, pos: u32) -> Option<String> {
        // Find the first token at or after pos
        let tok_idx = self.tokens.iter().position(|t| t.span.start >= pos)?;
        let mut i = tok_idx;
        while i < self.tokens.len() {
            let kind = self.tokens[i].kind;
            if kind == SyntaxKind::DocComment {
                let text = self.text_of(&self.tokens[i]);
                let stripped = text.strip_prefix("--").unwrap_or(text);
                if let Some(rest) = stripped.strip_prefix(" ^ ") {
                    return Some(rest.trim().to_string());
                } else if stripped == " ^" {
                    return Some(String::new());
                }
                break;
            } else if kind == SyntaxKind::Whitespace || kind == SyntaxKind::Comma {
                i += 1;
                continue;
            } else {
                break;
            }
        }
        None
    }

    /// Find the start position of an Ident token matching `name` within `span`.
    fn find_ident_token(&self, name: &str, span: Span) -> Option<u32> {
        for tok in &self.tokens {
            if tok.span.start < span.start {
                continue;
            }
            if tok.span.start >= span.end {
                break;
            }
            if tok.kind == SyntaxKind::Ident && self.text_of(tok) == name {
                return Some(tok.span.start);
            }
        }
        None
    }

    // ═════════════════════════════════════════════════════════════════════
    // Declarations
    // ═════════════════════════════════════════════════════════════════════
}

mod helpers;
use helpers::*;
mod decl;
mod expr;
mod pat;
mod ty;

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> Program {
        let mut parser = Parser::new(source);
        parser.parse_program()
    }

    #[test]
    fn parse_type_sig() {
        let prog = parse("add : I32 -> I32 -> I32");
        assert_eq!(prog.decls.len(), 1);
        match &prog.decls[0] {
            Decl::TypeSig {
                name, constraints, ..
            } => {
                assert_eq!(name, "add");
                assert!(constraints.is_empty());
            }
            other => panic!("expected TypeSig, got {:?}", other),
        }
    }

    #[test]
    fn parse_constrained_type_sig() {
        let prog = parse("lighting : Light a => a -> a");
        assert_eq!(prog.decls.len(), 1);
        match &prog.decls[0] {
            Decl::TypeSig {
                name,
                constraints,
                ty,
                ..
            } => {
                assert_eq!(name, "lighting");
                assert_eq!(constraints.len(), 1);
                assert_eq!(constraints[0].trait_name, "Light");
                assert_eq!(constraints[0].tys.len(), 1);
                assert!(matches!(&constraints[0].tys[0], Type::Var(name, _) if name == "a"));
                assert!(matches!(ty, Type::Arrow(_, _, _)));
            }
            other => panic!("expected TypeSig, got {:?}", other),
        }
    }

    #[test]
    fn parse_dependent_array_type_sig() {
        let prog = parse("grid : Array 2 (Array 4 F32)");
        assert_eq!(prog.decls.len(), 1);
        match &prog.decls[0] {
            Decl::TypeSig { ty, .. } => match ty {
                Type::App(outer, inner, _) => {
                    assert!(matches!(outer.as_ref(), Type::App(_, _, _)));
                    assert!(matches!(inner.as_ref(), Type::Paren(_, _)));
                }
                other => panic!("expected applied array type, got {:?}", other),
            },
            other => panic!("expected TypeSig, got {:?}", other),
        }
    }

    #[test]
    fn parse_fun_decl() {
        let prog = parse("add x y = x + y");
        assert_eq!(prog.decls.len(), 1);
        match &prog.decls[0] {
            Decl::FunDecl { name, params, .. } => {
                assert_eq!(name, "add");
                assert_eq!(params.len(), 2);
            }
            other => panic!("expected FunDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_data_decl() {
        let prog = parse("data Color = Red | Green | Blue");
        assert_eq!(prog.decls.len(), 1);
        match &prog.decls[0] {
            Decl::DataDecl {
                name, constructors, ..
            } => {
                assert_eq!(name, "Color");
                assert_eq!(constructors.len(), 3);
                assert_eq!(constructors[0].name, "Red");
                assert_eq!(constructors[1].name, "Green");
                assert_eq!(constructors[2].name, "Blue");
            }
            other => panic!("expected DataDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_case_expr() {
        let source = "f c = match c\n  | Red -> 0\n  | Green -> 1\n  | Blue -> 2";
        let prog = parse(source);
        assert_eq!(prog.decls.len(), 1);
        match &prog.decls[0] {
            Decl::FunDecl { body, .. } => match body {
                Expr::Case(_, arms, _) => {
                    assert_eq!(arms.len(), 3);
                }
                other => panic!("expected Case, got {:?}", other),
            },
            other => panic!("expected FunDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_let_expr() {
        let source = "f x = let y = x + 1 in y";
        let prog = parse(source);
        match &prog.decls[0] {
            Decl::FunDecl { body, .. } => match body {
                Expr::Let(binds, _, _) => {
                    assert_eq!(binds.len(), 1);
                    assert_eq!(binds[0].name, "y");
                }
                other => panic!("expected Let, got {:?}", other),
            },
            other => panic!("expected FunDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_where_clause() {
        let source = "f x = y + 1 where y = x";
        let prog = parse(source);
        match &prog.decls[0] {
            Decl::FunDecl {
                body, where_binds, ..
            } => {
                assert!(matches!(body, Expr::Infix(..)));
                assert_eq!(where_binds.len(), 1);
                assert_eq!(where_binds[0].name, "y");
            }
            other => panic!("expected FunDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_lambda() {
        let source = "f = \\x y -> x + y";
        let prog = parse(source);
        match &prog.decls[0] {
            Decl::FunDecl { body, .. } => match body {
                Expr::Lambda(params, _, _) => {
                    assert_eq!(params.len(), 2);
                }
                other => panic!("expected Lambda, got {:?}", other),
            },
            other => panic!("expected FunDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_if_expr() {
        let source = "f x = if x == 0 then 1 else 2";
        let prog = parse(source);
        match &prog.decls[0] {
            Decl::FunDecl { body, .. } => {
                assert!(matches!(body, Expr::If(..)));
            }
            other => panic!("expected FunDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_full_program() {
        let source = "\
data Color = Red | Green | Blue

show : Color -> I32
show c = match c
  | Red   -> 0
  | Green -> 1
  | Blue  -> 2

add : I32 -> I32 -> I32
add x y = x + y

main : I32 -> I32
main x =
  let y = add x 1
  in show Red
";
        let prog = parse(source);
        // We expect: DataDecl, TypeSig, FunDecl, TypeSig, FunDecl, TypeSig, FunDecl
        assert!(
            prog.decls.len() >= 4,
            "expected at least 4 decls, got {}: {:#?}",
            prog.decls.len(),
            prog.decls
        );
    }

    #[test]
    fn parse_entry_point() {
        let source = "@vertex\nmain x = x + 1";
        let prog = parse(source);
        assert_eq!(prog.decls.len(), 1);
        match &prog.decls[0] {
            Decl::EntryPoint {
                attributes, name, ..
            } => {
                assert_eq!(attributes.len(), 1);
                assert_eq!(attributes[0].name, "vertex");
                assert_eq!(name, "main");
            }
            other => panic!("expected EntryPoint, got {:?}", other),
        }
    }

    #[test]
    fn parse_operator_section() {
        let source = "f = (+)";
        let prog = parse(source);
        match &prog.decls[0] {
            Decl::FunDecl { body, .. } => {
                assert!(matches!(body, Expr::OpSection(..)));
            }
            other => panic!("expected FunDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_tuple() {
        let source = "f = (1, 2, 3)";
        let prog = parse(source);
        match &prog.decls[0] {
            Decl::FunDecl { body, .. } => match body {
                Expr::Tuple(elems, _) => {
                    assert_eq!(elems.len(), 3);
                }
                other => panic!("expected Tuple, got {:?}", other),
            },
            other => panic!("expected FunDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_unit() {
        let source = "f = ()";
        let prog = parse(source);
        match &prog.decls[0] {
            Decl::FunDecl { body, .. } => {
                assert!(matches!(body, Expr::Tuple(elems, _) if elems.is_empty()));
            }
            other => panic!("expected FunDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_precedence() {
        // 1 + 2 * 3 should parse as 1 + (2 * 3)
        let source = "f = 1 + 2 * 3";
        let prog = parse(source);
        match &prog.decls[0] {
            Decl::FunDecl { body, .. } => match body {
                Expr::Infix(_, op, rhs, _) => {
                    assert_eq!(op, "+");
                    assert!(matches!(rhs.as_ref(), Expr::Infix(_, op2, _, _) if op2 == "*"));
                }
                other => panic!("expected Infix(+), got {:?}", other),
            },
            other => panic!("expected FunDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_dollar_right_assoc() {
        // f $ g $ x should parse as f $ (g $ x)
        let source = "r = f $ g $ x";
        let prog = parse(source);
        match &prog.decls[0] {
            Decl::FunDecl { body, .. } => match body {
                Expr::Infix(_lhs, op, rhs, _) => {
                    assert_eq!(op, "$");
                    // rhs should be another `$` application
                    assert!(matches!(rhs.as_ref(), Expr::Infix(_, op2, _, _) if op2 == "$"));
                }
                other => panic!("expected Infix($), got {:?}", other),
            },
            other => panic!("expected FunDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_function_application() {
        let source = "f = add x y";
        let prog = parse(source);
        match &prog.decls[0] {
            Decl::FunDecl { body, .. } => {
                // add x y  =  (add x) y  =  App(App(Var(add), Var(x)), Var(y))
                match body {
                    Expr::App(lhs, rhs, _) => {
                        assert!(matches!(rhs.as_ref(), Expr::Var(name, _) if name == "y"));
                        assert!(matches!(lhs.as_ref(), Expr::App(..)));
                    }
                    other => panic!("expected App, got {:?}", other),
                }
            }
            other => panic!("expected FunDecl, got {:?}", other),
        }
    }

    #[test]
    fn doc_comment_on_decl() {
        let source = "-- | Adds two numbers.\nadd x y = x + y";
        let prog = parse(source);
        assert_eq!(prog.decls.len(), 1);
        // Doc comment should appear in the comments list with its ` | ` prefix
        let comments = prog.decls[0].comments();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0], " | Adds two numbers.");
    }

    #[test]
    fn doc_comment_on_constructor() {
        let source = "data Shape =\n  -- | A circle with radius.\n  Circle F32\n  | -- | A rectangle.\n  Rect F32 F32";
        let prog = parse(source);
        match &prog.decls[0] {
            Decl::DataDecl { constructors, .. } => {
                assert_eq!(
                    constructors[0].doc.as_deref(),
                    Some("A circle with radius.")
                );
                assert_eq!(constructors[1].doc.as_deref(), Some("A rectangle."));
            }
            other => panic!("expected DataDecl, got {:?}", other),
        }
    }

    #[test]
    fn doc_comment_on_record_field() {
        let source = "data Point = Point {\n  -- | X coordinate.\n  x : F32,\n  -- | Y coordinate.\n  y : F32\n}";
        let prog = parse(source);
        match &prog.decls[0] {
            Decl::DataDecl { constructors, .. } => {
                if let ConFields::Record(fields) = &constructors[0].fields {
                    assert_eq!(fields[0].doc.as_deref(), Some("X coordinate."));
                    assert_eq!(fields[1].doc.as_deref(), Some("Y coordinate."));
                } else {
                    panic!("expected record fields");
                }
            }
            other => panic!("expected DataDecl, got {:?}", other),
        }
    }

    #[test]
    fn trailing_doc_comment_on_field() {
        let source =
            "data Point = Point {\n  x : F32, -- ^ X coordinate.\n  y : F32  -- ^ Y coordinate.\n}";
        let prog = parse(source);
        match &prog.decls[0] {
            Decl::DataDecl { constructors, .. } => {
                if let ConFields::Record(fields) = &constructors[0].fields {
                    assert_eq!(fields[0].doc.as_deref(), Some("X coordinate."));
                    assert_eq!(fields[1].doc.as_deref(), Some("Y coordinate."));
                } else {
                    panic!("expected record fields");
                }
            }
            other => panic!("expected DataDecl, got {:?}", other),
        }
    }

    #[test]
    fn doc_comment_on_trait_method() {
        let source = "trait HasArea a where\n  -- | Compute the area.\n  area : a -> F32";
        let prog = parse(source);
        match &prog.decls[0] {
            Decl::TraitDecl { methods, .. } => {
                assert_eq!(
                    methods[0].doc.as_deref(),
                    Some("Compute the area."),
                    "trait method doc should be attached"
                );
            }
            other => panic!("expected TraitDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_standalone_impl_method_signature() {
        let source = "impl F32 where\n  half : F32 -> F32\n  half x = x";
        let prog = parse(source);
        match &prog.decls[0] {
            Decl::ImplDecl { methods, .. } => {
                assert_eq!(methods.len(), 1);
                assert_eq!(methods[0].name, "half");
                assert!(
                    methods[0].ty.is_some(),
                    "expected impl-local type signature"
                );
            }
            other => panic!("expected ImplDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_builtin_type_decl() {
        let prog = parse("builtin type Vec 2");
        match &prog.decls[0] {
            Decl::BuiltinTypeDecl { name, arity, .. } => {
                assert_eq!(name, "Vec");
                assert_eq!(*arity, 2);
            }
            other => panic!("expected BuiltinTypeDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_record_pattern_with_rest() {
        let source = "f value = match value\n  | Active { life, .. } -> life\n  | Dead -> 0.0";
        let prog = parse(source);
        match &prog.decls[0] {
            Decl::FunDecl { body, .. } => match body {
                Expr::Case(_, arms, _) => match &arms[0].0 {
                    Pat::Record(name, fields, has_rest, _) => {
                        assert_eq!(name, "Active");
                        assert_eq!(fields.len(), 1);
                        assert_eq!(fields[0].0, "life");
                        assert!(*has_rest, "expected record pattern rest marker");
                    }
                    other => panic!("expected record pattern, got {:?}", other),
                },
                other => panic!("expected match expression, got {:?}", other),
            },
            other => panic!("expected FunDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_render_block_with_bindings_and_entries() {
        let source = r#"render test
  @group(0)
    @binding(0) uniform globals : Globals

  @vertex
  vsMain : VertexInput -> VertexOutput
  vsMain input =
    let pos = vec4 0.0 0.0 0.0 1.0
    in VertexOutput { position = pos }

  @fragment
  fsMain : VertexOutput -> Vec<4, F32>
  fsMain input = vec4 1.0 0.0 0.0 1.0
"#;
        let prog = parse(source);
        assert_eq!(
            prog.decls.len(),
            1,
            "expected exactly one render block decl"
        );
        match &prog.decls[0] {
            Decl::RenderBlock {
                name,
                bindings,
                entries,
                ..
            } => {
                assert_eq!(name, "test");
                assert_eq!(bindings.len(), 1, "expected one binding decl");
                match &bindings[0] {
                    Decl::BindingDecl { name, .. } => {
                        assert_eq!(name, "globals");
                    }
                    other => panic!("expected BindingDecl, got {:?}", other),
                }
                assert_eq!(
                    entries.len(),
                    4,
                    "expected 4 entries: 2 type sigs + 2 entry points"
                );
                assert!(
                    matches!(&entries[0],
                        Decl::TypeSig { name, .. } if name == "vsMain"
                    ),
                    "expected TypeSig vsMain, got {:?}",
                    entries[0]
                );
                assert!(
                    matches!(
                        &entries[1],
                        Decl::EntryPoint { name, .. } if name == "vsMain"
                    ),
                    "expected EntryPoint vsMain, got {:?}",
                    entries[1]
                );
                assert!(
                    matches!(
                        &entries[2],
                        Decl::TypeSig { name, .. } if name == "fsMain"
                    ),
                    "expected TypeSig fsMain, got {:?}",
                    entries[2]
                );
                assert!(
                    matches!(
                        &entries[3],
                        Decl::EntryPoint { name, .. } if name == "fsMain"
                    ),
                    "expected EntryPoint fsMain, got {:?}",
                    entries[3]
                );
            }
            other => panic!("expected RenderBlock, got {:?}", other),
        }
    }
}
