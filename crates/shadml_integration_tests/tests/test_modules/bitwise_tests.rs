
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

        Ok(shadml_wgsl_codegen::emit_wgsl(&mir))
    }

    #[test]
    fn test_bitwise_and() {
        let source = "testAnd : U32 -> U32 -> U32\ntestAnd x y = x & y";
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("x & y"),
            "WGSL should contain bitwise AND, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_bitwise_xor() {
        let source = "testXor : U32 -> U32 -> U32\ntestXor x y = x ^ y";
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("x ^ y"),
            "WGSL should contain bitwise XOR, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_shift_left() {
        let source = "testShl : U32 -> U32 -> U32\ntestShl x y = x << y";
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("x << y"),
            "WGSL should contain shift left, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_shift_right_infix() {
        let source = "testShr : U32 -> U32 -> U32\ntestShr x y = x >> y";
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("x >> y"),
            "WGSL should contain shift right, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_user_builtin_extern_is_legal() {
        let source = r#"
builtin extern wave : F32 -> F32 = intrinsic(sin)

testWave : F32 -> F32
testWave x = wave x
"#;
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("sin(x)"),
            "WGSL should contain the user-declared builtin extern lowering, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_bitwise_not() {
        let source = "testNot : U32 -> U32\ntestNot x = ~x";
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("~x"),
            "WGSL should contain bitwise NOT, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_bitwise_or_builtin() {
        let source = "testOr : U32 -> U32 -> U32\ntestOr x y = bor x y";
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("x | y"),
            "WGSL should contain bitwise OR via bor, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_bitwise_operator_precedence() {
        // & should bind tighter than ^ — WGSL has the same precedence, so no parens needed
        let source = "testPrec : U32 -> U32 -> U32 -> U32\ntestPrec a b c = a ^ b & c";
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("a ^ b & c"),
            "WGSL should show & binding tighter than ^, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_bitwise_combined() {
        let source = "mask : U32 -> U32 -> U32\nmask flags bit = flags & (~bit)";
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("&") && wgsl.contains("~"),
            "WGSL should contain & and ~, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_bitand_trait_overload() {
        let source = r#"
data Mask = Mask { bits : U32 }

impl BitAnd Mask Mask where
  type Output = Mask
  (&) a b = Mask { bits = a.bits & b.bits }

andMask : Mask -> Mask -> Mask
andMask a b = a & b
"#;
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("fn bitand_Mask__Mask("),
            "WGSL should contain mangled bitand impl, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("bitand_Mask__Mask(a, b)"),
            "WGSL should dispatch & to the mangled BitAnd impl, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_bitxor_trait_overload() {
        let source = r#"
data Mask = Mask { bits : U32 }

impl BitXor Mask Mask where
  type Output = Mask
  (^) a b = Mask { bits = a.bits ^ b.bits }

xorMask : Mask -> Mask -> Mask
xorMask a b = a ^ b
"#;
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("fn bitxor_Mask__Mask("),
            "WGSL should contain mangled bitxor impl, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("bitxor_Mask__Mask(a, b)"),
            "WGSL should dispatch ^ to the mangled BitXor impl, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_shl_trait_overload() {
        let source = r#"
data Mask = Mask { bits : U32 }

impl Shl Mask Mask where
  type Output = Mask
  (<<) a b = Mask { bits = a.bits << b.bits }

shlMask : Mask -> Mask -> Mask
shlMask a b = a << b
"#;
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("fn shl_Mask__Mask("),
            "WGSL should contain mangled shl impl, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("shl_Mask__Mask(a, b)"),
            "WGSL should dispatch << to the mangled Shl impl, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_shr_trait_overload() {
        let source = r#"
data Mask = Mask { bits : U32 }

impl Shr Mask Mask where
  type Output = Mask
  (>>) a b = Mask { bits = a.bits >> b.bits }

shrMask : Mask -> Mask -> Mask
shrMask a b = a >> b
"#;
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("fn shr_Mask__Mask("),
            "WGSL should contain mangled shr impl, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("shr_Mask__Mask(a, b)"),
            "WGSL should dispatch >> to the mangled Shr impl, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_bitnot_trait_overload() {
        let source = r#"
data Mask = Mask { bits : U32 }

impl BitNot Mask where
  (~) a = Mask { bits = ~a.bits }

notMask : Mask -> Mask
notMask a = ~a
"#;
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("fn bitnot_Mask("),
            "WGSL should contain mangled bitnot impl, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("bitnot_Mask(a)"),
            "WGSL should dispatch ~ to bitnot_Mask, got: {}",
            wgsl
        );
    }

    #[test]
    fn test_neg_trait_overload() {
        let source = r#"
data Wrapper = Wrapper { val : F32 }

impl Neg Wrapper where
  (-) a = Wrapper { val = -a.val }

negWrapper : Wrapper -> Wrapper
negWrapper a = -a
"#;
        let wgsl = compile_to_wgsl(source).expect("should compile");
        assert!(
            wgsl.contains("fn negate_Wrapper("),
            "WGSL should contain mangled negate impl, got: {}",
            wgsl
        );
        assert!(
            wgsl.contains("negate_Wrapper(a)"),
            "WGSL should dispatch - to negate_Wrapper, got: {}",
            wgsl
        );
    }
