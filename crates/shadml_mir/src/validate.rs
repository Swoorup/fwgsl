use std::collections::HashSet;

use crate::{MirExpr, MirProgram, MirStmt, MirType};

pub fn validate_program(program: &MirProgram<'_>) -> Result<(), Vec<String>> {
    let function_names: HashSet<&str> = program.functions.iter().map(|f| f.name).collect();
    let constant_names: HashSet<&str> = program.constants.iter().map(|c| c.name).collect();
    let entry_point_names: HashSet<&str> = program.entry_points.iter().map(|ep| ep.name).collect();
    let global_names: HashSet<&str> = program.globals.iter().map(|g| g.name).collect();
    let mut errors = Vec::new();

    for c in &program.constants {
        validate_expr(
            &c.value,
            &function_names,
            &constant_names,
            &mut errors,
            &format!("const '{}'", c.name),
        );
    }

    for f in &program.functions {
        validate_stmts(
            &f.body,
            &function_names,
            &constant_names,
            &mut errors,
            &format!("function '{}'", f.name),
        );
        if let Some(expr) = &f.return_expr {
            validate_expr(
                expr,
                &function_names,
                &constant_names,
                &mut errors,
                &format!("function '{}' return", f.name),
            );
        }
    }

    for ep in &program.entry_points {
        validate_stmts(
            &ep.body,
            &function_names,
            &constant_names,
            &mut errors,
            &format!("entry point '{}'", ep.name),
        );
        if let Some(expr) = &ep.return_expr {
            validate_expr(
                expr,
                &function_names,
                &constant_names,
                &mut errors,
                &format!("entry point '{}' return", ep.name),
            );
        }
    }

    // Validate render blocks
    for rb in &program.render_blocks {
        if !rb.vertex_entry.is_empty() && !entry_point_names.contains(rb.vertex_entry) {
            errors.push(format!(
                "render block '{}' references unknown vertex entry point '{}'",
                rb.name, rb.vertex_entry
            ));
        }
        if !rb.fragment_entry.is_empty() && !entry_point_names.contains(rb.fragment_entry) {
            errors.push(format!(
                "render block '{}' references unknown fragment entry point '{}'",
                rb.name, rb.fragment_entry
            ));
        }
        for binding_name in &rb.binding_names {
            if !global_names.contains(binding_name) {
                errors.push(format!(
                    "render block '{}' references unknown binding '{}'",
                    rb.name, binding_name
                ));
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn validate_stmts(
    stmts: &[MirStmt<'_>],
    function_names: &HashSet<&str>,
    constant_names: &HashSet<&str>,
    errors: &mut Vec<String>,
    context: &str,
) {
    for stmt in stmts {
        match stmt {
            MirStmt::Let(_, _, expr)
            | MirStmt::Var(_, _, expr)
            | MirStmt::Assign(_, expr)
            | MirStmt::Return(expr) => {
                validate_expr(expr, function_names, constant_names, errors, context);
            }
            MirStmt::IndexAssign(base, index, value) => {
                validate_expr(base, function_names, constant_names, errors, context);
                validate_expr(index, function_names, constant_names, errors, context);
                validate_expr(value, function_names, constant_names, errors, context);
            }
            MirStmt::If(cond, then_stmts, else_stmts) => {
                validate_expr(cond, function_names, constant_names, errors, context);
                validate_stmts(then_stmts, function_names, constant_names, errors, context);
                validate_stmts(else_stmts, function_names, constant_names, errors, context);
            }
            MirStmt::Block(stmts) | MirStmt::Loop(stmts) => {
                validate_stmts(stmts, function_names, constant_names, errors, context);
            }
            MirStmt::Switch(expr, cases, default) => {
                validate_expr(expr, function_names, constant_names, errors, context);
                for case in cases {
                    validate_stmts(&case.body, function_names, constant_names, errors, context);
                }
                validate_stmts(default, function_names, constant_names, errors, context);
            }
            MirStmt::Break | MirStmt::Continue => {}
        }
    }
}

fn validate_expr(
    expr: &MirExpr<'_>,
    function_names: &HashSet<&str>,
    constant_names: &HashSet<&str>,
    errors: &mut Vec<String>,
    context: &str,
) {
    match expr {
        MirExpr::Lit(_) | MirExpr::Var(_, _) => {}
        MirExpr::BinOp(_, lhs, rhs, _) => {
            validate_expr(lhs, function_names, constant_names, errors, context);
            validate_expr(rhs, function_names, constant_names, errors, context);
        }
        MirExpr::UnaryOp(_, operand, _) | MirExpr::Cast(operand, _) => {
            validate_expr(operand, function_names, constant_names, errors, context);
        }
        MirExpr::Call(name, args, ty) => {
            for arg in args {
                validate_expr(arg, function_names, constant_names, errors, context);
            }

            if constant_names.contains(name) {
                errors.push(format!(
                    "{} contains invalid call to constant '{}'; promoted/module constants must be referenced as values, not called",
                    context, name
                ));
            } else if !function_names.contains(name) && !is_allowed_intrinsic_call(name, ty) {
                errors.push(format!(
                    "{} contains unresolved call target '{}'",
                    context, name
                ));
            }
        }
        MirExpr::ConstructStruct(_, fields) => {
            for field in fields {
                validate_expr(field, function_names, constant_names, errors, context);
            }
        }
        MirExpr::FieldAccess(base, _, _) => {
            validate_expr(base, function_names, constant_names, errors, context);
        }
        MirExpr::Index(base, index, _) => {
            validate_expr(base, function_names, constant_names, errors, context);
            validate_expr(index, function_names, constant_names, errors, context);
        }
    }
}

fn is_allowed_intrinsic_call(name: &str, ty: &MirType<'_>) -> bool {
    is_type_constructor_call(name, ty)
        || matches!(
            name,
            "sin"
                | "cos"
                | "tan"
                | "abs"
                | "fract"
                | "floor"
                | "sign"
                | "sqrt"
                | "log"
                | "log2"
                | "exp"
                | "exp2"
                | "ceil"
                | "round"
                | "trunc"
                | "negate"
                | "saturate"
                | "inverseSqrt"
                | "asin"
                | "acos"
                | "sinh"
                | "cosh"
                | "tanh"
                | "asinh"
                | "acosh"
                | "atanh"
                | "radians"
                | "degrees"
                | "max"
                | "min"
                | "step"
                | "mod"
                | "pow"
                | "reflect"
                | "atan"
                | "atan2"
                | "ldexp"
                | "clamp"
                | "mix"
                | "smoothstep"
                | "fma"
                | "normalize"
                | "length"
                | "dot"
                | "distance"
                | "cross"
                | "faceForward"
                | "refract"
                | "select"
                | "determinant"
                | "transpose"
                | "all"
                | "any"
                | "unpack4x8unorm"
                | "pack4x8unorm"
                | "unpack4x8snorm"
                | "pack4x8snorm"
                | "unpack2x16float"
                | "pack2x16float"
                | "unpack2x16unorm"
                | "pack2x16unorm"
                | "unpack2x16snorm"
                | "pack2x16snorm"
                | "countOneBits"
                | "countLeadingZeros"
                | "countTrailingZeros"
                | "reverseBits"
                | "firstTrailingBit"
                | "firstLeadingBit"
                | "extractBits"
                | "insertBits"
                | "dpdx"
                | "dpdy"
                | "dpdxCoarse"
                | "dpdxFine"
                | "dpdyCoarse"
                | "dpdyFine"
                | "fwidth"
                | "fwidthCoarse"
                | "fwidthFine"
                | "storageBarrier"
                | "workgroupBarrier"
                | "textureSample"
                | "textureSampleArray"
                | "textureLoad"
                | "textureLoadMsaa"
                | "textureLoadArray"
                | "textureStore"
                | "textureDimensions"
                | "textureDimensionsMsaa"
                | "textureDimensionsArray"
                | "atomicLoad"
                | "atomicStore"
                | "atomicAdd"
                | "atomicSub"
                | "atomicMax"
                | "atomicMin"
                | "atomicAnd"
                | "atomicOr"
                | "atomicXor"
                | "atomicExchange"
                | "arrayLength"
                | "load"
                | "toF32"
                | "toI32"
                | "toU32"
                | "toBool"
                | "toF16"
                | "writeAt"
        )
}

fn is_type_constructor_call(name: &str, ty: &MirType<'_>) -> bool {
    match ty {
        MirType::Vec(n, _) => matches!((name, *n), ("vec2", 2) | ("vec3", 3) | ("vec4", 4)),
        MirType::Mat(cols, rows, _) => matches!(
            (name, *cols, *rows),
            ("mat2x2", 2, 2)
                | ("mat2x3", 2, 3)
                | ("mat2x4", 2, 4)
                | ("mat3x2", 3, 2)
                | ("mat3x3", 3, 3)
                | ("mat3x4", 3, 4)
                | ("mat4x2", 4, 2)
                | ("mat4x3", 4, 3)
                | ("mat4x4", 4, 4)
        ),
        MirType::Struct(struct_name) => name == *struct_name,
        MirType::Array(_, _) | MirType::RuntimeArray(_) => name == ty.to_string(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use shadml_allocator::Allocator;

    use crate::{
        AddressSpace, MirConst, MirEntryPoint, MirExpr, MirFunction, MirGlobal, MirLit, MirProgram,
        MirRenderBlock, MirStmt, MirType, ShaderStage,
    };

    use super::validate_program;

    #[test]
    fn rejects_call_to_promoted_const() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![],
            constants: vec![MirConst {
                name: "maxLights",
                ty: MirType::I32,
                value: MirExpr::Lit(MirLit::I32(64)),
            }],
            entry_points: vec![MirEntryPoint {
                name: "main",
                stage: ShaderStage::Compute,
                workgroup_size: Some([1, 1, 1]),
                params: vec![],
                return_ty: MirType::Unit,
                body: vec![MirStmt::Let(
                    "x",
                    MirType::I32,
                    MirExpr::Call("maxLights", vec![], MirType::I32),
                )],
                return_expr: None,
                comments: vec![],
            }],
            render_blocks: vec![],
        };

        let errors = validate_program(&program).expect_err("validator should reject const calls");
        assert!(errors
            .iter()
            .any(|e| e.contains("invalid call to constant 'maxLights'")));
        let _ = arena;
    }

    #[test]
    fn rejects_unresolved_call_target() {
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![MirFunction {
                name: "helper",
                params: vec![],
                return_ty: MirType::I32,
                body: vec![],
                return_expr: Some(MirExpr::Lit(MirLit::I32(1))),
                comments: vec![],
            }],
            constants: vec![],
            entry_points: vec![MirEntryPoint {
                name: "main",
                stage: ShaderStage::Compute,
                workgroup_size: Some([1, 1, 1]),
                params: vec![],
                return_ty: MirType::Unit,
                body: vec![MirStmt::Let(
                    "x",
                    MirType::I32,
                    MirExpr::Call("missing", vec![], MirType::I32),
                )],
                return_expr: None,
                comments: vec![],
            }],
            render_blocks: vec![],
        };

        let errors = validate_program(&program).expect_err("validator should reject missing calls");
        assert!(errors
            .iter()
            .any(|e| e.contains("unresolved call target 'missing'")));
    }

    #[test]
    fn rejects_render_block_with_unknown_vertex_entry() {
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![],
            constants: vec![],
            entry_points: vec![],
            render_blocks: vec![MirRenderBlock {
                name: "test",
                binding_names: vec![],
                vertex_entry: "missing_vertex",
                fragment_entry: "",
            }],
        };

        let errors =
            validate_program(&program).expect_err("validator should reject unknown vertex entry");
        assert!(errors
            .iter()
            .any(|e| e.contains("unknown vertex entry point 'missing_vertex'")));
    }

    #[test]
    fn rejects_render_block_with_unknown_binding() {
        let program = MirProgram {
            structs: vec![],
            globals: vec![MirGlobal {
                name: "real_binding",
                address_space: AddressSpace::Uniform,
                ty: MirType::F32,
                group: 0,
                binding: 0,
                origin_module: None,
            }],
            functions: vec![],
            constants: vec![],
            entry_points: vec![],
            render_blocks: vec![MirRenderBlock {
                name: "test",
                binding_names: vec!["missing_binding"],
                vertex_entry: "",
                fragment_entry: "",
            }],
        };

        let errors =
            validate_program(&program).expect_err("validator should reject unknown binding");
        assert!(errors
            .iter()
            .any(|e| e.contains("unknown binding 'missing_binding'")));
    }

    #[test]
    fn accepts_valid_render_block() {
        let program = MirProgram {
            structs: vec![],
            globals: vec![MirGlobal {
                name: "my_binding",
                address_space: AddressSpace::Uniform,
                ty: MirType::F32,
                group: 0,
                binding: 0,
                origin_module: None,
            }],
            functions: vec![],
            constants: vec![],
            entry_points: vec![MirEntryPoint {
                name: "vertex_main",
                stage: ShaderStage::Vertex,
                workgroup_size: None,
                params: vec![],
                return_ty: MirType::Unit,
                body: vec![],
                return_expr: None,
                comments: vec![],
            }],
            render_blocks: vec![MirRenderBlock {
                name: "test",
                binding_names: vec!["my_binding"],
                vertex_entry: "vertex_main",
                fragment_entry: "",
            }],
        };

        validate_program(&program).expect("valid render block should pass validation");
    }

    #[test]
    fn accepts_render_block_with_vertex_and_fragment() {
        let program = MirProgram {
            structs: vec![],
            globals: vec![MirGlobal {
                name: "my_uniform",
                address_space: AddressSpace::Uniform,
                ty: MirType::F32,
                group: 0,
                binding: 0,
                origin_module: None,
            }],
            functions: vec![],
            constants: vec![],
            entry_points: vec![
                MirEntryPoint {
                    name: "vs_main",
                    stage: ShaderStage::Vertex,
                    workgroup_size: None,
                    params: vec![],
                    return_ty: MirType::Unit,
                    body: vec![],
                    return_expr: None,
                    comments: vec![],
                },
                MirEntryPoint {
                    name: "fs_main",
                    stage: ShaderStage::Fragment,
                    workgroup_size: None,
                    params: vec![],
                    return_ty: MirType::Unit,
                    body: vec![],
                    return_expr: None,
                    comments: vec![],
                },
            ],
            render_blocks: vec![MirRenderBlock {
                name: "pipeline",
                binding_names: vec!["my_uniform"],
                vertex_entry: "vs_main",
                fragment_entry: "fs_main",
            }],
        };

        validate_program(&program)
            .expect("render block with vertex + fragment should pass validation");
    }

    #[test]
    fn rejects_render_block_with_unknown_fragment_entry() {
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![],
            constants: vec![],
            entry_points: vec![],
            render_blocks: vec![MirRenderBlock {
                name: "test",
                binding_names: vec![],
                vertex_entry: "",
                fragment_entry: "missing_fragment",
            }],
        };

        let errors =
            validate_program(&program).expect_err("validator should reject unknown fragment entry");
        assert!(errors
            .iter()
            .any(|e| e.contains("unknown fragment entry point 'missing_fragment'")));
    }
}
