use super::*;

#[test]
fn parse_full_program_has_at_least_4_decls() {
    let source = "\
data Color = Red | Green | Blue

show : Color -> I32
show c = match c
  | Red   -> 0
  | Green -> 1
  | Blue  -> 2

add : I32 -> I32 -> I32
add x y = x + y

main : I32 -> I32
main x =
  let y = add x 1
  in show Red
";
    let (program, _has_errors) = parse_raw(source);
    assert!(
        program.decls.len() >= 4,
        "full program should produce at least 4 declarations, got {}",
        program.decls.len()
    );
}

#[test]
fn parse_data_decl_is_first() {
    let source = "\
data Color = Red | Green | Blue

show c = match c
  | Red -> 0
  | Green -> 1
  | Blue -> 2
";
    let (program, _has_errors) = parse_raw(source);
    assert!(
        matches!(&program.decls[0], Decl::DataDecl { name, .. } if name == "Color"),
        "first declaration should be DataDecl Color, got {:?}",
        program.decls[0]
    );
}

#[test]
fn parse_comments_are_ignored_by_lexer() {
    let source = "-- This is a comment\nadd x y = x + y";
    let (program, has_errors) = parse_raw(source);
    assert!(!has_errors, "comments should not cause parse errors");
    assert!(
        program.decls.len() >= 1,
        "should have at least 1 declaration"
    );
}

#[test]
fn parse_block_comment_is_ignored() {
    let source = "{- block comment -} add x y = x + y";
    let (program, has_errors) = parse_raw(source);
    assert!(!has_errors, "block comments should not cause parse errors");
    assert_eq!(program.decls.len(), 1);
}
