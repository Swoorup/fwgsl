//! Mid-level Intermediate Representation (MIR) for shadml.
//!
//! MIR is a lowered representation suitable for code generation.
//! It has been type-checked, monomorphised, and desugared from HIR
//! into a form that maps closely to WGSL constructs.
//!
//! All MIR types are parameterised by a lifetime `'a` that ties them
//! to an arena allocator.  Strings are stored as `&'a str` and
//! recursive nodes as `&'a T` instead of `Box<T>`, enabling
//! bump-allocated, cache-friendly compilation.

pub mod lower;
pub mod reachability;
pub mod validate;

use std::fmt;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// An error that can occur during HIR → MIR lowering.
#[derive(Debug, Clone, PartialEq)]
pub enum MirLowerError {
    /// A type cannot be represented in MIR / WGSL.
    UnsupportedType(String),
    /// An expression construct is not supported in MIR lowering.
    UnsupportedExpr(String),
    /// An expression appears in an invalid context (e.g. `if` in pure position).
    InvalidContext(String),
}

impl fmt::Display for MirLowerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MirLowerError::UnsupportedType(ty) => write!(f, "cannot convert to MIR type: {ty}"),
            MirLowerError::UnsupportedExpr(msg) => write!(f, "{msg}"),
            MirLowerError::InvalidContext(msg) => write!(f, "{msg}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level program
// ---------------------------------------------------------------------------

/// A complete MIR program ready for code generation.
#[derive(Debug, Clone, PartialEq)]
pub struct MirProgram<'a> {
    pub structs: Vec<MirStruct<'a>>,
    pub globals: Vec<MirGlobal<'a>>,
    pub functions: Vec<MirFunction<'a>>,
    pub entry_points: Vec<MirEntryPoint<'a>>,
    pub constants: Vec<MirConst<'a>>,
    pub render_blocks: Vec<MirRenderBlock<'a>>,
}

/// A module-level constant declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct MirConst<'a> {
    pub name: &'a str,
    pub ty: MirType<'a>,
    pub value: MirExpr<'a>,
}

/// A module-scope variable declaration (GPU binding).
#[derive(Debug, Clone, PartialEq)]
pub struct MirGlobal<'a> {
    pub name: &'a str,
    pub address_space: AddressSpace,
    pub ty: MirType<'a>,
    pub group: u32,
    pub binding: u32,
    /// The module name where this global was originally defined.
    pub origin_module: Option<&'a str>,
}

/// WGSL address space for global bindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddressSpace {
    Uniform,
    StorageRead,
    StorageReadWrite,
    /// `var<immediate>` — push constants (no @group/@binding)
    Immediate,
    /// Opaque resource (texture/sampler) — no address space keyword
    Opaque,
}

/// A render block: `render name { bindings; entry_points }`
/// Explicitly scopes bindings to a vertex+fragment pipeline pair.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MirRenderBlock<'a> {
    pub name: &'a str,
    /// Names of globals that belong to this render block.
    pub binding_names: Vec<&'a str>,
    /// Name of the vertex entry point.
    pub vertex_entry: &'a str,
    /// Name of the fragment entry point.
    pub fragment_entry: &'a str,
}

// ---------------------------------------------------------------------------
// Struct definitions
// ---------------------------------------------------------------------------

/// A struct type definition.
#[derive(Debug, Clone, PartialEq)]
pub struct MirStruct<'a> {
    pub name: &'a str,
    pub fields: Vec<MirField<'a>>,
    /// The module name where this struct was originally defined.
    pub origin_module: Option<&'a str>,
    /// If this struct was lowered from a multi-constructor ADT, preserve variant metadata.
    pub adt_variants: Option<Vec<MirAdtVariant<'a>>>,
    /// If this struct was lowered from a bitfield, preserve field bit-ranges.
    pub bitfield_fields: Option<Vec<MirBitfieldFieldDef<'a>>>,
}

/// Metadata for a single ADT variant, preserved during lowering for bindgen.
#[derive(Debug, Clone, PartialEq)]
pub struct MirAdtVariant<'a> {
    pub name: &'a str,
    pub tag: u32,
    pub fields: Vec<MirField<'a>>,
}

/// Metadata for a single bitfield field, preserved during lowering for bindgen.
#[derive(Debug, Clone, PartialEq)]
pub struct MirBitfieldFieldDef<'a> {
    pub name: &'a str,
    pub offset: u32,
    pub width: u32,
}

/// A single field in a struct.
#[derive(Debug, Clone, PartialEq)]
pub struct MirField<'a> {
    pub name: &'a str,
    pub ty: MirType<'a>,
    pub attributes: Vec<MirAttribute<'a>>,
}

/// An attribute annotation (e.g. `@location(0)`, `@builtin(position)`).
#[derive(Debug, Clone, PartialEq)]
pub struct MirAttribute<'a> {
    pub name: &'a str,
    pub args: Vec<&'a str>,
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Concrete types that map to WGSL types.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MirType<'a> {
    I32,
    U32,
    F32,
    Bool,
    /// `vec{n}<T>` — e.g. `Vec(3, F32)` → `vec3<f32>`
    Vec(u8, &'a MirType<'a>),
    /// `mat{cols}x{rows}<T>` — e.g. `Mat(4, 4, F32)` → `mat4x4<f32>`
    Mat(u8, u8, &'a MirType<'a>),
    /// A user-defined struct type.
    Struct(&'a str),
    /// `array<T, N>`
    Array(&'a MirType<'a>, u32),
    /// `array<T>` (unsized / runtime-sized storage array)
    RuntimeArray(&'a MirType<'a>),
    /// `texture_2d<f32>`
    Texture2d(&'a MirType<'a>),
    /// `texture_multisampled_2d<f32>`
    Texture2dMultisampled(&'a MirType<'a>),
    /// `texture_2d_array<f32, N>`
    Texture2dArray(&'a MirType<'a>),
    /// `sampler`
    Sampler,
    /// `sampler_comparison`
    SamplerComparison,
    /// `binding_array<T, N>`
    BindingArray(&'a MirType<'a>, u32),
    /// The unit type — no WGSL representation (used for void returns).
    Unit,
}

impl fmt::Display for MirType<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MirType::I32 => write!(f, "i32"),
            MirType::U32 => write!(f, "u32"),
            MirType::F32 => write!(f, "f32"),
            MirType::Bool => write!(f, "bool"),
            MirType::Vec(n, inner) => write!(f, "vec{}<{}>", n, inner),
            MirType::Mat(cols, rows, inner) => write!(f, "mat{}x{}<{}>", cols, rows, inner),
            MirType::Struct(name) => write!(f, "{}", name),
            MirType::Array(inner, len) => write!(f, "array<{}, {}>", inner, len),
            MirType::RuntimeArray(inner) => write!(f, "array<{}>", inner),
            MirType::Texture2d(inner) => write!(f, "texture_2d<{}>", inner),
            MirType::Texture2dMultisampled(inner) => {
                write!(f, "texture_multisampled_2d<{}>", inner)
            }
            MirType::Texture2dArray(inner) => write!(f, "texture_2d_array<{}>", inner),
            MirType::Sampler => write!(f, "sampler"),
            MirType::SamplerComparison => write!(f, "sampler_comparison"),
            MirType::BindingArray(inner, count) => write!(f, "binding_array<{}, {}>", inner, count),
            MirType::Unit => write!(f, "void"),
        }
    }
}

// ---------------------------------------------------------------------------
// Functions
// ---------------------------------------------------------------------------

/// A regular (non-entry-point) function.
#[derive(Debug, Clone, PartialEq)]
pub struct MirFunction<'a> {
    pub name: &'a str,
    pub params: Vec<MirParam<'a>>,
    pub return_ty: MirType<'a>,
    pub body: Vec<MirStmt<'a>>,
    pub return_expr: Option<MirExpr<'a>>,
    pub comments: Vec<&'a str>,
}

/// A function parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct MirParam<'a> {
    pub name: &'a str,
    pub ty: MirType<'a>,
}

// ---------------------------------------------------------------------------
// Entry points (shader stages)
// ---------------------------------------------------------------------------

/// An entry-point function annotated with a shader stage.
#[derive(Debug, Clone, PartialEq)]
pub struct MirEntryPoint<'a> {
    pub name: &'a str,
    pub stage: ShaderStage,
    /// Workgroup size for compute shaders — `[x, y, z]`.
    pub workgroup_size: Option<[u32; 3]>,
    pub params: Vec<MirParam<'a>>,
    pub return_ty: MirType<'a>,
    pub body: Vec<MirStmt<'a>>,
    pub return_expr: Option<MirExpr<'a>>,
    pub comments: Vec<&'a str>,
}

/// Shader stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderStage {
    Compute,
    Vertex,
    Fragment,
}

impl fmt::Display for ShaderStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShaderStage::Compute => write!(f, "compute"),
            ShaderStage::Vertex => write!(f, "vertex"),
            ShaderStage::Fragment => write!(f, "fragment"),
        }
    }
}

// ---------------------------------------------------------------------------
// Statements
// ---------------------------------------------------------------------------

/// A MIR statement.
#[derive(Debug, Clone, PartialEq)]
pub enum MirStmt<'a> {
    /// `let name: ty = expr;`
    Let(&'a str, MirType<'a>, MirExpr<'a>),
    /// `var name: ty = expr;`
    Var(&'a str, MirType<'a>, MirExpr<'a>),
    /// `name = expr;`
    Assign(&'a str, MirExpr<'a>),
    /// `base[index] = expr;`
    IndexAssign(MirExpr<'a>, MirExpr<'a>, MirExpr<'a>),
    /// `if (cond) { then } else { else }`
    If(MirExpr<'a>, Vec<MirStmt<'a>>, Vec<MirStmt<'a>>),
    /// `return expr;`
    Return(MirExpr<'a>),
    /// A nested block `{ ... }`
    Block(Vec<MirStmt<'a>>),
    /// `switch (expr) { case Xu: { ... } ... default: { ... } }`
    Switch(MirExpr<'a>, Vec<MirSwitchCase<'a>>, Vec<MirStmt<'a>>),
    /// `loop { body }`
    Loop(Vec<MirStmt<'a>>),
    /// `break;`
    Break,
    /// `continue;`
    Continue,
}

/// A single `case` arm in a switch statement (supports multi-value: `case 0u, 1u:`).
#[derive(Debug, Clone, PartialEq)]
pub struct MirSwitchCase<'a> {
    pub values: Vec<MirLit>,
    pub body: Vec<MirStmt<'a>>,
}

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

/// A MIR expression.
#[derive(Debug, Clone, PartialEq)]
pub enum MirExpr<'a> {
    /// A literal value.
    Lit(MirLit),
    /// A variable reference. The second element is the type.
    Var(&'a str, MirType<'a>),
    /// A binary operation: `op(lhs, rhs) -> ty`.
    BinOp(MirBinOp, &'a MirExpr<'a>, &'a MirExpr<'a>, MirType<'a>),
    /// A unary operation: `op(operand) -> ty`.
    UnaryOp(MirUnaryOp, &'a MirExpr<'a>, MirType<'a>),
    /// A function call: `name(args) -> ty`.
    Call(&'a str, Vec<MirExpr<'a>>, MirType<'a>),
    /// Struct construction: `Name(field_exprs...)`.
    ConstructStruct(&'a str, Vec<MirExpr<'a>>),
    /// Field access: `expr.field -> ty`.
    FieldAccess(&'a MirExpr<'a>, &'a str, MirType<'a>),
    /// Index access: `expr[index] -> ty`.
    Index(&'a MirExpr<'a>, &'a MirExpr<'a>, MirType<'a>),
    /// Type cast: `ty(expr)`.
    Cast(&'a MirExpr<'a>, MirType<'a>),
}

impl<'a> MirExpr<'a> {
    /// Return the result type of this expression, if it carries one.
    pub fn result_type(&self) -> Option<MirType<'a>> {
        match self {
            MirExpr::Lit(lit) => Some(match lit {
                MirLit::I32(_) => MirType::I32,
                MirLit::U32(_) => MirType::U32,
                MirLit::F32(_) => MirType::F32,
                MirLit::Bool(_) => MirType::Bool,
            }),
            MirExpr::Var(_, ty) => Some(ty.clone()),
            MirExpr::BinOp(_, _, _, ty) => Some(ty.clone()),
            MirExpr::UnaryOp(_, _, ty) => Some(ty.clone()),
            MirExpr::Call(_, _, ty) => Some(ty.clone()),
            MirExpr::ConstructStruct(name, _) => Some(MirType::Struct(name)),
            MirExpr::FieldAccess(_, _, ty) => Some(ty.clone()),
            MirExpr::Index(_, _, ty) => Some(ty.clone()),
            MirExpr::Cast(_, ty) => Some(ty.clone()),
        }
    }

    /// Produce a zero/default value for the given MIR type.
    ///
    /// For composite types (vec, mat, struct, array) we emit a
    /// zero-value constructor — e.g. `vec3<f32>()` or `MyStruct()` —
    /// which WGSL defines as all-zeros / false / 0.0.
    ///
    /// The arena is needed to allocate the type-name string for
    /// `Call` nodes that reference computed names like `"vec3"`.
    pub fn default_value(arena: &'a shadml_allocator::Allocator, ty: &MirType<'a>) -> MirExpr<'a> {
        match ty {
            MirType::I32 => MirExpr::Lit(MirLit::I32(0)),
            MirType::U32 => MirExpr::Lit(MirLit::U32(0)),
            MirType::F32 => MirExpr::Lit(MirLit::F32(0.0)),
            MirType::Bool => MirExpr::Lit(MirLit::Bool(false)),
            // vec / mat: use short names so is_type_constructor_call matches
            MirType::Vec(n, _) => {
                let name = arena.alloc_str(&format!("vec{}", n));
                MirExpr::Call(name, vec![], ty.clone())
            }
            MirType::Mat(cols, rows, _) => {
                let name = arena.alloc_str(&format!("mat{}x{}", cols, rows));
                MirExpr::Call(name, vec![], ty.clone())
            }
            // Struct: zero-value constructor is just TypeName()
            MirType::Struct(name) => MirExpr::Call(name, vec![], ty.clone()),
            // Array: zero-value constructor is array<T, N>()
            MirType::Array(..) | MirType::RuntimeArray(_) => {
                let name = arena.alloc_str(&ty.to_string());
                MirExpr::Call(name, vec![], ty.clone())
            }
            // Opaque types don't have default values — they are resources
            MirType::Texture2d(_)
            | MirType::Texture2dMultisampled(_)
            | MirType::Texture2dArray(_)
            | MirType::Sampler
            | MirType::SamplerComparison
            | MirType::BindingArray(..) => MirExpr::Lit(MirLit::I32(0)),
            MirType::Unit => MirExpr::Lit(MirLit::I32(0)),
        }
    }
}

// ---------------------------------------------------------------------------
// Literals
// ---------------------------------------------------------------------------

/// A literal value.
#[derive(Debug, Clone, PartialEq)]
pub enum MirLit {
    I32(i32),
    U32(u32),
    F32(f64), // stored as f64 for precision during compilation
    Bool(bool),
}

// ---------------------------------------------------------------------------
// Operators
// ---------------------------------------------------------------------------

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MirBinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Neq,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

impl fmt::Display for MirBinOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            MirBinOp::Add => "+",
            MirBinOp::Sub => "-",
            MirBinOp::Mul => "*",
            MirBinOp::Div => "/",
            MirBinOp::Mod => "%",
            MirBinOp::Eq => "==",
            MirBinOp::Neq => "!=",
            MirBinOp::Lt => "<",
            MirBinOp::Le => "<=",
            MirBinOp::Gt => ">",
            MirBinOp::Ge => ">=",
            MirBinOp::And => "&&",
            MirBinOp::Or => "||",
            MirBinOp::BitAnd => "&",
            MirBinOp::BitOr => "|",
            MirBinOp::BitXor => "^",
            MirBinOp::Shl => "<<",
            MirBinOp::Shr => ">>",
        };
        write!(f, "{}", s)
    }
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MirUnaryOp {
    Neg,
    Not,
    BitNot,
}

impl fmt::Display for MirUnaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            MirUnaryOp::Neg => "-",
            MirUnaryOp::Not => "!",
            MirUnaryOp::BitNot => "~",
        };
        write!(f, "{}", s)
    }
}
