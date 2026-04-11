use super::*;

#[test]
fn parse_then_semantic_data_type_with_match() {
    let source = "\
data Color = Red | Green | Blue

show c = match c
  | Red   -> 0
  | Green -> 1
  | Blue  -> 2
";
    let (program, _parse_errors) = parse(source);
    let mut sa = SemanticAnalyzer::new();
    sa.analyze(&program);

    assert!(sa.constructors.contains_key("Red"));
    assert!(sa.constructors.contains_key("Green"));
    assert!(sa.constructors.contains_key("Blue"));
}

#[test]
fn mir_to_wgsl_round_trip_is_valid_text() {
    let arena = shadml_allocator::Allocator::new();
    let program = MirProgram {
        structs: vec![],
        globals: vec![],
        functions: vec![MirFunction {
            name: arena.alloc_str("identity"),
            params: vec![MirParam {
                name: arena.alloc_str("x"),
                ty: MirType::I32,
            }],
            return_ty: MirType::I32,
            body: vec![],
            return_expr: Some(MirExpr::Var(arena.alloc_str("x"), MirType::I32)),
            comments: vec![],
        }],
        entry_points: vec![],
        constants: vec![],
        render_blocks: vec![],
    };

    let wgsl = emit_wgsl(&program);
    assert!(!wgsl.is_empty(), "WGSL output should not be empty");
    assert!(wgsl.contains("fn "), "WGSL should contain function keyword");
    assert!(
        wgsl.contains("return"),
        "WGSL should contain return statement"
    );
    assert!(wgsl.ends_with('\n'), "WGSL should end with newline");
}

#[test]
fn wgsl_codegen_no_spurious_semicolons() {
    let arena = shadml_allocator::Allocator::new();
    let program = MirProgram {
        structs: vec![],
        globals: vec![],
        functions: vec![MirFunction {
            name: arena.alloc_str("f"),
            params: vec![],
            return_ty: MirType::I32,
            body: vec![
                MirStmt::Let(
                    arena.alloc_str("a"),
                    MirType::I32,
                    MirExpr::Lit(MirLit::I32(1)),
                ),
                MirStmt::Let(
                    arena.alloc_str("b"),
                    MirType::I32,
                    MirExpr::Lit(MirLit::I32(2)),
                ),
            ],
            return_expr: Some(MirExpr::BinOp(
                MirBinOp::Add,
                arena.alloc(MirExpr::Var(arena.alloc_str("a"), MirType::I32)),
                arena.alloc(MirExpr::Var(arena.alloc_str("b"), MirType::I32)),
                MirType::I32,
            )),
            comments: vec![],
        }],
        entry_points: vec![],
        constants: vec![],
        render_blocks: vec![],
    };

    let wgsl = emit_wgsl(&program);
    assert!(
        !wgsl.contains(";;"),
        "WGSL should not contain double semicolons: {}",
        wgsl
    );
}

#[test]
fn wgsl_codegen_proper_indentation() {
    let arena = shadml_allocator::Allocator::new();
    let program = MirProgram {
        structs: vec![],
        globals: vec![],
        functions: vec![MirFunction {
            name: arena.alloc_str("f"),
            params: vec![MirParam {
                name: arena.alloc_str("x"),
                ty: MirType::I32,
            }],
            return_ty: MirType::I32,
            body: vec![],
            return_expr: Some(MirExpr::Var(arena.alloc_str("x"), MirType::I32)),
            comments: vec![],
        }],
        entry_points: vec![],
        constants: vec![],
        render_blocks: vec![],
    };

    let wgsl = emit_wgsl(&program);
    assert!(
        wgsl.contains("  return x;"),
        "return statement should be indented in WGSL: {}",
        wgsl
    );
}

#[test]
fn semantic_analysis_with_adt_fixture() {
    let source = include_str!("../../../../fixtures/adt.shadml");
    let (program, _) = parse(source);
    let mut sa = SemanticAnalyzer::new();
    sa.analyze(&program);

    assert!(
        sa.data_types.contains_key("Color"),
        "Color data type should be registered after analyzing adt.shadml"
    );
    assert_eq!(
        sa.data_types["Color"].constructors.len(),
        3,
        "Color should have 3 constructors"
    );
}

#[test]
fn full_mir_program_with_all_shader_stages() {
    let arena = shadml_allocator::Allocator::new();
    let program = MirProgram {
        structs: vec![],
        globals: vec![],
        functions: vec![],
        entry_points: vec![
            MirEntryPoint {
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
            },
            MirEntryPoint {
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
            },
            MirEntryPoint {
                name: arena.alloc_str("cs_main"),
                stage: ShaderStage::Compute,
                workgroup_size: Some([64, 1, 1]),
                params: vec![],
                return_ty: MirType::Unit,
                body: vec![],
                return_expr: None,
                comments: vec![],
            },
        ],
        constants: vec![],
        render_blocks: vec![],
    };

    let wgsl = emit_wgsl(&program);
    assert!(wgsl.contains("@vertex"), "should contain vertex stage");
    assert!(wgsl.contains("@fragment"), "should contain fragment stage");
    assert!(wgsl.contains("@compute"), "should contain compute stage");
    assert!(wgsl.contains("fn vs_main"), "should contain vs_main");
    assert!(wgsl.contains("fn fs_main"), "should contain fs_main");
    assert!(wgsl.contains("fn cs_main"), "should contain cs_main");
}
