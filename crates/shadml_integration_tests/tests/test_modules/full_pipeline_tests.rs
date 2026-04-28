use super::*;

const SWIZZLES_EXAMPLE: &str = include_str!("../../../../examples/swizzles.shadml");
const VEC_LITERALS_EXAMPLE: &str = include_str!("../../../../examples/vec-literals.shadml");

/// Full pipeline helper: source -> WGSL
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

    Ok(emit_wgsl(&mir))
}

#[test]
fn test_full_pipeline_add_function() {
    let source = "add : I32 -> I32 -> I32\nadd x y = x + y";
    let wgsl = compile_to_wgsl(source).expect("compilation should succeed");
    assert!(
        wgsl.contains("fn add("),
        "WGSL should contain fn add, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("x: i32"),
        "WGSL should contain x: i32, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("y: i32"),
        "WGSL should contain y: i32, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("-> i32"),
        "WGSL should contain -> i32, got: {}",
        wgsl
    );
    assert!(wgsl.contains("+"), "WGSL should contain +, got: {}", wgsl);
}

#[test]
fn test_full_pipeline_record_pattern_rest_binds_named_field() {
    let source = r#"
alias Vec3F = Vec<3, F32>

data ParticleState
  = Active { position : Vec3F, velocity : Vec3F, life : F32 }
  | Dead

impl ParticleState where
  lifeValue : ParticleState -> F32
  lifeValue particle =
    match particle
      | Active { life, .. } -> life
      | Dead -> 0.0
"#;
    let wgsl = compile_to_wgsl(source).expect("compilation should succeed");
    assert!(wgsl.contains("let life = "), "WGSL: {}", wgsl);
    assert!(wgsl.contains(".life;"), "WGSL: {}", wgsl);
    assert!(
        !wgsl.contains("let life = _scrut_607.position;"),
        "WGSL: {}",
        wgsl
    );
}

#[test]
fn test_full_pipeline_double_function() {
    let source = "double : I32 -> I32\ndouble x = x * 2";
    let wgsl = compile_to_wgsl(source).expect("compilation should succeed");
    assert!(
        wgsl.contains("fn double("),
        "WGSL should contain fn double, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("-> i32"),
        "WGSL should contain -> i32, got: {}",
        wgsl
    );
    assert!(wgsl.contains("*"), "WGSL should contain *, got: {}", wgsl);
}

#[test]
fn test_full_pipeline_if_expression() {
    let source = "f x = if x == 0 then 1 else 2";
    let wgsl = compile_to_wgsl(source).expect("compilation should succeed");
    assert!(
        wgsl.contains("fn f("),
        "WGSL should contain fn f, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("select("),
        "WGSL should contain select call for simple if-then-else, got: {}",
        wgsl
    );
}

#[test]
fn test_full_pipeline_let_expression() {
    let source = "f = let x = 42 in x + 1";
    let wgsl = compile_to_wgsl(source).expect("compilation should succeed");
    assert!(
        wgsl.contains("fn f("),
        "WGSL should contain fn f, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("let x"),
        "WGSL should contain let x, got: {}",
        wgsl
    );
}

#[test]
fn test_full_pipeline_where_clause() {
    let source = "f x = y * 2 where y = x + 1";
    let wgsl = compile_to_wgsl(source).expect("compilation should succeed");
    assert!(
        wgsl.contains("fn f("),
        "WGSL should contain fn f, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("let y"),
        "WGSL should contain lowered where binding, got: {}",
        wgsl
    );
    assert!(wgsl.contains("*"), "WGSL should contain *, got: {}", wgsl);
}

#[test]
fn test_full_pipeline_multiple_functions() {
    let source = r#"
add : I32 -> I32 -> I32
add x y = x + y

double : I32 -> I32
double x = x * 2
"#;
    let wgsl = compile_to_wgsl(source).expect("should compile");
    assert!(
        wgsl.contains("fn add("),
        "should contain fn add, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("fn double("),
        "should contain fn double, got: {}",
        wgsl
    );
    assert!(wgsl.contains("i32"), "should contain i32, got: {}", wgsl);
}

#[test]
fn test_full_pipeline_generic_function_not_emitted_without_specialization() {
    let source = "add : Add a b => a -> b -> a.Output\nadd x y = x + y";
    let wgsl = compile_to_wgsl(source).expect("generic definition with a signature should compile");
    assert!(
        !wgsl.contains("fn add("),
        "unspecialized generic template should not be emitted as WGSL, got: {}",
        wgsl
    );
    assert!(
        !wgsl.contains("fn add_i32("),
        "no specialization should be emitted without a concrete call site, got: {}",
        wgsl
    );
}

#[test]
fn test_full_pipeline_generic_function_specializes_at_concrete_call_site() {
    let source = r#"
add : Add a b => a -> b -> a.Output
add x y = x + y

result : I32
result = add 1 2
"#;
    let wgsl = compile_to_wgsl(source).expect("generic call should specialize");
    assert!(
        wgsl.contains("fn add_i32_i32("),
        "WGSL should contain the specialized add_i32_i32 function, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("fn result() -> i32") && wgsl.contains("return add_i32_i32(1i, 2i);"),
        "WGSL should call the specialized add_i32_i32 helper from result, got: {}",
        wgsl
    );
}

#[test]
fn test_full_pipeline_vec_literals_example() {
    let wgsl = compile_to_wgsl(VEC_LITERALS_EXAMPLE).expect("vec literals example should compile");
    assert!(
        wgsl.contains("fn main("),
        "WGSL should contain main, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("vec3<"),
        "WGSL should contain vec3 constructor, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("vec4<"),
        "WGSL should contain vec4 constructor, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains(".x"),
        "WGSL should contain swizzle field access, got: {}",
        wgsl
    );
}

#[test]
fn test_full_pipeline_swizzles_example() {
    let wgsl = compile_to_wgsl(SWIZZLES_EXAMPLE).expect("swizzles example should compile");
    assert!(
        wgsl.contains("fn main("),
        "WGSL should contain main, got: {}",
        wgsl
    );
    for swizzle in [".xy", ".rg", ".xyz", ".rgb", ".xyzw", ".rgba", ".a"] {
        assert!(
            wgsl.contains(swizzle),
            "WGSL should contain swizzle {}, got: {}",
            swizzle,
            wgsl
        );
    }
}

#[test]
fn test_full_pipeline_loop_expression() {
    // Named tail-recursive loop: counts i up to x, returns final i
    let source = "f : I32 -> I32\nf x = loop go (i = 0) in if i < x then go (i + 1) else i";
    let wgsl = compile_to_wgsl(source).expect("loop compilation should succeed");
    assert!(
        wgsl.contains("fn f("),
        "WGSL should contain fn f, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("loop {"),
        "WGSL should contain loop statement, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("break;"),
        "WGSL should contain break statement, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("continue;"),
        "WGSL should contain continue statement, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("var i"),
        "WGSL should contain var i, got: {}",
        wgsl
    );
}

// ---------------------------------------------------------------------------
// Sampler state hint attribute tests
// ---------------------------------------------------------------------------

#[test]
fn test_full_pipeline_sampler_state_stripped_from_wgsl() {
    let source = r#"@group(0) @binding(0) @samplerState(filter = "nearest", address_mode_u = "clamp_to_edge") mySampler : Sampler

@compute @workgroup_size(1, 1, 1)
main : () -> ()
main () = ()
"#;
    let wgsl = compile_to_wgsl(source).expect("compilation should succeed");
    assert!(
        !wgsl.contains("samplerState"),
        "WGSL should not contain @samplerState, got: {}",
        wgsl
    );
}

#[test]
fn test_full_pipeline_sampler_state_on_non_sampler_produces_error() {
    let source = r#"@group(0) @binding(0) @samplerState(filter = "nearest") myBuf : F32

@compute @workgroup_size(1, 1, 1)
main : () -> ()
main () = ()
"#;
    let result = compile_to_wgsl(source);
    assert!(
        result.is_err(),
        "compilation should fail for @samplerState on non-sampler"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("@samplerState") && err.contains("sampler bindings"),
        "Error should mention @samplerState and sampler bindings, got: {}",
        err
    );
}

#[test]
fn test_full_pipeline_sampler_state_unknown_field_produces_error() {
    let source = r#"@group(0) @binding(0) @samplerState(unknown_field = "value") mySampler : Sampler

@compute @workgroup_size(1, 1, 1)
main : () -> ()
main () = ()
"#;
    let result = compile_to_wgsl(source);
    assert!(
        result.is_err(),
        "compilation should fail for unknown @samplerState field"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("unknown '@samplerState' field"),
        "Error should mention unknown field, got: {}",
        err
    );
}

#[test]
fn test_full_pipeline_sampler_state_compare_on_sampler_produces_error() {
    let source = r#"@group(0) @binding(0) @samplerState(compare = "less_equal") mySampler : Sampler

@compute @workgroup_size(1, 1, 1)
main : () -> ()
main () = ()
"#;
    let result = compile_to_wgsl(source);
    assert!(
        result.is_err(),
        "compilation should fail for compare on Sampler"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("compare") && err.contains("SamplerComparison"),
        "Error should mention compare and SamplerComparison, got: {}",
        err
    );
}

#[test]
fn test_full_pipeline_sampler_state_valid_on_sampler_comparison() {
    let source = r#"@group(0) @binding(0) @samplerState(filter = "nearest") mySampler : SamplerComparison

@compute @workgroup_size(1, 1, 1)
main : () -> ()
main () = ()
"#;
    let wgsl = compile_to_wgsl(source).expect("compilation should succeed");
    assert!(
        !wgsl.contains("samplerState"),
        "WGSL should not contain @samplerState, got: {}",
        wgsl
    );
}

// ---------------------------------------------------------------------------
// Texture sample type hint attribute tests
// ---------------------------------------------------------------------------

#[test]
fn test_full_pipeline_texture_sample_type_stripped_from_wgsl() {
    let source = r#"@group(0) @binding(0) @textureSampleType(filterable = false) myTex : Texture2d F32

@compute @workgroup_size(1, 1, 1)
main : () -> ()
main () = ()
"#;
    let wgsl = compile_to_wgsl(source).expect("compilation should succeed");
    assert!(
        !wgsl.contains("textureSampleType"),
        "WGSL should not contain @textureSampleType, got: {}",
        wgsl
    );
}

#[test]
fn test_full_pipeline_texture_sample_type_on_sampler_produces_error() {
    let source = r#"@group(0) @binding(0) @textureSampleType(filterable = false) mySampler : Sampler

@compute @workgroup_size(1, 1, 1)
main : () -> ()
main () = ()
"#;
    let result = compile_to_wgsl(source);
    assert!(
        result.is_err(),
        "compilation should fail for @textureSampleType on sampler"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("@textureSampleType") && err.contains("texture bindings"),
        "Error should mention @textureSampleType and texture bindings, got: {}",
        err
    );
}

#[test]
fn test_full_pipeline_texture_sample_type_filterable_on_i32_produces_error() {
    let source = r#"@group(0) @binding(0) @textureSampleType(filterable = false) myTex : Texture2d I32

@compute @workgroup_size(1, 1, 1)
main : () -> ()
main () = ()
"#;
    let result = compile_to_wgsl(source);
    assert!(
        result.is_err(),
        "compilation should fail for filterable on I32 texture"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("filterable") && err.contains("F32"),
        "Error should mention filterable and F32, got: {}",
        err
    );
}

#[test]
fn test_full_pipeline_texture_sample_type_unknown_field_produces_error() {
    let source = r#"@group(0) @binding(0) @textureSampleType(unknown = "value") myTex : Texture2d F32

@compute @workgroup_size(1, 1, 1)
main : () -> ()
main () = ()
"#;
    let result = compile_to_wgsl(source);
    assert!(
        result.is_err(),
        "compilation should fail for unknown @textureSampleType field"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("unknown '@textureSampleType' field"),
        "Error should mention unknown field, got: {}",
        err
    );
}
