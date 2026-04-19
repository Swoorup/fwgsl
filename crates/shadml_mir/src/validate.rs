use std::collections::HashSet;

use crate::{MirExpr, MirProgram, MirStmt, MirType};

pub fn validate_program(program: &MirProgram<'_>) -> Result<(), Vec<String>> {
    let function_names: HashSet<&str> = program.functions.iter().map(|f| f.name).collect();
    let constant_names: HashSet<&str> = program.constants.iter().map(|c| c.name).collect();
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
        MirConst, MirEntryPoint, MirExpr, MirFunction, MirLit, MirProgram, MirStmt, MirType,
        ShaderStage,
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
        };

        let errors = validate_program(&program).expect_err("validator should reject missing calls");
        assert!(errors
            .iter()
            .any(|e| e.contains("unresolved call target 'missing'")));
    }
}
