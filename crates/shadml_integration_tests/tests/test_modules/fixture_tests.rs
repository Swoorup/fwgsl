
    use super::*;

    const HELLO_SHADML: &str = include_str!("../../../../fixtures/hello.shadml");
    const ADT_SHADML: &str = include_str!("../../../../fixtures/adt.shadml");
    const PARTICLE_SHADML: &str = include_str!("../../../../fixtures/particle.shadml");
    const OPTION_RESULT_EXAMPLE: &str = include_str!("../../../../examples/option-result.shadml");
    const PRELUDE_UTILS_EXAMPLE: &str = include_str!("../../../../examples/prelude-utils.shadml");
    const TENSOR_ALIASES_EXAMPLE: &str = include_str!("../../../../examples/tensor-aliases.shadml");

    #[test]
    fn fixture_hello_lexes_without_errors() {
        let tokens = lex(HELLO_SHADML);
        let error_count = tokens
            .iter()
            .filter(|t| t.kind == SyntaxKind::Error)
            .count();
        assert_eq!(
            error_count, 0,
            "hello.shadml should lex without error tokens"
        );
    }

    #[test]
    fn fixture_adt_lexes_without_errors() {
        let tokens = lex(ADT_SHADML);
        let error_count = tokens
            .iter()
            .filter(|t| t.kind == SyntaxKind::Error)
            .count();
        assert_eq!(error_count, 0, "adt.shadml should lex without error tokens");
    }

    #[test]
    fn fixture_particle_lexes_without_errors() {
        let tokens = lex(PARTICLE_SHADML);
        let error_count = tokens
            .iter()
            .filter(|t| t.kind == SyntaxKind::Error)
            .count();
        assert_eq!(
            error_count, 0,
            "particle.shadml should lex without error tokens"
        );
    }

    #[test]
    fn fixture_hello_produces_declarations() {
        let (program, _) = parse(HELLO_SHADML);
        assert!(
            !program.decls.is_empty(),
            "hello.shadml should produce at least one declaration"
        );
    }

    #[test]
    fn fixture_adt_has_data_declaration() {
        let (program, _) = parse(ADT_SHADML);
        let has_data_decl = program
            .decls
            .iter()
            .any(|d| matches!(d, Decl::DataDecl { name, .. } if name == "Color"));
        assert!(
            has_data_decl,
            "adt.shadml should contain a Color data declaration"
        );
    }

    #[test]
    fn fixture_adt_registers_constructors() {
        let (program, _) = parse(ADT_SHADML);
        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);
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
    fn fixture_particle_has_data_declaration() {
        let (program, _) = parse(PARTICLE_SHADML);
        let has_particle_data = program
            .decls
            .iter()
            .any(|d| matches!(d, Decl::DataDecl { name, .. } if name == "ParticleState"));
        assert!(
            has_particle_data,
            "particle.shadml should have ParticleState data type"
        );
    }

    #[test]
    fn fixture_particle_has_two_constructors() {
        let (program, _) = parse(PARTICLE_SHADML);
        if let Some(Decl::DataDecl { constructors, .. }) = program
            .decls
            .iter()
            .find(|d| matches!(d, Decl::DataDecl { name, .. } if name == "ParticleState"))
        {
            assert_eq!(
                constructors.len(),
                2,
                "ParticleState should have 2 constructors (Active, Dead)"
            );
            assert_eq!(constructors[0].name, "Active");
            assert_eq!(constructors[1].name, "Dead");
        } else {
            panic!("ParticleState data declaration not found");
        }
    }

    #[test]
    fn example_option_result_type_checks() {
        let (_, has_errors) = parse_and_analyze(OPTION_RESULT_EXAMPLE);
        assert!(
            !has_errors,
            "option-result example should pass semantic analysis"
        );
    }

    #[test]
    fn example_prelude_utils_type_checks() {
        let (_, has_errors) = parse_and_analyze(PRELUDE_UTILS_EXAMPLE);
        assert!(
            !has_errors,
            "prelude-utils example should pass semantic analysis"
        );
    }

    #[test]
    fn example_tensor_aliases_type_checks() {
        let (_, has_errors) = parse_and_analyze(TENSOR_ALIASES_EXAMPLE);
        assert!(
            !has_errors,
            "tensor-aliases example should pass semantic analysis"
        );
    }
