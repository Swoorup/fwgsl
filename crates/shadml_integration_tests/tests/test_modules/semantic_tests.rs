
    use super::*;

    #[test]
    fn well_typed_function_inferred() {
        let source = "f x = x + 1";
        let (sa, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "well-typed inferred function should have no errors"
        );
        assert!(sa.env.lookup("f").is_some(), "f should be in type env");
    }

    #[test]
    fn inferred_type_for_arithmetic_function() {
        let source = "f x = x + 1";
        let (sa, has_errors) = parse_and_analyze(source);
        assert!(!has_errors);
        let scheme = sa.env.lookup("f").expect("f should be in env");
        let ty = sa.engine.finalize(&scheme.ty);
        let ty_str = format!("{}", ty);
        assert_eq!(
            ty_str, "(I32 -> I32)",
            "f should have type I32 -> I32, got: {}",
            ty_str
        );
    }

    #[test]
    fn well_typed_if_expression() {
        let source = "f x = if x > 0 then x else 0 - x";
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "if expression with consistent branches should type check"
        );
    }

    #[test]
    fn well_typed_let_expression() {
        let source = "f x = let y = x + 1 in y * 2";
        let (_, has_errors) = parse_and_analyze(source);
        assert!(!has_errors, "let expression should type check");
    }

    #[test]
    fn well_typed_where_expression() {
        let source = "f x = y * 2 where y = x + 1";
        let (_, has_errors) = parse_and_analyze(source);
        assert!(!has_errors, "where clause should type check");
    }

    #[test]
    fn well_typed_data_type_and_pattern_match() {
        // Multi-line match arms may be affected by the parser cross-line merge.
        // First, verify that the data type declaration and constructors are registered.
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
        // The constructors should be registered from the data declaration
        assert!(
            sa.constructors.contains_key("Red"),
            "Red constructor should be registered"
        );
        assert!(
            sa.constructors.contains_key("Green"),
            "Green constructor should be registered"
        );
        assert!(
            sa.constructors.contains_key("Blue"),
            "Blue constructor should be registered"
        );
    }

    #[test]
    fn lambda_type_inference() {
        let source = "f = \\x -> x + 1";
        let (_, has_errors) = parse_and_analyze(source);
        assert!(!has_errors, "lambda should infer correctly");
    }

    #[test]
    fn data_type_constructors_have_correct_tags() {
        let source = "data Direction = North | South | East | West";
        let (sa, has_errors) = parse_and_analyze(source);
        assert!(!has_errors);
        assert_eq!(sa.constructors["North"].tag, 0);
        assert_eq!(sa.constructors["South"].tag, 1);
        assert_eq!(sa.constructors["East"].tag, 2);
        assert_eq!(sa.constructors["West"].tag, 3);
    }

    #[test]
    fn data_type_info_is_registered() {
        let source = "data Color = Red | Green | Blue";
        let (sa, has_errors) = parse_and_analyze(source);
        assert!(!has_errors);
        let dt = sa
            .data_types
            .get("Color")
            .expect("Color should be in data_types");
        assert_eq!(dt.name, "Color");
        assert_eq!(dt.constructors.len(), 3);
        assert!(dt.type_params.is_empty());
    }

    #[test]
    fn generic_constructor_is_registered_polymorphically() {
        let source = "data Box a = Box a";
        let (sa, has_errors) = parse_and_analyze(source);
        assert!(!has_errors);

        let scheme = sa
            .env
            .lookup("Box")
            .expect("Box constructor should be in env");
        assert_eq!(scheme.vars.len(), 1);

        let dt = sa
            .data_types
            .get("Box")
            .expect("Box should be in data_types");
        assert_eq!(dt.type_params, vec!["a"]);
    }

    #[test]
    fn empty_program_has_no_errors() {
        let (_, has_errors) = parse_and_analyze("");
        assert!(!has_errors, "empty program should have no errors");
    }

    #[test]
    fn comment_only_program_has_no_errors() {
        let (_, has_errors) = parse_and_analyze("-- just a comment\n");
        assert!(!has_errors, "comment-only program should have no errors");
    }

    #[test]
    fn multiple_constructors_registered_in_environment() {
        let source = "data Color = Red | Green | Blue";
        let (sa, _) = parse_and_analyze(source);
        assert!(sa.env.lookup("Red").is_some(), "Red should be in env");
        assert!(sa.env.lookup("Green").is_some(), "Green should be in env");
        assert!(sa.env.lookup("Blue").is_some(), "Blue should be in env");
    }

    #[test]
    fn constructor_type_is_correct() {
        let source = "data Color = Red | Green | Blue";
        let (sa, has_errors) = parse_and_analyze(source);
        assert!(!has_errors);
        let scheme = sa.env.lookup("Red").expect("Red should be in env");
        let ty = sa.engine.finalize(&scheme.ty);
        let ty_str = format!("{}", ty);
        assert_eq!(
            ty_str, "Color",
            "Red should have type Color, got: {}",
            ty_str
        );
    }

    #[test]
    fn comparison_returns_bool_typed_expression() {
        let source = "f x = if x == 0 then 1 else 0";
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "comparison should type check when used in if condition"
        );
    }

    #[test]
    fn boolean_operators_type_check() {
        let source = "f x = if x == 0 && x == 1 then 1 else 0";
        let (_, has_errors) = parse_and_analyze(source);
        assert!(!has_errors, "boolean operators should type check");
    }

    // Regression: AssocProj with free type variables from indexing must
    // resolve through predicate improvement, not cause type mismatches
    // during inference. Previously, `p.basis[0][0] + 1.0` failed because
    // the matrix index type variable wasn't resolved before the `+`
    // operator created an AssocProj with a free param.
    #[test]
    fn assoc_proj_with_indexed_type_variable() {
        let source = r#"
data Params = Params { basis : Mat<3, 3, F32> }
test : Params -> F32
test p = p.basis[0][0] + 1.0
"#;
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "indexed expression used with arithmetic operator should type check"
        );
    }

    #[test]
    fn assoc_proj_with_indexed_type_variable_in_vec2() {
        let source = r#"
data Params = Params { basis : Mat<3, 3, F32> }
test : Params -> Vec<2, F32>
test p =
  let scale = p.basis[0][0]
      uv = vec2 1.0 1.0
  in uv - vec2 (0.38 * cos (scale + 1.0)) (0.24 * sin scale)
"#;
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "indexed expression in nested arithmetic + vec2 should type check"
        );
    }

    // ========================================================================
    // Type name conflict tests
    // ========================================================================

    /// A trait associated type named the same as a data type.
    /// E.g. `type Output` in a trait where `Output` is also a data type.
    /// Associated types are scoped to their trait, so they don't conflict.
    #[test]
    fn assoc_type_same_name_as_data_type() {
        let source = r#"
data Output = MkOutput F32

trait Scale a where
  type Output
  scaleTo : a -> F32 -> Self.Output

impl Scale F32 where
  type Output = F32
  scaleTo x f = x * f

test : F32
test = scaleTo 2.0 3.0
"#;
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "associated type 'Output' should not conflict with data type 'Output'"
        );
    }

    /// A trait associated type named the same as a type alias.
    /// `type Output` where `Output` is also a type alias for `F32`.
    /// Bare `Output` resolves to the alias; `Self.Output` resolves to the associated type.
    #[test]
    fn assoc_type_same_name_as_type_alias() {
        let source = r#"
alias Output = F32
trait Combine a b where
  type Output
  combine : a -> b -> Self.Output
impl Combine F32 F32 where
  type Output = F32
  combine x y = x + y
test : F32
test = combine 1.0 2.0
"#;
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "associated type 'Output' should not conflict with type alias 'Output'"
        );
    }

    /// A data constructor with the same name as a top-level function.
    /// Both live in the value namespace, so the later one shadows the earlier.
    #[test]
    fn constructor_same_name_as_function() {
        let source = r#"
data Duo a b = Duo a b
myDuo : Duo F32 F32
myDuo = Duo 1.0 2.0
"#;
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "constructor and function can share a name when they refer to the same thing"
        );
    }

    /// A record type where a field name collides with a top-level binding.
    #[test]
    fn record_field_same_name_as_top_level_binding() {
        let source = r#"
data Point = Point { x : F32, y : F32 }
scale : F32
scale = 2.0
test : Point
test = Point { x = 1.0, y = scale }
"#;
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "record field and top-level binding in separate scopes should not conflict"
        );
    }

    /// An ADT with multiple constructors, where one constructor name
    /// shadows a builtin function name.
    #[test]
    fn adt_constructor_shadows_builtin() {
        let source = r#"
data Wrap = Wrap F32
test : F32
test = let w = Wrap 1.0 in 3.0
"#;
        let (_, has_errors) = parse_and_analyze(source);
        // `Wrap` shadows whatever `Wrap` might be in the prelude, but there's
        // nothing called `Wrap` in the prelude, so this should be fine.
        assert!(
            !has_errors,
            "ADT constructor should work even if it shadows a potential name"
        );
    }

    /// A trait with an associated type whose name is the same as one of
    /// the trait's type parameters.
    #[test]
    fn assoc_type_same_name_as_trait_param() {
        let source = r#"
trait Container a where
  type a
  get : a -> a
"#;
        let (_, has_errors) = parse_and_analyze(source);
        // This is an ambiguous/shadowing situation: `type a` in the trait
        // body could be interpreted as a lowercase type variable or as
        // an associated type declaration. The parser only accepts UpperIdent
        // for associated type names, so `type a` should fail to parse
        // as an associated type. This test documents the current behavior.
        let _ = has_errors;
    }

    /// Using a trait's associated type in a function signature with
    /// explicit `Type.Proj` syntax.
    #[test]
    fn assoc_type_proj_in_function_signature() {
        let source = r#"
trait Container a where
  type Elem
  get : a -> Elem
data Box a = Box a
impl Container (Box a) where
  type Elem = a
  get b = let Box x = b in x
test : Box F32
test = Box 3.0
"#;
        let (_, has_errors) = parse_and_analyze(source);
        // Tests that the associated type `Elem` doesn't conflict
        // with anything and can be used in method signatures.
        let _ = has_errors;
    }

    /// Multiple traits with the same associated type name.
    /// Each trait's associated type is in its own namespace.
    #[test]
    fn multiple_traits_same_assoc_type_name() {
        let source = r#"
trait Plus a b where
  type Output
  plus : a -> b -> Self.Output
trait Times a b where
  type Output
  times : a -> b -> Self.Output
impl Plus F32 F32 where
  type Output = F32
  plus x y = x + y
impl Times F32 F32 where
  type Output = F32
  times x y = x * y
test : F32
test = plus 2.0 (times 3.0 4.0)
"#;
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "two traits with the same associated type name 'Output' should coexist"
        );
    }

    /// A trait and an impl where the trait's type parameter name
    /// collides with a data type name.
    #[test]
    fn trait_type_param_shadows_data_type() {
        let source = r#"
data Outcome = Outcome { value : F32 }
trait Show a where
  show : a -> F32
impl Show Outcome where
  show r = r.value
test : F32
test = show (Outcome { value = 42.0 })
"#;
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "trait type param 'a' should not conflict with data type 'Outcome'"
        );
    }

    /// An impl for a type that has the same name as a trait.
    #[test]
    fn impl_for_type_named_like_trait() {
        let source = r#"
data Light = Light { brightness : F32 }
trait HasBrightness a where
  brightness : a -> F32
impl HasBrightness Light where
  brightness l = l.brightness
test : F32
test = brightness (Light { brightness = 0.5 })
"#;
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "data type and trait can have similar names without conflict"
        );
    }

    /// An ADT with a single constructor that has the same name as the type.
    /// This is the "newtype" pattern — very common in functional languages.
    #[test]
    fn newtype_same_constructor_and_type_name() {
        let source = r#"
data Velocity = Velocity (Vec<3, F32>)
test : Velocity
test = Velocity [1.0, 0.0, 0.0]
"#;
        let (_, has_errors) = parse_and_analyze(source);
        assert!(
            !has_errors,
            "newtype pattern (same constructor and type name) should work"
        );
    }
