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
fn parse_trait_decl() {
    let source = "trait Num a where\n  add : a -> a -> a\n  sub : a -> a -> a";
    let (program, has_errors) = parse_raw(source);
    assert!(!has_errors, "trait decl should parse without errors");
    assert_eq!(program.decls.len(), 1);
    match &program.decls[0] {
        Decl::TraitDecl {
            name,
            vars,
            methods,
            ..
        } => {
            assert_eq!(name, "Num");
            assert_eq!(vars, &vec!["a".to_string()]);
            assert_eq!(methods.len(), 2);
            assert_eq!(methods[0].name, "add");
            assert_eq!(methods[1].name, "sub");
        }
        other => panic!("expected TraitDecl, got {:?}", other),
    }
}

#[test]
fn parse_impl_decl() {
    let source = "impl Num F32 where\n  add x y = x + y\n  sub x y = x - y";
    let (program, has_errors) = parse_raw(source);
    assert!(!has_errors, "impl decl should parse without errors");
    assert_eq!(program.decls.len(), 1);
    match &program.decls[0] {
        Decl::ImplDecl {
            trait_name,
            methods,
            ..
        } => {
            assert_eq!(trait_name.as_deref(), Some("Num"));
            assert_eq!(methods.len(), 2);
            assert_eq!(methods[0].name, "add");
            assert_eq!(methods[1].name, "sub");
        }
        other => panic!("expected ImplDecl, got {:?}", other),
    }
}

#[test]
fn parse_trait_with_operator_methods() {
    let source = "trait Num a where\n  (+) : a -> a -> a\n  (-) : a -> a -> a";
    let (program, has_errors) = parse_raw(source);
    assert!(
        !has_errors,
        "trait with operators should parse without errors"
    );
    match &program.decls[0] {
        Decl::TraitDecl { methods, .. } => {
            assert_eq!(methods[0].name, "+");
            assert_eq!(methods[1].name, "-");
        }
        other => panic!("expected TraitDecl, got {:?}", other),
    }
}

#[test]
fn trait_impl_lowers_to_function() {
    let source = r#"
trait Scalable a where
  scale : a -> F32 -> a

impl Scalable F32 where
  scale x factor = x * factor

applyScale : F32 -> F32 -> F32
applyScale value factor = scale value factor
"#;
    let wgsl = compile_to_wgsl(source).expect("compilation should succeed");
    assert!(
        wgsl.contains("fn scale_F32("),
        "WGSL should contain mangled impl method, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("scale_F32(value"),
        "WGSL should dispatch to mangled method, got: {}",
        wgsl
    );
}

#[test]
fn method_call_syntax_sugar() {
    let source = r#"
impl F32 where
  half : F32 -> F32
  half x = x * 0.5

apply : F32 -> F32
apply x = x.half
"#;
    let wgsl = compile_to_wgsl(source).expect("compilation should succeed");
    assert!(
        wgsl.contains("half_F32(x)"),
        "method-call sugar should desugar to function call, got: {}",
        wgsl
    );
}

#[test]
fn method_call_syntax_sugar_with_inferred_receiver_type() {
    let source = r#"
impl F32 where
  half : F32 -> F32
  half x = x * 0.5

apply x = x.half
"#;
    let wgsl = compile_to_wgsl(source).expect("compilation should succeed");
    assert!(
        wgsl.contains("half_F32(x)"),
        "dot-call sugar should still infer the impl receiver type, got: {}",
        wgsl
    );
    assert!(
        !wgsl.contains(".half"),
        "dot-call sugar should lower to a function call, got: {}",
        wgsl
    );
}

#[test]
fn method_call_syntax_sugar_for_prelude_functions() {
    let source = r#"
apply : F32 -> F32
apply x = x.sin
"#;
    let wgsl = compile_to_wgsl(source).expect("compilation should succeed");
    assert!(
        wgsl.contains("sin(x)"),
        "prelude functions should remain callable via dot syntax, got: {}",
        wgsl
    );
}

#[test]
fn trait_example_compiles() {
    let source = include_str!("../../../../examples/traits.shadml");
    let wgsl = compile_to_wgsl(source).expect("traits example should compile");
    assert!(wgsl.contains("fn scale_F32("));
    assert!(wgsl.contains("fn applyScale("));
}

#[test]
fn method_syntax_example_compiles() {
    let source = include_str!("../../../../examples/method-syntax.shadml");
    let wgsl = compile_to_wgsl(source).expect("method-syntax example should compile");
    assert!(wgsl.contains("fn half_F32("));
    assert!(wgsl.contains("fn double("));
    assert!(wgsl.contains("fn clampVal_F32("));
}

#[test]
fn slang_style_generic_lighting_compiles_with_specialization() {
    let source = include_str!("../../../../examples/slang-generics.shadml");
    let wgsl = compile_to_wgsl(source).expect("slang generics example should compile");
    assert!(wgsl.contains("fn lighting_pointlight("));
    assert!(wgsl.contains("fn lighting_spotlight("));
    assert!(wgsl.contains("position_PointLight"));
    assert!(wgsl.contains("position_SpotLight"));
    assert!(wgsl.contains("lighting_pointlight(point(),"));
    assert!(wgsl.contains("lighting_spotlight(spot(),"));
}

/// Strips the cross-module section (section 14) from the conflicts example,
/// since single-file compilation can't resolve `import ConflictsLib`.
fn conflicts_source_without_imports() -> String {
    let full = include_str!("../../../../examples/conflicts.shadml");
    // Remove lines starting with "import ConflictsLib" and everything
    // from the section 14 comment onward.
    let mut result = String::new();
    let mut skipping = false;
    for line in full.lines() {
        if line.starts_with("import ConflictsLib") {
            continue;
        }
        if line.contains("14. Cross-module:") {
            skipping = true;
        }
        if skipping {
            continue;
        }
        result.push_str(line);
        result.push('\n');
    }
    result
}

#[test]
fn conflicts_example_type_checks() {
    let source = conflicts_source_without_imports();
    let (sa, has_errors) = parse_and_analyze(&source);
    assert!(
        !has_errors,
        "conflicts.shadml should type-check without errors"
    );
    // Data type "Output" should be registered alongside the associated type "Output".
    assert!(
        sa.data_types.contains_key("Output"),
        "data type 'Output' should be registered"
    );
    // Data type "Velocity" (newtype pattern: constructor = type name).
    assert!(
        sa.data_types.contains_key("Velocity"),
        "data type 'Velocity' should be registered"
    );
    // Prelude traits Add and Mul should be available.
    assert!(
        sa.traits.contains_key("Add"),
        "prelude trait 'Add' should be registered"
    );
    assert!(
        sa.traits.contains_key("Mul"),
        "prelude trait 'Mul' should be registered"
    );
    // User trait with `type Output` alongside prelude traits with `type Output`.
    assert!(
        sa.traits.contains_key("Scale"),
        "user trait 'Scale' should be registered"
    );
    let scale_trait = sa.traits.get("Scale").expect("Scale trait should exist");
    assert!(
        scale_trait.associated_types.contains(&"Output".to_string()),
        "Scale trait should have associated type Output"
    );
    // User type alias (new name, no conflict with prelude).
    assert!(
        sa.type_aliases.contains_key("Color"),
        "user alias 'Color' should be registered"
    );
}

#[test]
fn conflicts_example_compiles_to_wgsl() {
    let source = conflicts_source_without_imports();
    let wgsl = compile_to_wgsl(&source).expect("conflicts example should compile to WGSL");
    // Verify key mangled function names appear in output.
    assert!(
        wgsl.contains("fn scaleTo_Weight("),
        "user Scale trait impl should appear"
    );
    assert!(
        wgsl.contains("struct MaybeVal"),
        "user MaybeVal data type should appear"
    );
    assert!(
        wgsl.contains("struct Status"),
        "user Status data type should appear"
    );
    assert!(
        wgsl.contains("struct Velocity"),
        "newtype Velocity should appear"
    );
}

#[test]
fn conflicts_cross_module_bundles_successfully() {
    use shadml_bundler::{bundle_virtual, VirtualFile};

    let main_source = include_str!("../../../../examples/conflicts.shadml");
    let lib_source = include_str!("../../../../examples/ConflictsLib.shadml");

    let files = vec![
        VirtualFile {
            path: "conflicts.shadml".to_string(),
            source: main_source.to_string(),
        },
        VirtualFile {
            path: "ConflictsLib.shadml".to_string(),
            source: lib_source.to_string(),
        },
    ];

    let result = bundle_virtual(&files, &[], false);
    // The redesigned modules have no type-level name collisions,
    // so the bundler should succeed (or fail for a non-collision reason).
    // Value-level shadowing (e.g., ConflictsLib's `distance` function
    // shadowing the prelude's `extern distance`) is allowed.
    assert!(
        result.is_ok(),
        "conflicts example should bundle without name collisions, got: {:?}",
        result
    );
}

#[test]
fn constrained_generic_let_bound_trait_use_compiles() {
    let source = r#"
trait Light a where
  position : a -> Vec<3, F32>

data PointLight = PointLight {
  lightPosition : Vec<3, F32>
}

impl Light PointLight where
  position light = light.lightPosition

lighting : Light a => a -> Vec<3, F32> -> Vec<3, F32>
lighting light worldPos =
  let lightDir = position light - worldPos
  in lightDir

point : PointLight
point = PointLight { lightPosition = [1.0, 2.0, 3.0] }

result : Vec<3, F32>
result = lighting point [0.0, 0.0, 0.0]
"#;
    let wgsl = compile_to_wgsl(source).expect("generic let-bound trait use should compile");
    assert!(wgsl.contains("fn lighting_pointlight("));
}

#[test]
fn constrained_generic_call_without_impl_fails() {
    let source = r#"
trait Light a where
  position : a -> Vec<3, F32>

data Unlit = Unlit {
  value : F32
}

lighting : Light a => a -> Vec<3, F32>
lighting light = position light

bad : Unlit
bad = Unlit { value = 1.0 }

result : Vec<3, F32>
result = lighting bad
"#;
    assert!(
        compile_to_wgsl(source).is_err(),
        "calling a constrained generic function without a matching impl should fail"
    );
}

#[test]
fn semantic_analysis_with_traits() {
    let source = r#"
trait Show a where
  display : a -> F32

impl Show F32 where
  display x = x

test : F32 -> F32
test x = display x
"#;
    let (_, has_errors) = parse_and_analyze(source);
    assert!(
        !has_errors,
        "trait-using program should pass semantic analysis"
    );
}

#[test]
fn tuple_argument_function_compiles() {
    let source = r#"
pairSum : (I32, I32) -> I32
pairSum (a, b) = a + b

result : I32
result = pairSum (1, 2)
"#;
    let wgsl = compile_to_wgsl(source).expect("tuple-argument function should compile");
    assert!(wgsl.contains("fn pairSum"));
}

#[test]
fn tuple_argument_signature_rejects_curried_definition() {
    let source = r#"
pairSum : (I32, I32) -> I32
pairSum a b = a + b
"#;
    let err = compile_to_wgsl(source).expect_err("curried definition should be rejected");
    assert!(err.contains("expects 1"));
}

#[test]
fn tuple_variable_argument_function_compiles() {
    let source = r#"
pairSum : (I32, I32) -> I32
pairSum (a, b) = a + b

result : I32
result =
  let p = (1, 2)
  in pairSum p
"#;
    let wgsl = compile_to_wgsl(source).expect("tuple variable argument should compile");
    assert!(wgsl.contains("fn pairSum"));
    assert!(wgsl.contains("let __tuple_p_0 = 1i;"));
}

#[test]
fn unannotated_tuple_pattern_parameter_compiles() {
    let source = r#"
test2 a b (k, j) = a

result : I32
result = test2 1 2 (3, 4)
"#;
    let wgsl = compile_to_wgsl(source).expect("unannotated tuple-pattern parameter should compile");
    assert!(wgsl.contains("fn test2_"));
    assert!(wgsl.contains("let k = __tuple__arg2_0;"));
}

#[test]
fn tuple_example_compiles() {
    let source = include_str!("../../../../examples/tuple.shadml");
    compile_to_wgsl(source).expect("tuple example should compile");
}

#[test]
fn bitfield_construction_produces_shift_or_chain() {
    let source = r#"
bitfield Flags : U32 = Flags {
  layer   : U32 : 4,
  stencil : U32 : 8,
}

makeFlags : I32 -> I32 -> Flags
makeFlags l s = Flags { layer = l, stencil = s }
"#;
    let wgsl = compile_to_wgsl(source).expect("bitfield construction should compile");
    assert!(
        wgsl.contains("fn makeFlags("),
        "WGSL should contain fn makeFlags, got: {}",
        wgsl
    );
    // Should contain bitwise ops: &, |, <<
    assert!(
        wgsl.contains("&"),
        "WGSL should contain & for masking, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("|"),
        "WGSL should contain | for combining, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("<<"),
        "WGSL should contain << for shifting, got: {}",
        wgsl
    );
    // Return type should be u32 (bitfield base type)
    assert!(
        wgsl.contains("-> u32"),
        "WGSL should return u32, got: {}",
        wgsl
    );
}

#[test]
fn bitfield_construction_bool_field_uses_select() {
    let source = r#"
bitfield Flags : U32 = Flags {
  visible : Bool,
  layer   : U32 : 4,
}

makeFlags : Bool -> I32 -> Flags
makeFlags v l = Flags { visible = v, layer = l }
"#;
    let wgsl = compile_to_wgsl(source).expect("bitfield construction with bool should compile");
    // 1-bit bool fields should use select(0u, 1u, val)
    assert!(
        wgsl.contains("select("),
        "WGSL should contain select for bool field, got: {}",
        wgsl
    );
}

#[test]
fn bitfield_functional_update_clears_and_sets_field() {
    let source = r#"
bitfield Flags : U32 = Flags {
  visible : Bool,
  layer   : U32 : 4,
  stencil : U32 : 8,
}

updateLayer : Flags -> I32 -> Flags
updateLayer f newLayer = f { layer = newLayer }
"#;
    let wgsl = compile_to_wgsl(source).expect("bitfield update should compile");
    assert!(
        wgsl.contains("fn updateLayer("),
        "WGSL should contain fn updateLayer, got: {}",
        wgsl
    );
    // Should clear bits with AND mask and set new bits with OR
    assert!(
        wgsl.contains("&"),
        "WGSL should contain & for clearing, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("|"),
        "WGSL should contain | for setting, got: {}",
        wgsl
    );
}

#[test]
fn bitfield_construction_in_entry_point() {
    let source = r#"
bitfield Flags : U32 = Flags {
  layer   : U32 : 4,
  stencil : U32 : 8,
}

@group(0) @binding(0) storage(read_write) output : Array<U32, 64>

@compute @workgroup_size(64, 1, 1)
main idx =
  let f = Flags { layer = 5, stencil = 128 }
  in writeAt output idx f
"#;
    let wgsl =
        compile_to_wgsl(source).expect("bitfield construction in entry point should compile");
    assert!(
        wgsl.contains("@compute"),
        "WGSL should contain @compute, got: {}",
        wgsl
    );
    assert!(
        wgsl.contains("0u |"),
        "WGSL should start accumulator from 0u, got: {}",
        wgsl
    );
}

// ========================================================================
// Unified type namespace: duplicate type name error tests
// ========================================================================

#[test]
fn newtype_pattern_is_not_duplicate() {
    // Constructor sharing name with its type is fine (value namespace).
    let source = r#"
data Velocity = Velocity (Vec<3, F32>)
velocityResult : Velocity
velocityResult = Velocity [1.0, 0.0, 0.0]
"#;
    let (sa, has_errors) = parse_and_analyze(source);
    assert!(
        !has_errors,
        "newtype pattern should not be a duplicate type name error, got: {:?}",
        sa.diagnostics()
            .iter()
            .map(|d| &d.message)
            .collect::<Vec<_>>()
    );
}
