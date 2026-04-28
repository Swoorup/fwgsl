use super::*;
use shadml_allocator::Allocator;

#[test]
fn codegen_simple_add_function() {
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
            is_const: false,
        }],
        entry_points: vec![],
        constants: vec![],
        render_blocks: vec![],
    };

    let wgsl = emit_wgsl(&program);
    assert!(
        wgsl.contains("fn add(x: i32, y: i32) -> i32"),
        "WGSL: {}",
        wgsl
    );
    assert!(wgsl.contains("return x + y;"), "WGSL: {}", wgsl);
}

#[test]
fn codegen_compute_shader_entry_point() {
    let arena = Allocator::new();
    let program = MirProgram {
        structs: vec![MirStruct {
            name: arena.alloc_str("ComputeInput"),
            fields: vec![MirField {
                name: arena.alloc_str("gid"),
                ty: MirType::Vec(3, arena.alloc(MirType::U32)),
                doc: None,
                attributes: vec![MirAttribute {
                    name: arena.alloc_str("builtin"),
                    args: vec![arena.alloc_str("global_invocation_id")],
                }],
            }],
            comments: vec![],
            origin_module: None,
            adt_variants: None,
            bitfield_fields: None,
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
                    arena.alloc(MirExpr::Var(
                        arena.alloc_str("input"),
                        MirType::Struct(arena.alloc_str("ComputeInput")),
                    )),
                    arena.alloc_str("gid"),
                    MirType::Vec(3, arena.alloc(MirType::U32)),
                ),
            )],
            return_expr: None,
            comments: vec![],
        }],
        constants: vec![],
        render_blocks: vec![],
    };

    let wgsl = emit_wgsl(&program);
    assert!(
        wgsl.contains("@compute @workgroup_size(64, 1, 1)"),
        "WGSL: {}",
        wgsl
    );
    assert!(wgsl.contains("input: ComputeInput"), "WGSL: {}", wgsl);
    assert!(
        wgsl.contains("@builtin(global_invocation_id) gid: vec3<u32>"),
        "WGSL: {}",
        wgsl
    );
}

#[test]
fn codegen_struct_and_function_ordering() {
    let arena = Allocator::new();
    let program = MirProgram {
        structs: vec![MirStruct {
            name: arena.alloc_str("Particle"),
            fields: vec![
                MirField {
                    name: arena.alloc_str("pos"),
                    ty: MirType::Vec(3, arena.alloc(MirType::F32)),
                    doc: None,
                    attributes: vec![],
                },
                MirField {
                    name: arena.alloc_str("life"),
                    ty: MirType::F32,
                    doc: None,
                    attributes: vec![],
                },
            ],
            adt_variants: None,
            bitfield_fields: None,
            comments: vec![],
            origin_module: None,
        }],
        globals: vec![],
        functions: vec![MirFunction {
            name: arena.alloc_str("get_life"),
            params: vec![MirParam {
                name: arena.alloc_str("p"),
                ty: MirType::Struct(arena.alloc_str("Particle")),
            }],
            return_ty: MirType::F32,
            body: vec![],
            return_expr: Some(MirExpr::FieldAccess(
                arena.alloc(MirExpr::Var(
                    arena.alloc_str("p"),
                    MirType::Struct(arena.alloc_str("Particle")),
                )),
                arena.alloc_str("life"),
                MirType::F32,
            )),
            comments: vec![],
            is_const: false,
        }],
        entry_points: vec![],
        constants: vec![],
        render_blocks: vec![],
    };

    let wgsl = emit_wgsl(&program);
    assert!(wgsl.contains("struct Particle {"), "WGSL: {}", wgsl);
    assert!(wgsl.contains("life: f32,"), "WGSL: {}", wgsl);
    assert!(wgsl.contains("return p.life;"), "WGSL: {}", wgsl);

    let struct_pos = wgsl.find("struct Particle").unwrap();
    let fn_pos = wgsl.find("fn get_life").unwrap();
    assert!(
        struct_pos < fn_pos,
        "structs should appear before functions in WGSL output"
    );
}

#[test]
fn codegen_if_else_statement() {
    let arena = Allocator::new();
    let program = MirProgram {
        structs: vec![],
        globals: vec![],
        functions: vec![MirFunction {
            name: arena.alloc_str("max_val"),
            params: vec![
                MirParam {
                    name: arena.alloc_str("a"),
                    ty: MirType::I32,
                },
                MirParam {
                    name: arena.alloc_str("b"),
                    ty: MirType::I32,
                },
            ],
            return_ty: MirType::I32,
            body: vec![MirStmt::If(
                MirExpr::BinOp(
                    MirBinOp::Gt,
                    arena.alloc(MirExpr::Var(arena.alloc_str("a"), MirType::I32)),
                    arena.alloc(MirExpr::Var(arena.alloc_str("b"), MirType::I32)),
                    MirType::Bool,
                ),
                vec![MirStmt::Return(MirExpr::Var(
                    arena.alloc_str("a"),
                    MirType::I32,
                ))],
                vec![MirStmt::Return(MirExpr::Var(
                    arena.alloc_str("b"),
                    MirType::I32,
                ))],
            )],
            return_expr: None,
            comments: vec![],
            is_const: false,
        }],
        entry_points: vec![],
        constants: vec![],
        render_blocks: vec![],
    };

    let wgsl = emit_wgsl(&program);
    assert!(wgsl.contains("if ("), "WGSL: {}", wgsl);
    assert!(wgsl.contains("} else {"), "WGSL: {}", wgsl);
    assert!(wgsl.contains("return a;"), "WGSL: {}", wgsl);
    assert!(wgsl.contains("return b;"), "WGSL: {}", wgsl);
}

#[test]
fn codegen_vertex_shader() {
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
                    MirExpr::Lit(MirLit::F32(0.5)),
                    MirExpr::Lit(MirLit::F32(0.0)),
                    MirExpr::Lit(MirLit::F32(1.0)),
                ],
                MirType::Vec(4, arena.alloc(MirType::F32)),
            )),
            comments: vec![],
        }],
        constants: vec![],
        render_blocks: vec![],
    };

    let wgsl = emit_wgsl(&program);
    assert!(wgsl.contains("@vertex"), "WGSL: {}", wgsl);
    assert!(wgsl.contains("fn vs_main()"), "WGSL: {}", wgsl);
    assert!(wgsl.contains("-> @location(0) vec4<f32>"), "WGSL: {}", wgsl);
}

#[test]
fn codegen_fragment_shader() {
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
        render_blocks: vec![],
    };

    let wgsl = emit_wgsl(&program);
    assert!(wgsl.contains("@fragment"), "WGSL: {}", wgsl);
    assert!(wgsl.contains("fn fs_main()"), "WGSL: {}", wgsl);
    assert!(wgsl.contains("-> @location(0) vec4<f32>"), "WGSL: {}", wgsl);
}

#[test]
fn codegen_mir_type_display() {
    let arena = Allocator::new();
    assert_eq!(format!("{}", MirType::I32), "i32");
    assert_eq!(format!("{}", MirType::U32), "u32");
    assert_eq!(format!("{}", MirType::F32), "f32");
    assert_eq!(format!("{}", MirType::Bool), "bool");
    assert_eq!(
        format!("{}", MirType::Vec(3, arena.alloc(MirType::F32))),
        "vec3<f32>"
    );
    assert_eq!(
        format!("{}", MirType::Mat(4, 4, arena.alloc(MirType::F32))),
        "mat4x4<f32>"
    );
    assert_eq!(
        format!("{}", MirType::Array(arena.alloc(MirType::F32), 16)),
        "array<f32, 16>"
    );
    assert_eq!(
        format!(
            "{}",
            MirType::Array(arena.alloc(MirType::Array(arena.alloc(MirType::F32), 4)), 2)
        ),
        "array<array<f32, 4>, 2>"
    );
}
