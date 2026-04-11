use super::*;

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
        return Err(format!(
            "SA: {}",
            sa.diagnostics()
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    let mut lowering = AstLowering::new(&sa);
    let hir = lowering.lower_program(&program);

    if lowering.has_errors() {
        return Err(format!(
            "LOWERING: {}",
            lowering
                .diagnostics()
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n")
        ));
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
fn test_fold_range_basic_sum() {
    let source = r#"
sumRange : I32 -> I32
sumRange n = foldRange 0 n 0 (\acc i -> acc + i)
"#;
    let wgsl = compile_to_wgsl(source).expect("should compile");
    assert!(
        wgsl.contains("loop {"),
        "WGSL should contain a loop, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("acc + i"),
        "WGSL should contain accumulator update, got: {}",
        wgsl
    );
}

#[test]
fn test_fold_range_with_named_function() {
    let source = r#"
addToAcc : I32 -> I32 -> I32
addToAcc acc i = acc + i

sumNamed : I32 -> I32
sumNamed n = foldRange 0 n 0 addToAcc
"#;
    let wgsl = compile_to_wgsl(source).expect("should compile");
    assert!(
        wgsl.contains("loop {"),
        "WGSL should contain a loop, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("addToAcc(_fold_acc, _fold_i)"),
        "WGSL should call named function in loop, got: {}",
        wgsl
    );
}

#[test]
fn test_fold_range_vec3_accumulator() {
    let source = r#"
particle : Vec<2, F32> -> F32 -> F32 -> Vec<3, F32>
particle uv id time = vec3 (sin id) (cos id) 0.0

test : Vec<2, F32> -> F32 -> Vec<3, F32>
test uv time = foldRange 0 10 (vec3 0.0 0.0 0.0) (\acc i ->
  acc + particle uv (toF32 i) time)
"#;
    let wgsl = compile_to_wgsl(source).expect("should compile");
    assert!(
        wgsl.contains("loop {"),
        "WGSL should contain a loop, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("particle(uv, f32(i), time)"),
        "WGSL should call particle in loop body, got: {}",
        wgsl
    );
}

#[test]
fn test_fold_range_constant_bounds() {
    let source = r#"
factorial5 : I32
factorial5 = foldRange 1 6 1 (\acc i -> acc * i)
"#;
    let wgsl = compile_to_wgsl(source).expect("should compile");
    assert!(
        wgsl.contains("i >= 6i"),
        "WGSL should check i >= end, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("acc * i"),
        "WGSL should multiply in loop body, got: {}",
        wgsl
    );
}

#[test]
fn test_fold_range_in_where() {
    let source = r#"
test : F32 -> F32
test x = result
  where
    result = foldRange 0 5 x (\acc i -> acc + toF32 i)
"#;
    let wgsl = compile_to_wgsl(source).expect("should compile");
    assert!(
        wgsl.contains("loop {"),
        "WGSL should contain a loop from foldRange in where, got: {}",
        wgsl
    );
}

#[test]
fn test_fold_range_with_vector_lambda() {
    let source = r#"
shadow : Vec 2 F32 -> Vec 2 F32 -> F32 -> F32
shadow p pos radius =
  let dir = normalize (pos - p)
      dl = length (p - pos)
      result = foldRange 0 32 (vec3 0.01 (radius * dl) 1.0)
        (\acc i ->
           let dt = acc.x
               lf = acc.y
               alive = acc.z
           in if alive < 0.5 then acc
              else if dt > dl then vec3 dt lf 0.0
              else
                let sd = length (p + dir * splat2 dt) - 0.1
                in if sd < (-radius) then vec3 dt 0.0 0.0
                   else vec3 (dt + max 0.02 (abs sd)) (min lf (sd / dt)) 1.0
        )
  in result.x

render test
  @fragment
  fsMain : Vec<4, F32> -> Vec<4, F32>
  fsMain input =
    let p = vec2 0.0 0.0
        light = shadow p (vec2 0.5 0.5) 0.05
    in vec4 light light light 1.0
"#;
    let wgsl = compile_to_wgsl(source).expect("should compile");
    assert!(wgsl.contains("loop {"), "WGSL should contain a loop");
}
