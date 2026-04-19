//! WGSL code generation from MIR.
//!
//! This crate provides a tree-walk emitter that translates a [`MirProgram`]
//! into valid WGSL source text. The emitter walks each struct, function, and
//! entry point in the program and serialises them into a `String` buffer.

use shadml_mir::*;

/// Map shadml-internal overloaded texture function names to their WGSL equivalents.
///
/// shadml uses distinct names for each overload (e.g. `textureLoadMsaa`)
/// because the type environment cannot store multiple schemes per name.
/// WGSL uses the same name for all overloads, resolved by argument types.
fn wgsl_builtin_name(name: &str) -> &str {
    match name {
        "textureSampleArray" => "textureSample",
        "textureLoadMsaa" => "textureLoad",
        "textureLoadArray" => "textureLoad",
        "textureDimensionsMsaa" => "textureDimensions",
        "textureDimensionsArray" => "textureDimensions",
        _ => name,
    }
}

fn sanitize_identifier(name: &str) -> String {
    // Split into base name and count of trailing primes
    let prime_count = name.chars().rev().take_while(|&c| c == '\'').count();
    let base = &name[..name.len() - prime_count];

    let mut out = String::with_capacity(base.len() + 4);
    for ch in base.chars() {
        match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' => out.push(ch),
            _ => out.push('_'),
        }
    }

    if out.is_empty() {
        out.push('_');
    } else if out.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        out.insert(0, '_');
    }

    // Apply reserved-word escaping to the base name (before prime suffix)
    if is_reserved_identifier(&out) {
        out.insert_str(0, "fw_");
    }

    // Append numbered suffix for primes: x' -> x_1, x'' -> x_2, etc.
    if prime_count > 0 {
        out.push('_');
        out.push_str(&prime_count.to_string());
    }

    out
}

fn is_reserved_identifier(name: &str) -> bool {
    matches!(
        name,
        "alias"
            | "bitcast"
            | "bool"
            | "break"
            | "case"
            | "compute"
            | "const"
            | "const_assert"
            | "continue"
            | "continuing"
            | "default"
            | "diagnostic"
            | "discard"
            | "else"
            | "enable"
            | "f32"
            | "false"
            | "fn"
            | "for"
            | "fragment"
            | "i32"
            | "if"
            | "let"
            | "loop"
            | "mat2x2"
            | "mat3x3"
            | "mat4x4"
            | "override"
            | "private"
            | "read"
            | "read_write"
            | "requires"
            | "return"
            | "storage"
            | "struct"
            | "switch"
            | "true"
            | "u32"
            | "uniform"
            | "var"
            | "vec2"
            | "vec3"
            | "vec4"
            | "vertex"
            | "while"
            | "workgroup"
            | "write"
            | "final"
    )
}

// WGSL operator precedence levels (higher = tighter binding).
// See https://www.w3.org/TR/WGSL/#operator-precedence-associativity
const PREC_OR: u8 = 1;
const PREC_AND: u8 = 2;
const PREC_BITOR: u8 = 3;
const PREC_BITXOR: u8 = 4;
const PREC_BITAND: u8 = 5;
const PREC_EQ: u8 = 6; // == !=
const PREC_REL: u8 = 7; // < > <= >=
const PREC_SHIFT: u8 = 8; // << >>
const PREC_ADD: u8 = 9; // + -
const PREC_MUL: u8 = 10; // * / %
const PREC_UNARY: u8 = 11; // - ! ~
const PREC_POSTFIX: u8 = 12; // . []

fn binop_precedence(op: &MirBinOp) -> u8 {
    match op {
        MirBinOp::Or => PREC_OR,
        MirBinOp::And => PREC_AND,
        MirBinOp::BitOr => PREC_BITOR,
        MirBinOp::BitXor => PREC_BITXOR,
        MirBinOp::BitAnd => PREC_BITAND,
        MirBinOp::Eq | MirBinOp::Neq => PREC_EQ,
        MirBinOp::Lt | MirBinOp::Le | MirBinOp::Gt | MirBinOp::Ge => PREC_REL,
        MirBinOp::Shl | MirBinOp::Shr => PREC_SHIFT,
        MirBinOp::Add | MirBinOp::Sub => PREC_ADD,
        MirBinOp::Mul | MirBinOp::Div | MirBinOp::Mod => PREC_MUL,
    }
}

/// A tree-walk emitter that writes WGSL text into a `String` buffer.
pub struct WgslEmitter {
    output: String,
    indent: usize,
    preserve_comments: bool,
}

impl Default for WgslEmitter {
    fn default() -> Self {
        Self::new()
    }
}

impl WgslEmitter {
    /// Create a new emitter with an empty output buffer.
    pub fn new() -> Self {
        Self {
            output: String::new(),
            indent: 0,
            preserve_comments: false,
        }
    }

    /// Create a new emitter that preserves source comments.
    pub fn with_comments() -> Self {
        Self {
            output: String::new(),
            indent: 0,
            preserve_comments: true,
        }
    }

    /// Consume the emitter and produce the WGSL text for the given program.
    pub fn emit_program(mut self, program: &MirProgram) -> String {
        // Emit constants
        for c in &program.constants {
            self.emit_const(c);
            self.newline();
        }

        // Emit structs
        for s in &program.structs {
            self.emit_struct(s);
            self.newline();
        }

        // Emit global bindings
        for g in &program.globals {
            self.emit_global(g);
            self.newline();
        }

        // Emit functions
        for f in &program.functions {
            self.emit_comments(&f.comments);
            self.emit_function(f);
            self.newline();
        }

        // Emit entry points
        for ep in &program.entry_points {
            self.emit_comments(&ep.comments);
            self.emit_entry_point(ep);
            self.newline();
        }

        self.output.trim_end().to_string() + "\n"
    }

    // -----------------------------------------------------------------------
    // Struct emission
    // -----------------------------------------------------------------------
    // Constant emission
    // -----------------------------------------------------------------------

    fn emit_comments(&mut self, comments: &[&str]) {
        if !self.preserve_comments || comments.is_empty() {
            return;
        }
        for comment in comments {
            self.write(&format!("//{}", comment));
            self.newline();
        }
    }

    fn emit_const(&mut self, c: &MirConst) {
        self.write(&format!(
            "const {}: {} = ",
            sanitize_identifier(&c.name),
            self.format_type(&c.ty)
        ));
        self.emit_expr(&c.value);
        self.write(";");
        self.newline();
    }

    // -----------------------------------------------------------------------
    // Struct emission
    // -----------------------------------------------------------------------

    fn emit_struct(&mut self, s: &MirStruct) {
        self.write(&format!("struct {} {{", sanitize_identifier(&s.name)));
        self.newline();
        self.indent += 1;
        for field in &s.fields {
            self.write_indent();
            // Emit field attributes (e.g. @location(0), @builtin(position))
            for attr in &field.attributes {
                self.write("@");
                self.write(&attr.name);
                if !attr.args.is_empty() {
                    self.write("(");
                    self.write(&attr.args.join(", "));
                    self.write(")");
                }
                self.write(" ");
            }
            self.write(&format!(
                "{}: {},",
                sanitize_identifier(&field.name),
                self.format_type(&field.ty)
            ));
            self.newline();
        }
        self.indent -= 1;
        self.write("}");
        self.newline();
    }

    // -----------------------------------------------------------------------
    // Global variable emission (GPU bindings)
    // -----------------------------------------------------------------------

    fn emit_global(&mut self, g: &MirGlobal) {
        match g.address_space {
            AddressSpace::Uniform | AddressSpace::StorageRead | AddressSpace::StorageReadWrite => {
                let addr_space = match g.address_space {
                    AddressSpace::Uniform => "uniform",
                    AddressSpace::StorageRead => "storage, read",
                    AddressSpace::StorageReadWrite => "storage, read_write",
                    _ => unreachable!(),
                };
                self.write(&format!(
                    "@group({}) @binding({}) var<{}> {}: {};",
                    g.group,
                    g.binding,
                    addr_space,
                    sanitize_identifier(&g.name),
                    self.format_type(&g.ty),
                ));
                self.newline();
            }
            AddressSpace::Immediate => {
                self.write(&format!(
                    "var<immediate> {}: {};",
                    sanitize_identifier(&g.name),
                    self.format_type(&g.ty),
                ));
                self.newline();
            }
            AddressSpace::Opaque => {
                self.write(&format!(
                    "@group({}) @binding({}) var {}: {};",
                    g.group,
                    g.binding,
                    sanitize_identifier(&g.name),
                    self.format_type(&g.ty),
                ));
                self.newline();
            }
        }
    }

    // -----------------------------------------------------------------------
    // Function emission
    // -----------------------------------------------------------------------

    fn emit_function(&mut self, f: &MirFunction) {
        self.write(&format!("fn {}(", sanitize_identifier(&f.name)));
        for (i, param) in f.params.iter().enumerate() {
            if i > 0 {
                self.write(", ");
            }
            self.write(&format!(
                "{}: {}",
                sanitize_identifier(&param.name),
                self.format_type(&param.ty)
            ));
        }
        self.write(")");
        if f.return_ty != MirType::Unit {
            self.write(&format!(" -> {}", self.format_type(&f.return_ty)));
        }
        self.write(" {");
        self.newline();
        self.indent += 1;

        for stmt in &f.body {
            self.emit_stmt(stmt);
        }

        if let Some(ref expr) = f.return_expr {
            self.write_indent();
            self.write("return ");
            self.emit_expr(expr);
            self.write(";");
            self.newline();
        }

        self.indent -= 1;
        self.write("}");
        self.newline();
    }

    // -----------------------------------------------------------------------
    // Entry-point emission
    // -----------------------------------------------------------------------

    fn emit_entry_point(&mut self, ep: &MirEntryPoint) {
        // Stage attribute
        match ep.stage {
            ShaderStage::Compute => {
                if let Some(wg) = ep.workgroup_size {
                    self.write(&format!(
                        "@compute @workgroup_size({}, {}, {})",
                        wg[0], wg[1], wg[2]
                    ));
                } else {
                    self.write("@compute @workgroup_size(1, 1, 1)");
                }
            }
            ShaderStage::Vertex => self.write("@vertex"),
            ShaderStage::Fragment => self.write("@fragment"),
        }
        self.newline();

        self.write(&format!("fn {}(", sanitize_identifier(&ep.name)));

        let mut first = true;

        // Parameters — struct-typed params carry @builtin/@location on their fields
        for param in &ep.params {
            if !first {
                self.write(", ");
            }
            first = false;
            self.write(&format!(
                "{}: {}",
                sanitize_identifier(&param.name),
                self.format_type(&param.ty)
            ));
        }

        self.write(")");
        if ep.return_ty != MirType::Unit {
            // Non-struct return types on vertex/fragment entry points need @location(0)
            let needs_location = matches!(ep.stage, ShaderStage::Vertex | ShaderStage::Fragment)
                && !matches!(ep.return_ty, MirType::Struct(_));
            if needs_location {
                self.write(&format!(
                    " -> @location(0) {}",
                    self.format_type(&ep.return_ty)
                ));
            } else {
                self.write(&format!(" -> {}", self.format_type(&ep.return_ty)));
            }
        }
        self.write(" {");
        self.newline();
        self.indent += 1;

        for stmt in &ep.body {
            self.emit_stmt(stmt);
        }

        if let Some(ref expr) = ep.return_expr {
            self.write_indent();
            self.write("return ");
            self.emit_expr(expr);
            self.write(";");
            self.newline();
        }

        self.indent -= 1;
        self.write("}");
        self.newline();
    }

    // -----------------------------------------------------------------------
    // Statement emission
    // -----------------------------------------------------------------------

    fn emit_stmt(&mut self, stmt: &MirStmt) {
        match stmt {
            MirStmt::Let(name, ty, expr) => {
                // Unit-typed lets are side-effect markers; WGSL has no void type.
                if !matches!(ty, MirType::Unit) {
                    self.write_indent();
                    self.write(&format!("let {} = ", sanitize_identifier(name)));
                    self.emit_expr(expr);
                    self.write(";");
                    self.newline();
                }
            }
            MirStmt::Var(name, ty, expr) => {
                // Unit-typed variables are side-effect markers (e.g. from loops on void).
                // WGSL has no `void` type, so skip the declaration as a safety net.
                if !matches!(ty, MirType::Unit) {
                    self.write_indent();
                    self.write(&format!(
                        "var {}: {} = ",
                        sanitize_identifier(name),
                        self.format_type(ty)
                    ));
                    self.emit_expr(expr);
                    self.write(";");
                    self.newline();
                }
            }
            MirStmt::Assign(name, expr) => {
                self.write_indent();
                self.write(&format!("{} = ", sanitize_identifier(name)));
                self.emit_expr(expr);
                self.write(";");
                self.newline();
            }
            MirStmt::IndexAssign(base, index, value) => {
                self.write_indent();
                self.emit_expr(base);
                self.write("[");
                self.emit_expr(index);
                self.write("] = ");
                self.emit_expr(value);
                self.write(";");
                self.newline();
            }
            MirStmt::If(cond, then_stmts, else_stmts) => {
                self.write_indent();
                self.write("if (");
                self.emit_expr(cond);
                self.write(") {");
                self.newline();
                self.indent += 1;
                for s in then_stmts {
                    self.emit_stmt(s);
                }
                self.indent -= 1;
                // Only emit the else clause if there are statements to emit.
                // Empty else blocks are invalid in WGSL compute entry points.
                if !else_stmts.is_empty() {
                    self.write_indent();
                    self.write("} else {");
                    self.newline();
                    self.indent += 1;
                    for s in else_stmts {
                        self.emit_stmt(s);
                    }
                    self.indent -= 1;
                    self.write_indent();
                    self.write("}");
                    self.newline();
                } else {
                    self.write_indent();
                    self.write("}");
                    self.newline();
                }
            }
            MirStmt::Return(expr) => {
                self.write_indent();
                self.write("return ");
                self.emit_expr(expr);
                self.write(";");
                self.newline();
            }
            MirStmt::Block(stmts) => {
                self.write_indent();
                self.write("{");
                self.newline();
                self.indent += 1;
                for s in stmts {
                    self.emit_stmt(s);
                }
                self.indent -= 1;
                self.write_indent();
                self.write("}");
                self.newline();
            }
            MirStmt::Switch(expr, cases, default_body) => {
                self.write_indent();
                self.write("switch (");
                self.emit_expr(expr);
                self.write(") {");
                self.newline();
                self.indent += 1;
                for case in cases {
                    self.write_indent();
                    self.write("case ");
                    for (i, val) in case.values.iter().enumerate() {
                        if i > 0 {
                            self.write(", ");
                        }
                        self.emit_lit(val);
                    }
                    self.write(": {");
                    self.newline();
                    self.indent += 1;
                    for s in &case.body {
                        self.emit_stmt(s);
                    }
                    self.indent -= 1;
                    self.write_indent();
                    self.write("}");
                    self.newline();
                }
                if !default_body.is_empty() {
                    self.write_indent();
                    self.write("default: {");
                    self.newline();
                    self.indent += 1;
                    for s in default_body {
                        self.emit_stmt(s);
                    }
                    self.indent -= 1;
                    self.write_indent();
                    self.write("}");
                    self.newline();
                }
                self.indent -= 1;
                self.write_indent();
                self.write("}");
                self.newline();
            }
            MirStmt::Loop(body) => {
                self.write_indent();
                self.write("loop {");
                self.newline();
                self.indent += 1;
                for s in body {
                    self.emit_stmt(s);
                }
                self.indent -= 1;
                self.write_indent();
                self.write("}");
                self.newline();
            }
            MirStmt::Break => {
                self.write_indent();
                self.write("break;");
                self.newline();
            }
            MirStmt::Continue => {
                self.write_indent();
                self.write("continue;");
                self.newline();
            }
        }
    }

    // -----------------------------------------------------------------------
    // Expression emission (precedence-aware)
    // -----------------------------------------------------------------------

    fn emit_expr(&mut self, expr: &MirExpr) {
        self.emit_expr_prec(expr, 0);
    }

    /// Emit an expression, inserting parentheses only when the expression's
    /// precedence is lower than the surrounding context (`min_prec`).
    fn emit_expr_prec(&mut self, expr: &MirExpr, min_prec: u8) {
        match expr {
            MirExpr::Lit(lit) => self.emit_lit(lit),
            MirExpr::Var(name, _) => self.write(&sanitize_identifier(name)),
            MirExpr::BinOp(op, lhs, rhs, _) => {
                let prec = binop_precedence(op);
                let need_parens = prec < min_prec;
                if need_parens {
                    self.write("(");
                }
                // Left-associative: left child uses same precedence,
                // right child uses prec+1 to parenthesize same-precedence on the right.
                self.emit_expr_prec(lhs, prec);
                self.write(&format!(" {} ", op));
                self.emit_expr_prec(rhs, prec + 1);
                if need_parens {
                    self.write(")");
                }
            }
            MirExpr::UnaryOp(op, operand, _) => {
                self.write(&format!("{}", op));
                self.emit_expr_prec(operand, PREC_UNARY);
            }
            MirExpr::Call(name, args, ty) => {
                if is_type_constructor_call(name, ty) {
                    self.write(&self.format_type(ty));
                } else {
                    self.write(&sanitize_identifier(wgsl_builtin_name(name)));
                }
                self.write("(");
                // arrayLength in WGSL takes a pointer argument: arrayLength(&buf)
                let needs_addr_of = *name == "arrayLength";
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        self.write(", ");
                    }
                    if needs_addr_of && i == 0 {
                        self.write("&");
                    }
                    self.emit_expr_prec(arg, 0);
                }
                self.write(")");
            }
            MirExpr::ConstructStruct(name, fields) => {
                self.write(&sanitize_identifier(name));
                self.write("(");
                for (i, field) in fields.iter().enumerate() {
                    if i > 0 {
                        self.write(", ");
                    }
                    self.emit_expr_prec(field, 0);
                }
                self.write(")");
            }
            MirExpr::FieldAccess(base, field, _) => {
                self.emit_expr_prec(base, PREC_POSTFIX);
                self.write(&format!(".{}", sanitize_identifier(field)));
            }
            MirExpr::Index(array, index, _) => {
                self.emit_expr_prec(array, PREC_POSTFIX);
                self.write("[");
                self.emit_expr_prec(index, 0);
                self.write("]");
            }
            MirExpr::Cast(inner, ty) => {
                self.write(&format!("{}(", self.format_type(ty)));
                self.emit_expr_prec(inner, 0);
                self.write(")");
            }
        }
    }

    // -----------------------------------------------------------------------
    // Literal emission
    // -----------------------------------------------------------------------

    fn emit_lit(&mut self, lit: &MirLit) {
        match lit {
            MirLit::I32(v) => {
                if *v < 0 {
                    // Wrap negative literals in parentheses for safety
                    self.write(&format!("({}i)", v));
                } else {
                    self.write(&format!("{}i", v));
                }
            }
            MirLit::U32(v) => self.write(&format!("{}u", v)),
            MirLit::F32(v) => {
                let s = format!("{}", v);
                if s.contains('.') {
                    self.write(&s);
                } else {
                    self.write(&format!("{}.0", s));
                }
            }
            MirLit::Bool(v) => self.write(if *v { "true" } else { "false" }),
        }
    }

    // -----------------------------------------------------------------------
    // Type formatting
    // -----------------------------------------------------------------------

    fn format_type(&self, ty: &MirType) -> String {
        ty.to_string()
    }

    // -----------------------------------------------------------------------
    // Buffer helpers
    // -----------------------------------------------------------------------

    fn write(&mut self, s: &str) {
        self.output.push_str(s);
    }

    fn write_indent(&mut self) {
        for _ in 0..self.indent {
            self.output.push_str("  ");
        }
    }

    fn newline(&mut self) {
        self.output.push('\n');
    }
}

fn is_type_constructor_call(name: &str, ty: &MirType) -> bool {
    matches!(
        (name, ty),
        ("vec2", MirType::Vec(2, _))
            | ("vec3", MirType::Vec(3, _))
            | ("vec4", MirType::Vec(4, _))
            | ("mat2x2", MirType::Mat(2, 2, _))
            | ("mat3x3", MirType::Mat(3, 3, _))
            | ("mat4x4", MirType::Mat(4, 4, _))
    )
}

/// Convenience function to emit a [`MirProgram`] as WGSL source text.
pub fn emit_wgsl(program: &MirProgram) -> String {
    if let Err(errors) = shadml_mir::validate::validate_program(program) {
        panic!(
            "attempted to emit invalid MIR as WGSL:\n{}",
            errors.join("\n")
        );
    }
    WgslEmitter::new().emit_program(program)
}

/// Emit a [`MirProgram`] as WGSL, preserving source comments.
pub fn emit_wgsl_with_comments(program: &MirProgram) -> String {
    if let Err(errors) = shadml_mir::validate::validate_program(program) {
        panic!(
            "attempted to emit invalid MIR as WGSL:\n{}",
            errors.join("\n")
        );
    }
    WgslEmitter::with_comments().emit_program(program)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use shadml_allocator::Allocator;

    #[test]
    fn test_emit_simple_function() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![MirFunction {
                name: arena.alloc_str("add"),
                params: vec![
                    MirParam {
                        name: arena.alloc_str("x"),
                        ty: MirType::I32,
                    },
                    MirParam {
                        name: arena.alloc_str("y"),
                        ty: MirType::I32,
                    },
                ],
                return_ty: MirType::I32,
                body: vec![],
                return_expr: Some(MirExpr::BinOp(
                    MirBinOp::Add,
                    arena.alloc(MirExpr::Var(arena.alloc_str("x"), MirType::I32)),
                    arena.alloc(MirExpr::Var(arena.alloc_str("y"), MirType::I32)),
                    MirType::I32,
                )),
                comments: vec![],
            }],
            entry_points: vec![],
            constants: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(wgsl.contains("fn add(x: i32, y: i32) -> i32"));
        assert!(wgsl.contains("return x + y;"));
    }

    #[test]
    fn test_emit_void_function() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![MirFunction {
                name: arena.alloc_str("do_nothing"),
                params: vec![],
                return_ty: MirType::Unit,
                body: vec![],
                return_expr: None,
                comments: vec![],
            }],
            entry_points: vec![],
            constants: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(wgsl.contains("fn do_nothing() {"));
        assert!(!wgsl.contains("->"));
    }

    #[test]
    fn test_emit_struct() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![MirStruct {
                name: arena.alloc_str("Particle"),
                fields: vec![
                    MirField {
                        name: arena.alloc_str("tag"),
                        ty: MirType::U32,
                        attributes: vec![],
                    },
                    MirField {
                        name: arena.alloc_str("position"),
                        ty: MirType::Vec(3, arena.alloc(MirType::F32)),
                        attributes: vec![],
                    },
                    MirField {
                        name: arena.alloc_str("life"),
                        ty: MirType::F32,
                        attributes: vec![],
                    },
                ],
            }],
            globals: vec![],
            functions: vec![],
            entry_points: vec![],
            constants: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(wgsl.contains("struct Particle {"));
        assert!(wgsl.contains("tag: u32,"));
        assert!(wgsl.contains("position: vec3<f32>,"));
        assert!(wgsl.contains("life: f32,"));
    }

    #[test]
    fn test_emit_compute_entry_point() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![MirStruct {
                name: arena.alloc_str("ComputeInput"),
                fields: vec![MirField {
                    name: arena.alloc_str("gid"),
                    ty: MirType::Vec(3, arena.alloc(MirType::U32)),
                    attributes: vec![MirAttribute {
                        name: arena.alloc_str("builtin"),
                        args: vec![arena.alloc_str("global_invocation_id")],
                    }],
                }],
            }],
            globals: vec![],
            functions: vec![],
            entry_points: vec![MirEntryPoint {
                name: arena.alloc_str("main"),
                stage: ShaderStage::Compute,
                workgroup_size: Some([64, 1, 1]),
                params: vec![MirParam {
                    name: arena.alloc_str("input"),
                    ty: MirType::Struct(arena.alloc_str("ComputeInput")),
                }],
                return_ty: MirType::Unit,
                body: vec![MirStmt::Let(
                    arena.alloc_str("idx"),
                    MirType::U32,
                    MirExpr::FieldAccess(
                        arena.alloc(MirExpr::FieldAccess(
                            arena.alloc(MirExpr::Var(
                                arena.alloc_str("input"),
                                MirType::Struct(arena.alloc_str("ComputeInput")),
                            )),
                            arena.alloc_str("gid"),
                            MirType::Vec(3, arena.alloc(MirType::U32)),
                        )),
                        arena.alloc_str("x"),
                        MirType::U32,
                    ),
                )],
                return_expr: None,
                comments: vec![],
            }],
            constants: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(wgsl.contains("@compute @workgroup_size(64, 1, 1)"));
        assert!(wgsl.contains("input: ComputeInput"));
        assert!(wgsl.contains("@builtin(global_invocation_id) gid: vec3<u32>"));
    }

    #[test]
    fn test_emit_vertex_entry_point() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![],
            entry_points: vec![MirEntryPoint {
                name: arena.alloc_str("vs_main"),
                stage: ShaderStage::Vertex,
                workgroup_size: None,
                params: vec![],
                return_ty: MirType::Vec(4, arena.alloc(MirType::F32)),
                body: vec![],
                return_expr: Some(MirExpr::Call(
                    arena.alloc_str("vec4"),
                    vec![
                        MirExpr::Lit(MirLit::F32(0.0)),
                        MirExpr::Lit(MirLit::F32(0.0)),
                        MirExpr::Lit(MirLit::F32(0.0)),
                        MirExpr::Lit(MirLit::F32(1.0)),
                    ],
                    MirType::Vec(4, arena.alloc(MirType::F32)),
                )),
                comments: vec![],
            }],
            constants: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(wgsl.contains("@vertex"));
        assert!(wgsl.contains("fn vs_main()"));
        assert!(wgsl.contains("-> @location(0) vec4<f32>"));
        assert!(wgsl.contains("return vec4<f32>(0.0, 0.0, 0.0, 1.0);"));
    }

    #[test]
    fn test_emit_fragment_entry_point() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![],
            entry_points: vec![MirEntryPoint {
                name: arena.alloc_str("fs_main"),
                stage: ShaderStage::Fragment,
                workgroup_size: None,
                params: vec![],
                return_ty: MirType::Vec(4, arena.alloc(MirType::F32)),
                body: vec![],
                return_expr: Some(MirExpr::Call(
                    arena.alloc_str("vec4"),
                    vec![
                        MirExpr::Lit(MirLit::F32(1.0)),
                        MirExpr::Lit(MirLit::F32(0.0)),
                        MirExpr::Lit(MirLit::F32(0.0)),
                        MirExpr::Lit(MirLit::F32(1.0)),
                    ],
                    MirType::Vec(4, arena.alloc(MirType::F32)),
                )),
                comments: vec![],
            }],
            constants: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(wgsl.contains("@fragment"));
        assert!(wgsl.contains("fn fs_main()"));
        assert!(wgsl.contains("-> @location(0) vec4<f32>"));
    }

    #[test]
    fn test_emit_if_statement() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![MirFunction {
                name: arena.alloc_str("abs_val"),
                params: vec![MirParam {
                    name: arena.alloc_str("x"),
                    ty: MirType::I32,
                }],
                return_ty: MirType::I32,
                body: vec![MirStmt::If(
                    MirExpr::BinOp(
                        MirBinOp::Lt,
                        arena.alloc(MirExpr::Var(arena.alloc_str("x"), MirType::I32)),
                        arena.alloc(MirExpr::Lit(MirLit::I32(0))),
                        MirType::Bool,
                    ),
                    vec![MirStmt::Return(MirExpr::UnaryOp(
                        MirUnaryOp::Neg,
                        arena.alloc(MirExpr::Var(arena.alloc_str("x"), MirType::I32)),
                        MirType::I32,
                    ))],
                    vec![MirStmt::Return(MirExpr::Var(
                        arena.alloc_str("x"),
                        MirType::I32,
                    ))],
                )],
                return_expr: None,
                comments: vec![],
            }],
            entry_points: vec![],
            constants: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(wgsl.contains("if ("));
        assert!(wgsl.contains("} else {"));
        assert!(wgsl.contains("return -x;"));
        assert!(wgsl.contains("return x;"));
    }

    #[test]
    fn test_emit_if_without_else() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![MirFunction {
                name: arena.alloc_str("maybe_inc"),
                params: vec![MirParam {
                    name: arena.alloc_str("x"),
                    ty: MirType::I32,
                }],
                return_ty: MirType::Unit,
                body: vec![
                    MirStmt::Var(
                        arena.alloc_str("r"),
                        MirType::I32,
                        MirExpr::Var(arena.alloc_str("x"), MirType::I32),
                    ),
                    MirStmt::If(
                        MirExpr::BinOp(
                            MirBinOp::Gt,
                            arena.alloc(MirExpr::Var(arena.alloc_str("x"), MirType::I32)),
                            arena.alloc(MirExpr::Lit(MirLit::I32(0))),
                            MirType::Bool,
                        ),
                        vec![MirStmt::Assign(
                            arena.alloc_str("r"),
                            MirExpr::BinOp(
                                MirBinOp::Add,
                                arena.alloc(MirExpr::Var(arena.alloc_str("r"), MirType::I32)),
                                arena.alloc(MirExpr::Lit(MirLit::I32(1))),
                                MirType::I32,
                            ),
                        )],
                        vec![],
                    ),
                ],
                return_expr: None,
                comments: vec![],
            }],
            entry_points: vec![],
            constants: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(wgsl.contains("var r: i32 = x;"));
        assert!(wgsl.contains("if ("));
        assert!(!wgsl.contains("else"));
    }

    #[test]
    fn test_emit_let_and_var() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![MirFunction {
                name: arena.alloc_str("f"),
                params: vec![],
                return_ty: MirType::Unit,
                body: vec![
                    MirStmt::Let(
                        arena.alloc_str("a"),
                        MirType::I32,
                        MirExpr::Lit(MirLit::I32(42)),
                    ),
                    MirStmt::Var(
                        arena.alloc_str("b"),
                        MirType::F32,
                        MirExpr::Lit(MirLit::F32(3.14)),
                    ),
                ],
                return_expr: None,
                comments: vec![],
            }],
            entry_points: vec![],
            constants: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(wgsl.contains("let a = 42i;"));
        assert!(wgsl.contains("var b: f32 = 3.14;"));
    }

    #[test]
    fn test_emit_literals() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![MirFunction {
                name: arena.alloc_str("lits"),
                params: vec![],
                return_ty: MirType::Unit,
                body: vec![
                    MirStmt::Let(
                        arena.alloc_str("a"),
                        MirType::I32,
                        MirExpr::Lit(MirLit::I32(-5)),
                    ),
                    MirStmt::Let(
                        arena.alloc_str("b"),
                        MirType::U32,
                        MirExpr::Lit(MirLit::U32(10)),
                    ),
                    MirStmt::Let(
                        arena.alloc_str("c"),
                        MirType::F32,
                        MirExpr::Lit(MirLit::F32(2.0)),
                    ),
                    MirStmt::Let(
                        arena.alloc_str("d"),
                        MirType::Bool,
                        MirExpr::Lit(MirLit::Bool(true)),
                    ),
                ],
                return_expr: None,
                comments: vec![],
            }],
            entry_points: vec![],
            constants: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(wgsl.contains("(-5i)"));
        assert!(wgsl.contains("10u"));
        assert!(wgsl.contains("2.0"));
        assert!(wgsl.contains("true"));
    }

    #[test]
    fn test_emit_call_expression() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![MirFunction {
                name: arena.alloc_str("f"),
                params: vec![],
                return_ty: MirType::F32,
                body: vec![],
                return_expr: Some(MirExpr::Call(
                    arena.alloc_str("max"),
                    vec![
                        MirExpr::Lit(MirLit::F32(1.0)),
                        MirExpr::Lit(MirLit::F32(2.0)),
                    ],
                    MirType::F32,
                )),
                comments: vec![],
            }],
            entry_points: vec![],
            constants: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(wgsl.contains("return max(1.0, 2.0);"));
    }

    #[test]
    fn test_emit_struct_construction() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![MirStruct {
                name: arena.alloc_str("Vec2"),
                fields: vec![
                    MirField {
                        name: arena.alloc_str("x"),
                        ty: MirType::F32,
                        attributes: vec![],
                    },
                    MirField {
                        name: arena.alloc_str("y"),
                        ty: MirType::F32,
                        attributes: vec![],
                    },
                ],
            }],
            globals: vec![],
            functions: vec![MirFunction {
                name: arena.alloc_str("make_vec"),
                params: vec![],
                return_ty: MirType::Struct(arena.alloc_str("Vec2")),
                body: vec![],
                return_expr: Some(MirExpr::ConstructStruct(
                    arena.alloc_str("Vec2"),
                    vec![
                        MirExpr::Lit(MirLit::F32(1.0)),
                        MirExpr::Lit(MirLit::F32(2.0)),
                    ],
                )),
                comments: vec![],
            }],
            entry_points: vec![],
            constants: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(wgsl.contains("return Vec2(1.0, 2.0);"));
    }

    #[test]
    fn test_emit_field_access() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![MirFunction {
                name: arena.alloc_str("get_x"),
                params: vec![MirParam {
                    name: arena.alloc_str("v"),
                    ty: MirType::Vec(3, arena.alloc(MirType::F32)),
                }],
                return_ty: MirType::F32,
                body: vec![],
                return_expr: Some(MirExpr::FieldAccess(
                    arena.alloc(MirExpr::Var(
                        arena.alloc_str("v"),
                        MirType::Vec(3, arena.alloc(MirType::F32)),
                    )),
                    arena.alloc_str("x"),
                    MirType::F32,
                )),
                comments: vec![],
            }],
            entry_points: vec![],
            constants: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(wgsl.contains("return v.x;"));
    }

    #[test]
    fn test_emit_index_access() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![MirFunction {
                name: arena.alloc_str("get_elem"),
                params: vec![MirParam {
                    name: arena.alloc_str("arr"),
                    ty: MirType::Array(arena.alloc(MirType::F32), 4),
                }],
                return_ty: MirType::F32,
                body: vec![],
                return_expr: Some(MirExpr::Index(
                    arena.alloc(MirExpr::Var(
                        arena.alloc_str("arr"),
                        MirType::Array(arena.alloc(MirType::F32), 4),
                    )),
                    arena.alloc(MirExpr::Lit(MirLit::U32(0))),
                    MirType::F32,
                )),
                comments: vec![],
            }],
            entry_points: vec![],
            constants: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(wgsl.contains("return arr[0u];"));
    }

    #[test]
    fn test_emit_cast() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![MirFunction {
                name: arena.alloc_str("to_float"),
                params: vec![MirParam {
                    name: arena.alloc_str("x"),
                    ty: MirType::I32,
                }],
                return_ty: MirType::F32,
                body: vec![],
                return_expr: Some(MirExpr::Cast(
                    arena.alloc(MirExpr::Var(arena.alloc_str("x"), MirType::I32)),
                    MirType::F32,
                )),
                comments: vec![],
            }],
            entry_points: vec![],
            constants: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(wgsl.contains("return f32(x);"));
    }

    #[test]
    fn test_emit_block_statement() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![MirFunction {
                name: arena.alloc_str("f"),
                params: vec![],
                return_ty: MirType::Unit,
                body: vec![MirStmt::Block(vec![MirStmt::Let(
                    arena.alloc_str("x"),
                    MirType::I32,
                    MirExpr::Lit(MirLit::I32(1)),
                )])],
                return_expr: None,
                comments: vec![],
            }],
            entry_points: vec![],
            constants: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(wgsl.contains("  {\n    let x = 1i;\n  }"));
    }

    #[test]
    fn test_emit_mat_type() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![MirFunction {
                name: arena.alloc_str("identity"),
                params: vec![],
                return_ty: MirType::Mat(4, 4, arena.alloc(MirType::F32)),
                body: vec![],
                return_expr: Some(MirExpr::Call(
                    arena.alloc_str("mat4x4"),
                    vec![],
                    MirType::Mat(4, 4, arena.alloc(MirType::F32)),
                )),
                comments: vec![],
            }],
            entry_points: vec![],
            constants: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(wgsl.contains("-> mat4x4<f32>"));
    }

    #[test]
    fn test_emit_array_type() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![MirStruct {
                name: arena.alloc_str("Data"),
                fields: vec![MirField {
                    name: arena.alloc_str("values"),
                    ty: MirType::Array(arena.alloc(MirType::F32), 16),
                    attributes: vec![],
                }],
            }],
            globals: vec![],
            functions: vec![],
            entry_points: vec![],
            constants: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(wgsl.contains("values: array<f32, 16>,"));
    }

    #[test]
    fn test_emit_compute_default_workgroup() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![],
            entry_points: vec![MirEntryPoint {
                name: arena.alloc_str("main"),
                stage: ShaderStage::Compute,
                workgroup_size: None,
                params: vec![],
                return_ty: MirType::Unit,
                body: vec![],
                return_expr: None,
                comments: vec![],
            }],
            constants: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(wgsl.contains("@compute @workgroup_size(1, 1, 1)"));
    }

    #[test]
    fn test_emit_full_program() {
        let arena = Allocator::new();
        // A complete small program: struct + helper function + compute entry point
        let program = MirProgram {
            structs: vec![MirStruct {
                name: arena.alloc_str("Particle"),
                fields: vec![
                    MirField {
                        name: arena.alloc_str("pos"),
                        ty: MirType::Vec(3, arena.alloc(MirType::F32)),
                        attributes: vec![],
                    },
                    MirField {
                        name: arena.alloc_str("vel"),
                        ty: MirType::Vec(3, arena.alloc(MirType::F32)),
                        attributes: vec![],
                    },
                ],
            }],
            globals: vec![],
            functions: vec![MirFunction {
                name: arena.alloc_str("step_particle"),
                params: vec![
                    MirParam {
                        name: arena.alloc_str("p"),
                        ty: MirType::Struct(arena.alloc_str("Particle")),
                    },
                    MirParam {
                        name: arena.alloc_str("dt"),
                        ty: MirType::F32,
                    },
                ],
                return_ty: MirType::Struct(arena.alloc_str("Particle")),
                body: vec![MirStmt::Let(
                    arena.alloc_str("new_pos"),
                    MirType::Vec(3, arena.alloc(MirType::F32)),
                    MirExpr::BinOp(
                        MirBinOp::Add,
                        arena.alloc(MirExpr::FieldAccess(
                            arena.alloc(MirExpr::Var(
                                arena.alloc_str("p"),
                                MirType::Struct(arena.alloc_str("Particle")),
                            )),
                            arena.alloc_str("pos"),
                            MirType::Vec(3, arena.alloc(MirType::F32)),
                        )),
                        arena.alloc(MirExpr::BinOp(
                            MirBinOp::Mul,
                            arena.alloc(MirExpr::FieldAccess(
                                arena.alloc(MirExpr::Var(
                                    arena.alloc_str("p"),
                                    MirType::Struct(arena.alloc_str("Particle")),
                                )),
                                arena.alloc_str("vel"),
                                MirType::Vec(3, arena.alloc(MirType::F32)),
                            )),
                            arena.alloc(MirExpr::Var(arena.alloc_str("dt"), MirType::F32)),
                            MirType::Vec(3, arena.alloc(MirType::F32)),
                        )),
                        MirType::Vec(3, arena.alloc(MirType::F32)),
                    ),
                )],
                return_expr: Some(MirExpr::ConstructStruct(
                    arena.alloc_str("Particle"),
                    vec![
                        MirExpr::Var(
                            arena.alloc_str("new_pos"),
                            MirType::Vec(3, arena.alloc(MirType::F32)),
                        ),
                        MirExpr::FieldAccess(
                            arena.alloc(MirExpr::Var(
                                arena.alloc_str("p"),
                                MirType::Struct(arena.alloc_str("Particle")),
                            )),
                            arena.alloc_str("vel"),
                            MirType::Vec(3, arena.alloc(MirType::F32)),
                        ),
                    ],
                )),
                comments: vec![],
            }],
            entry_points: vec![MirEntryPoint {
                name: arena.alloc_str("main"),
                stage: ShaderStage::Compute,
                workgroup_size: Some([256, 1, 1]),
                params: vec![],
                return_ty: MirType::Unit,
                body: vec![MirStmt::Let(
                    arena.alloc_str("idx"),
                    MirType::U32,
                    MirExpr::FieldAccess(
                        arena.alloc(MirExpr::Var(
                            arena.alloc_str("gid"),
                            MirType::Vec(3, arena.alloc(MirType::U32)),
                        )),
                        arena.alloc_str("x"),
                        MirType::U32,
                    ),
                )],
                return_expr: None,
                comments: vec![],
            }],
            constants: vec![],
        };

        let wgsl = emit_wgsl(&program);

        // Check that all major sections are present
        assert!(wgsl.contains("struct Particle {"));
        assert!(wgsl.contains("fn step_particle("));
        assert!(wgsl.contains("@compute @workgroup_size(256, 1, 1)"));
        assert!(wgsl.contains("fn main("));

        // Check that the output is properly ordered:
        // structs before functions, functions before entry points
        let struct_pos = wgsl.find("struct Particle").unwrap();
        let fn_pos = wgsl.find("fn step_particle").unwrap();
        let ep_pos = wgsl.find("@compute").unwrap();
        assert!(struct_pos < fn_pos);
        assert!(fn_pos < ep_pos);
    }

    #[test]
    fn test_emit_struct_field_attributes() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![MirStruct {
                name: arena.alloc_str("VertexOutput"),
                fields: vec![
                    MirField {
                        name: arena.alloc_str("clip_position"),
                        ty: MirType::Vec(4, arena.alloc(MirType::F32)),
                        attributes: vec![MirAttribute {
                            name: arena.alloc_str("builtin"),
                            args: vec![arena.alloc_str("position")],
                        }],
                    },
                    MirField {
                        name: arena.alloc_str("color"),
                        ty: MirType::Vec(4, arena.alloc(MirType::F32)),
                        attributes: vec![MirAttribute {
                            name: arena.alloc_str("location"),
                            args: vec![arena.alloc_str("0")],
                        }],
                    },
                    MirField {
                        name: arena.alloc_str("uv"),
                        ty: MirType::Vec(2, arena.alloc(MirType::F32)),
                        attributes: vec![
                            MirAttribute {
                                name: arena.alloc_str("location"),
                                args: vec![arena.alloc_str("1")],
                            },
                            MirAttribute {
                                name: arena.alloc_str("interpolate"),
                                args: vec![arena.alloc_str("linear"), arena.alloc_str("center")],
                            },
                        ],
                    },
                ],
            }],
            globals: vec![],
            functions: vec![],
            entry_points: vec![],
            constants: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(
            wgsl.contains("@builtin(position) clip_position: vec4<f32>,"),
            "expected @builtin(position) attribute, got:\n{}",
            wgsl
        );
        assert!(
            wgsl.contains("@location(0) color: vec4<f32>,"),
            "expected @location(0) attribute, got:\n{}",
            wgsl
        );
        assert!(
            wgsl.contains("@location(1) @interpolate(linear, center) uv: vec2<f32>,"),
            "expected multiple attributes, got:\n{}",
            wgsl
        );
    }

    #[test]
    fn test_emit_switch_statement() {
        let arena = Allocator::new();
        // Build a function that contains a switch on a u32 variable
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![MirFunction {
                name: arena.alloc_str("classify"),
                params: vec![MirParam {
                    name: arena.alloc_str("x"),
                    ty: MirType::U32,
                }],
                return_ty: MirType::I32,
                body: vec![
                    MirStmt::Var(
                        arena.alloc_str("result"),
                        MirType::I32,
                        MirExpr::Lit(MirLit::I32(0)),
                    ),
                    MirStmt::Switch(
                        MirExpr::Var(arena.alloc_str("x"), MirType::U32),
                        vec![
                            MirSwitchCase {
                                values: vec![MirLit::U32(0)],
                                body: vec![MirStmt::Assign(
                                    arena.alloc_str("result"),
                                    MirExpr::Lit(MirLit::I32(10)),
                                )],
                            },
                            MirSwitchCase {
                                values: vec![MirLit::U32(1)],
                                body: vec![MirStmt::Assign(
                                    arena.alloc_str("result"),
                                    MirExpr::Lit(MirLit::I32(20)),
                                )],
                            },
                            MirSwitchCase {
                                values: vec![MirLit::U32(2)],
                                body: vec![MirStmt::Assign(
                                    arena.alloc_str("result"),
                                    MirExpr::Lit(MirLit::I32(30)),
                                )],
                            },
                        ],
                        // default body
                        vec![MirStmt::Assign(
                            arena.alloc_str("result"),
                            MirExpr::Lit(MirLit::I32(-1)),
                        )],
                    ),
                ],
                return_expr: Some(MirExpr::Var(arena.alloc_str("result"), MirType::I32)),
                comments: vec![],
            }],
            entry_points: vec![],
            constants: vec![],
        };

        let wgsl = emit_wgsl(&program);
        assert!(
            wgsl.contains("switch (x)"),
            "expected switch statement, got:\n{}",
            wgsl
        );
        assert!(
            wgsl.contains("case 0u:"),
            "expected case 0u, got:\n{}",
            wgsl
        );
        assert!(
            wgsl.contains("case 1u:"),
            "expected case 1u, got:\n{}",
            wgsl
        );
        assert!(
            wgsl.contains("case 2u:"),
            "expected case 2u, got:\n{}",
            wgsl
        );
        assert!(
            wgsl.contains("default:"),
            "expected default case, got:\n{}",
            wgsl
        );
        assert!(
            wgsl.contains("result = 10i;"),
            "expected result = 10i in case body, got:\n{}",
            wgsl
        );
        assert!(
            wgsl.contains("result = (-1i);"),
            "expected result = (-1i) in default body, got:\n{}",
            wgsl
        );
    }
}
