
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
        let mir = shadml_mir::lower::lower_hir_to_mir(&arena, &hir).map_err(|e| e.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(", "))?;
        let mir = shadml_mir::reachability::eliminate_dead_code(&mir);

        Ok(emit_wgsl(&mir))
    }

    fn validate_wgsl(wgsl: &str) -> Result<(), String> {
        let module = naga::front::wgsl::parse_str(wgsl).map_err(|e| format!("naga parse: {e}"))?;
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        validator
            .validate(&module)
            .map_err(|e| format!("naga validate: {e}"))?;
        Ok(())
    }

    fn compile_and_validate(source: &str) -> Result<String, String> {
        let wgsl = compile_to_wgsl(source)?;
        validate_wgsl(&wgsl).map_err(|e| format!("{e}\n--- WGSL ---\n{wgsl}"))?;
        Ok(wgsl)
    }

    #[test]
    fn naga_simple_function() {
        let source = "add : I32 -> I32 -> I32\nadd x y = x + y";
        compile_and_validate(source).expect("simple function should produce valid WGSL");
    }

    #[test]
    fn naga_compute_entry_point() {
        let source = r#"
data ComputeInput = ComputeInput {
  @builtin(global_invocation_id) gid : Vec<3, U32>
}

@group(0) @binding(0) storage(read_write) output : Array<Vec<4, F32>, 64>

main : ComputeInput -> ()
@compute @workgroup_size(64, 1, 1)
main input =
  let idx = toI32 input.gid.x
      color = vec4 1.0 0.0 0.0 1.0
  in writeAt output idx color
"#;
        compile_and_validate(source).expect("compute entry point should produce valid WGSL");
    }

    #[test]
    fn naga_data_type_and_match() {
        let source = r#"
data Color = Red | Green | Blue

colorToFloat : Color -> F32
colorToFloat c = match c
  | Red   -> 1.0
  | Green -> 0.5
  | Blue  -> 0.0
"#;
        compile_and_validate(source).expect("enum + match should produce valid WGSL");
    }

    #[test]
    fn naga_record_type() {
        let source = r#"
data Particle = Particle {
  x : F32,
  y : F32,
}

getX : Particle -> F32
getX p = p.x
"#;
        compile_and_validate(source).expect("record type should produce valid WGSL");
    }

    #[test]
    fn naga_if_then_else() {
        let source = r#"
maxVal : F32 -> F32 -> F32
maxVal a b = if a > b then a else b
"#;
        compile_and_validate(source).expect("if-then-else should produce valid WGSL");
    }

    #[test]
    fn naga_let_binding() {
        let source = r#"
pythagoras : F32 -> F32 -> F32
pythagoras a b =
  let a2 = a * a
      b2 = b * b
  in sqrt (a2 + b2)
"#;
        compile_and_validate(source).expect("let binding should produce valid WGSL");
    }

    #[test]
    fn naga_loop_expression() {
        let source = r#"
sumTo : I32 -> I32
sumTo n = loop go (i = 0) (acc = 0) in
  if i >= n
    then acc
    else go (i + 1) (acc + i)
"#;
        compile_and_validate(source).expect("loop expression should produce valid WGSL");
    }

    #[test]
    fn naga_const_declaration() {
        let source = r#"
const PI : F32 = 3.14159

circleArea : F32 -> F32
circleArea r = PI * r * r
"#;
        compile_and_validate(source).expect("const declaration should produce valid WGSL");
    }

    #[test]
    fn naga_uniform_binding() {
        let source = r#"
@group(0) @binding(0) uniform time : F32

getTime : F32
getTime = load time
"#;
        compile_and_validate(source).expect("uniform binding should produce valid WGSL");
    }

    #[test]
    fn naga_group_block_bindings() {
        let source = r#"
@group(0)
  @binding(0) uniform frame  : Vec<4, F32>
  @binding(1) uniform params : Vec<4, F32>

getFrame : Vec<4, F32>
getFrame = load frame
"#;
        compile_and_validate(source).expect("group block bindings should produce valid WGSL");
    }
