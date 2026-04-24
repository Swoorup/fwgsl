use super::*;

#[test]
fn parse_single_type_signature() {
    let source = "add : I32 -> I32 -> I32";
    let (program, has_errors) = parse_raw(source);
    assert!(!has_errors, "single type sig should parse without errors");
    assert_eq!(program.decls.len(), 1);
    assert!(
        matches!(&program.decls[0], Decl::TypeSig { name, .. } if name == "add"),
        "expected TypeSig, got {:?}",
        program.decls[0]
    );
}

#[test]
fn parse_dependent_array_type_signature() {
    let source = "grid : Tensor 2 (Tensor 4 F32)";
    let (program, has_errors) = parse_raw(source);
    assert!(
        !has_errors,
        "dependent array type sig should parse without errors"
    );
    assert_eq!(program.decls.len(), 1);
    assert!(
        matches!(&program.decls[0], Decl::TypeSig { .. }),
        "expected TypeSig, got {:?}",
        program.decls[0]
    );
}

#[test]
fn parse_single_function() {
    let source = "add x y = x + y";
    let (program, has_errors) = parse_raw(source);
    assert!(!has_errors, "single function should parse without errors");
    assert_eq!(program.decls.len(), 1);
    assert!(
        matches!(&program.decls[0], Decl::FunDecl { name, params, .. } if name == "add" && params.len() == 2),
        "expected FunDecl with 2 params, got {:?}",
        program.decls[0]
    );
}

#[test]
fn parse_data_type_declaration() {
    let source = "data Color = Red | Green | Blue";
    let (program, has_errors) = parse_raw(source);
    assert!(!has_errors, "data declaration should parse without errors");
    assert_eq!(program.decls.len(), 1);
    if let Decl::DataDecl {
        name,
        constructors,
        type_params,
        ..
    } = &program.decls[0]
    {
        assert_eq!(name, "Color");
        assert!(type_params.is_empty());
        assert_eq!(constructors.len(), 3);
        assert_eq!(constructors[0].name, "Red");
        assert_eq!(constructors[1].name, "Green");
        assert_eq!(constructors[2].name, "Blue");
    } else {
        panic!("expected DataDecl, got {:?}", program.decls[0]);
    }
}

#[test]
fn parse_generic_data_type_declaration() {
    let source = "data Box a = Box a";
    let (program, has_errors) = parse_raw(source);
    assert!(
        !has_errors,
        "generic data declaration should parse without errors"
    );
    assert_eq!(program.decls.len(), 1);
    if let Decl::DataDecl {
        name,
        constructors,
        type_params,
        ..
    } = &program.decls[0]
    {
        assert_eq!(name, "Box");
        assert_eq!(type_params, &vec!["a".to_string()]);
        assert_eq!(constructors.len(), 1);
        assert_eq!(constructors[0].name, "Box");
    } else {
        panic!("expected DataDecl, got {:?}", program.decls[0]);
    }
}

#[test]
fn parse_function_with_inline_let() {
    let source = "f x = let y = x + 1 in y";
    let (program, has_errors) = parse_raw(source);
    assert!(!has_errors, "function with let should parse without errors");
    if let Decl::FunDecl { body, .. } = &program.decls[0] {
        assert!(
            matches!(body, Expr::Let(binds, _, _) if binds.len() == 1),
            "body should be a let expression with one binding, got {:?}",
            body
        );
    } else {
        panic!("expected FunDecl");
    }
}

#[test]
fn parse_function_with_where_clause() {
    let source = "f x = y + 1 where y = x";
    let (program, has_errors) = parse_raw(source);
    assert!(
        !has_errors,
        "function with where should parse without errors"
    );
    if let Decl::FunDecl {
        body, where_binds, ..
    } = &program.decls[0]
    {
        assert!(
            matches!(body, Expr::Infix(_, op, _, _) if op == "+"),
            "body should remain the main expression, got {:?}",
            body
        );
        assert_eq!(where_binds.len(), 1, "expected one where binding");
        assert_eq!(where_binds[0].name, "y");
    } else {
        panic!("expected FunDecl");
    }
}

#[test]
fn parse_lambda_expression() {
    let source = "f = \\x y -> x + y";
    let (program, has_errors) = parse_raw(source);
    assert!(!has_errors, "lambda should parse without errors");
    assert_eq!(program.decls.len(), 1);
    if let Decl::FunDecl { body, .. } = &program.decls[0] {
        assert!(
            matches!(body, Expr::Lambda(params, _, _) if params.len() == 2),
            "body should be a lambda with 2 params, got {:?}",
            body
        );
    } else {
        panic!("expected FunDecl");
    }
}

#[test]
fn parse_if_expression() {
    let source = "f x = if x == 0 then 1 else 2";
    let (program, has_errors) = parse_raw(source);
    assert!(!has_errors, "if expression should parse without errors");
    if let Decl::FunDecl { body, .. } = &program.decls[0] {
        assert!(
            matches!(body, Expr::If(_, _, _, _)),
            "body should be an if expression, got {:?}",
            body
        );
    } else {
        panic!("expected FunDecl");
    }
}

#[test]
fn parse_case_expression() {
    let source = "f c = match c\n  | Red -> 0\n  | Green -> 1\n  | Blue -> 2";
    let (program, has_errors) = parse_raw(source);
    assert!(!has_errors, "case expression should parse without errors");
    if let Decl::FunDecl { body, .. } = &program.decls[0] {
        assert!(
            matches!(body, Expr::Case(_, arms, _) if arms.len() == 3),
            "body should be a case expression with 3 arms, got {:?}",
            body
        );
    } else {
        panic!("expected FunDecl");
    }
}

#[test]
fn parse_infix_operators() {
    let source = "f x y = x + y * 2 - 1";
    let (program, has_errors) = parse_raw(source);
    assert!(!has_errors, "infix operators should parse without errors");
    assert_eq!(program.decls.len(), 1);
}

#[test]
fn parse_operator_section() {
    let source = "f = (+)";
    let (program, has_errors) = parse_raw(source);
    assert!(!has_errors, "operator section should parse without errors");
    if let Decl::FunDecl { body, .. } = &program.decls[0] {
        assert!(
            matches!(body, Expr::OpSection(..)),
            "body should be an operator section, got {:?}",
            body
        );
    } else {
        panic!("expected FunDecl");
    }
}

#[test]
fn parse_negation() {
    let source = "neg x = -x";
    let (program, has_errors) = parse_raw(source);
    assert!(!has_errors, "negation should parse without errors");
    if let Decl::FunDecl { body, .. } = &program.decls[0] {
        assert!(
            matches!(body, Expr::Neg(_, _)),
            "body should be a negation expression, got {:?}",
            body
        );
    } else {
        panic!("expected FunDecl");
    }
}

#[test]
fn parse_parenthesized_expression() {
    let source = "f x = (x + 1) * 2";
    let (program, has_errors) = parse_raw(source);
    assert!(
        !has_errors,
        "parenthesized expression should parse without errors"
    );
    assert_eq!(program.decls.len(), 1);
}

#[test]
fn parse_tuple() {
    let source = "f = (1, 2, 3)";
    let (program, has_errors) = parse_raw(source);
    assert!(!has_errors, "tuple should parse without errors");
    if let Decl::FunDecl { body, .. } = &program.decls[0] {
        assert!(
            matches!(body, Expr::Tuple(elems, _) if elems.len() == 3),
            "body should be a tuple with 3 elements, got {:?}",
            body
        );
    } else {
        panic!("expected FunDecl");
    }
}

#[test]
fn parse_unit() {
    let source = "f = ()";
    let (program, has_errors) = parse_raw(source);
    assert!(!has_errors, "unit should parse without errors");
    if let Decl::FunDecl { body, .. } = &program.decls[0] {
        assert!(
            matches!(body, Expr::Tuple(elems, _) if elems.is_empty()),
            "body should be an empty tuple (unit), got {:?}",
            body
        );
    } else {
        panic!("expected FunDecl");
    }
}

#[test]
fn parse_entry_point() {
    let source = "@vertex\nmain x = x + 1";
    let (program, has_errors) = parse_raw(source);
    assert!(!has_errors, "entry point should parse without errors");
    assert_eq!(program.decls.len(), 1);
    if let Decl::EntryPoint {
        attributes, name, ..
    } = &program.decls[0]
    {
        assert_eq!(name, "main");
        assert_eq!(attributes.len(), 1);
        assert_eq!(attributes[0].name, "vertex");
    } else {
        panic!("expected EntryPoint, got {:?}", program.decls[0]);
    }
}

#[test]
fn parse_compute_entry_point_with_workgroup_size() {
    // The parser requires parenthesized attribute arguments: @workgroup_size(64, 1, 1)
    let source = "@compute @workgroup_size(64, 1, 1)\nmain x = x + 1";
    let (program, has_errors) = parse_raw(source);
    assert!(
        !has_errors,
        "compute entry point should parse without errors"
    );
    assert_eq!(program.decls.len(), 1);
    if let Decl::EntryPoint { attributes, .. } = &program.decls[0] {
        assert!(
            attributes.len() >= 2,
            "expected at least 2 attributes, got {}",
            attributes.len()
        );
        assert_eq!(attributes[0].name, "compute");
        assert_eq!(attributes[1].name, "workgroup_size");
        match attributes[1].args.as_slice() {
            [AttrArg::Positional(AttrValue::Int(64)), AttrArg::Positional(AttrValue::Int(1)), AttrArg::Positional(AttrValue::Int(1))] =>
                {}
            other => panic!("expected [64, 1, 1], got {:?}", other),
        }
    } else {
        panic!("expected EntryPoint, got {:?}", program.decls[0]);
    }
}

#[test]
fn parse_precedence_mul_over_add() {
    let source = "f = 1 + 2 * 3";
    let (program, has_errors) = parse_raw(source);
    assert!(!has_errors);
    if let Decl::FunDecl { body, .. } = &program.decls[0] {
        assert!(
            matches!(body, Expr::Infix(_, op, _, _) if op == "+"),
            "top-level infix should be +, got {:?}",
            body
        );
    } else {
        panic!("expected FunDecl");
    }
}

#[test]
fn parse_nested_let() {
    let source = "f x = let a = 1 in let b = 2 in a + b";
    let (program, has_errors) = parse_raw(source);
    assert!(!has_errors, "nested let should parse without errors");
    if let Decl::FunDecl { body, .. } = &program.decls[0] {
        assert!(
            matches!(body, Expr::Let(_, inner, _) if matches!(inner.as_ref(), Expr::Let(_, _, _))),
            "body should be nested let expressions, got {:?}",
            body
        );
    } else {
        panic!("expected FunDecl");
    }
}

#[test]
fn parse_multiline_let_in() {
    let source = "f x =\n  let y = x + 1\n  in y * 2";
    let (program, has_errors) = parse_raw(source);
    assert!(!has_errors, "multiline let-in should parse without errors");
    if let Decl::FunDecl { body, .. } = &program.decls[0] {
        assert!(
            matches!(body, Expr::Let(_, _, _)),
            "body should be a let expression, got {:?}",
            body
        );
    } else {
        panic!("expected FunDecl");
    }
}
