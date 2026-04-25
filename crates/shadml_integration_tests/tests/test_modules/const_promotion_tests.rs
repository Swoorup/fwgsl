use super::*;
use std::panic;

fn compile_to_wgsl(source: &str) -> Result<String, String> {
    let mut parser = Parser::new(source);
    let mut program = parser.parse_program();

    if parser.diagnostics().has_errors() {
        return Err("parse error".into());
    }

    with_prelude(&mut program);

    let mut sa = SemanticAnalyzer::new();
    sa.analyze(&program);

    if sa.has_errors() {
        return Err(sa
            .diagnostics()
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
            .join("\n"));
    }

    let mut lowering = AstLowering::new(&sa);
    let hir = lowering.lower_program(&program);

    if lowering.has_errors() {
        return Err(lowering
            .diagnostics()
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
            .join("\n"));
    }

    let arena = shadml_allocator::Allocator::new();
    let mir = shadml_mir::lower::lower_hir_to_mir(&arena, &hir).map_err(|e| {
        e.iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    })?;
    let mir = shadml_mir::reachability::eliminate_dead_code(&mir);

    Ok(shadml_wgsl_codegen::emit_wgsl(&mir))
}

#[test]
fn zero_param_literal_becomes_const() {
    let source = "maxLights : I32\nmaxLights = 64";
    let wgsl = compile_to_wgsl(source).expect("should compile");
    assert!(
        wgsl.contains("const maxLights: i32 = 64i;"),
        "should emit const declaration, got: {}",
        wgsl
    );
    assert!(
        !wgsl.contains("fn maxLights"),
        "should NOT emit function declaration, got: {}",
        wgsl
    );
}

#[test]
fn zero_param_float_literal_becomes_const() {
    let source = "pi : F32\npi = 3.14159";
    let wgsl = compile_to_wgsl(source).expect("should compile");
    assert!(
        wgsl.contains("const pi: f32 ="),
        "should emit const for float literal, got: {}",
        wgsl
    );
    assert!(
        !wgsl.contains("fn pi"),
        "should NOT emit function, got: {}",
        wgsl
    );
}

#[test]
fn zero_param_arithmetic_becomes_const() {
    let source = "stride : I32\nstride = 4 + 3";
    let wgsl = compile_to_wgsl(source).expect("should compile");
    assert!(
        wgsl.contains("const stride: i32 ="),
        "should emit const for arithmetic, got: {}",
        wgsl
    );
    assert!(
        !wgsl.contains("fn stride"),
        "should NOT emit function, got: {}",
        wgsl
    );
}

#[test]
fn function_with_params_stays_function() {
    let source = "double : I32 -> I32\ndouble x = x * 2";
    let wgsl = compile_to_wgsl(source).expect("should compile");
    assert!(
        wgsl.contains("fn double("),
        "function with params should stay a function, got: {}",
        wgsl
    );
}

#[test]
fn zero_param_negation_becomes_const() {
    let source = "neg1 : I32\nneg1 = -1";
    let wgsl = compile_to_wgsl(source).expect("should compile");
    assert!(
        wgsl.contains("const neg1: i32 ="),
        "negation should be promoted to const, got: {}",
        wgsl
    );
}

#[test]
fn zero_param_bool_expr_becomes_const() {
    let source = "enabled : Bool\nenabled = 1 == 1";
    let wgsl = compile_to_wgsl(source).expect("should compile");
    assert!(
        wgsl.contains("const enabled: bool ="),
        "bool expression should be promoted to const, got: {}",
        wgsl
    );
    assert!(
        !wgsl.contains("fn enabled"),
        "should NOT emit function, got: {}",
        wgsl
    );
}

#[test]
fn const_used_in_entry_point() {
    // Ensure promoted constants are usable from entry points and produce valid WGSL
    let source = r#"
@group(0) @binding(0) storage(read_write) results : Array<Vec<4, F32>>

data ComputeInput = ComputeInput {
  @builtin(global_invocation_id) gid : Vec<3, U32>
}

maxLights : I32
maxLights = 64

main : ComputeInput -> ()
@compute @workgroup_size(64, 1, 1)
main input =
  let idx = toU32 input.gid.x
      result = vec4 (toF32 maxLights) 0.0 0.0 1.0
  in writeAt results idx result
"#;
    let wgsl = compile_to_wgsl(source).expect("should compile");
    assert!(
        wgsl.contains("const maxLights: i32 = 64i;"),
        "should emit const declaration, got: {}",
        wgsl
    );
    assert!(
        !wgsl.contains("fn maxLights"),
        "should NOT emit function, got: {}",
        wgsl
    );
}

#[test]
fn mir_validation_rejects_const_call_and_codegen_panics() {
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

    let errors = shadml_mir::validate::validate_program(&program)
        .expect_err("validator should reject calls to constants");
    assert!(
        errors
            .iter()
            .any(|e| e.contains("invalid call to constant 'maxLights'")),
        "expected const-call validation error, got: {:?}",
        errors
    );

    let panic_payload = panic::catch_unwind(|| emit_wgsl(&program))
        .expect_err("emit_wgsl should panic on invalid MIR");
    let panic_message = if let Some(msg) = panic_payload.downcast_ref::<String>() {
        msg.clone()
    } else if let Some(msg) = panic_payload.downcast_ref::<&str>() {
        msg.to_string()
    } else {
        "<non-string panic>".to_string()
    };
    assert!(
        panic_message.contains("attempted to emit invalid MIR as WGSL"),
        "expected codegen panic to mention invalid MIR, got: {}",
        panic_message
    );
    assert!(
        panic_message.contains("invalid call to constant 'maxLights'"),
        "expected codegen panic to include validator detail, got: {}",
        panic_message
    );
}

#[test]
fn explicit_const_attribute_emits_wgsl_const() {
    let source = "@const\nmaxLights : I32\nmaxLights = 64";
    let wgsl = compile_to_wgsl(source).expect("should compile");
    assert!(
        wgsl.contains("const maxLights: i32 = 64i;"),
        "@const should emit WGSL const, got: {}",
        wgsl
    );
    assert!(
        !wgsl.contains("fn maxLights"),
        "@const should NOT emit function, got: {}",
        wgsl
    );
}

#[test]
fn explicit_const_with_arithmetic_emits_wgsl_const() {
    let source = "@const\nstride : I32\nstride = 4 + 3";
    let wgsl = compile_to_wgsl(source).expect("should compile");
    assert!(
        wgsl.contains("const stride: i32 ="),
        "@const arithmetic should emit WGSL const, got: {}",
        wgsl
    );
}

#[test]
fn explicit_const_referencing_another_const_succeeds() {
    let source = r#"
@const
base = 4

@const
stride = base + 3
"#;
    let wgsl = compile_to_wgsl(source).expect("should compile");
    assert!(
        wgsl.contains("const base: i32 = 4i;"),
        "base should be emitted as const, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("const stride: i32 ="),
        "stride should be emitted as const, got: {}",
        wgsl
    );
}

#[test]
fn explicit_const_on_function_with_params_fails() {
    let source = "@const\ndouble : I32 -> I32\ndouble x = x * 2";
    let result = compile_to_wgsl(source);
    assert!(
        result.is_err(),
        "@const on function with params should fail"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("@const") && err.contains("parameters"),
        "expected @const parameter error, got: {}",
        err
    );
}

#[test]
fn explicit_const_referencing_non_const_fails() {
    let source = r#"
getBlockSize : I32 -> I32
getBlockSize x = x

@const
tableSize = 256 * getBlockSize 1
"#;
    let result = compile_to_wgsl(source);
    assert!(result.is_err(), "@const referencing non-const should fail");
    let err = result.unwrap_err();
    assert!(
        err.contains("@const"),
        "expected @const error, got: {}",
        err
    );
}
