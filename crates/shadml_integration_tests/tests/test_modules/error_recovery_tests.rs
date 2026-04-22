
    use super::*;

    #[test]
    fn parser_handles_empty_input() {
        let (program, has_errors) = parse_raw("");
        assert!(!has_errors, "empty input should not be a parse error");
        assert!(program.decls.is_empty());
    }

    #[test]
    fn parser_handles_only_whitespace() {
        let (_program, has_errors) = parse_raw("   \n\n  \n");
        assert!(!has_errors, "whitespace-only input should not error");
    }

    #[test]
    fn parser_does_not_panic_on_garbage_input() {
        let garbage_inputs = [
            "@@@@", "= = = =", "-> -> ->", "data", "data =", "let", "match", "| | | |", "( ( ( (",
            ") ) ) )", "{ { { {", "} } } }", "\\\\\\\\",
        ];
        for input in &garbage_inputs {
            let result = std::panic::catch_unwind(|| {
                let mut parser = Parser::new(input);
                let _program = parser.parse_program();
            });
            assert!(
                result.is_ok(),
                "parser should not panic on input: {:?}",
                input
            );
        }
    }

    #[test]
    fn lexer_does_not_panic_on_garbage_input() {
        let garbage_inputs = [
            "\0\0\0",
            "\"unterminated string",
            "'",
            "''",
            "0x",
            "0b",
            "0o",
            "/*",
            "///",
        ];
        for input in &garbage_inputs {
            let result = std::panic::catch_unwind(|| {
                let _tokens = lex(input);
            });
            assert!(
                result.is_ok(),
                "lexer should not panic on input: {:?}",
                input
            );
        }
    }

    #[test]
    fn parser_recovers_from_incomplete_function() {
        let result = std::panic::catch_unwind(|| {
            let mut parser = Parser::new("f x =");
            let _program = parser.parse_program();
        });
        assert!(
            result.is_ok(),
            "parser should not panic on incomplete function"
        );
    }

    #[test]
    fn parser_recovers_from_incomplete_data_decl() {
        let result = std::panic::catch_unwind(|| {
            let mut parser = Parser::new("data Color =");
            let _program = parser.parse_program();
        });
        assert!(
            result.is_ok(),
            "parser should not panic on incomplete data declaration"
        );
    }

    #[test]
    fn parser_handles_very_long_input_without_panic() {
        let mut source = String::new();
        for i in 0..100 {
            source.push_str(&format!("f{} x = x + {}\n", i, i));
        }
        let result = std::panic::catch_unwind(|| {
            let mut parser = Parser::new(&source);
            let _program = parser.parse_program();
        });
        assert!(
            result.is_ok(),
            "parser should handle large input without panic"
        );
    }

    #[test]
    fn semantic_analyzer_handles_empty_program() {
        let mut sa = SemanticAnalyzer::new();
        let program = Program { decls: vec![] };
        sa.analyze(&program);
        assert!(!sa.has_errors());
    }

    #[test]
    fn semantic_analyzer_handles_only_data_types() {
        let source = "data Color = Red | Green | Blue";
        let (program, _) = parse(source);
        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);
        assert!(
            !sa.has_errors(),
            "data-type-only program should not error in semantic analysis"
        );
    }

    #[test]
    fn parser_handles_nested_block_comments() {
        let source = "{- outer {- inner -} still outer -} f x = x";
        let (program, has_errors) = parse_raw(source);
        assert!(!has_errors, "nested block comments should not cause errors");
        assert!(
            !program.decls.is_empty(),
            "should parse declaration after comment"
        );
    }
