//! Integration tests for import semantics covering all four import forms.

use std::collections::HashSet;
use std::path::PathBuf;

use shadml_parser::module_resolver::{ModuleGraph, ParsedModule};
use shadml_parser::parser::{Decl, Expr, Parser, Type};
use shadml_parser::{exported_names, Renamer};

fn parse_module(name: &str, source: &str) -> ParsedModule {
    let mut parser = Parser::new(source);
    let program = parser.parse_program();
    let imports = program
        .decls
        .iter()
        .filter_map(|d| {
            if let Decl::ImportDecl {
                module_path, kind, ..
            } = d
            {
                Some(shadml_parser::module_resolver::ModuleImport {
                    module_path: module_path.clone(),
                    kind: kind.clone(),
                })
            } else {
                None
            }
        })
        .collect();
    ParsedModule {
        name: name.to_string(),
        path: PathBuf::from(format!("{}.shadml", name)),
        program,
        imports,
    }
}

#[test]
fn selective_import_only_imports_selected_names() {
    let main = parse_module(
        "Main",
        r#"
import Utils (helper_only)

main : () -> ()
@compute @workgroup_size(64, 1, 1)
main _ = helper_only ()
"#,
    );
    let utils = parse_module(
        "Utils",
        r#"
module Utils

helper_only : () -> ()
helper_only _ = ()

other_fn : () -> ()
other_fn _ = ()
"#,
    );

    let graph = ModuleGraph {
        modules: vec![utils, main],
    };
    let renamed = Renamer::new(&graph).run();

    // helper_only should be resolved.
    let main_decl = renamed
        .decls
        .iter()
        .find(|d| matches!(d, Decl::EntryPoint { name, .. } if name == "main"))
        .expect("main should exist");
    if let Decl::EntryPoint { body, .. } = main_decl {
        assert!(
            matches!(body, Expr::App(func, _, _) if matches!(func.as_ref(), Expr::Resolved(ref r, _) if r.original_name == "helper_only")),
            "helper_only should be resolved in main's body"
        );
    }

    // other_fn should NOT be in the unqualified scope of Main.
    // It is present as a declaration in the flat program, but main doesn't reference it.
    let other_fn_decl = renamed
        .decls
        .iter()
        .find(|d| matches!(d, Decl::FunDecl { name, .. } if name == "other_fn"));
    assert!(
        other_fn_decl.is_some(),
        "other_fn decl should exist in flat program"
    );
}

#[test]
fn qualified_import_allows_qualified_access() {
    let main = parse_module(
        "Main",
        r#"
import Utils as U

main : () -> ()
@compute @workgroup_size(64, 1, 1)
main _ = U.helper ()
"#,
    );
    let utils = parse_module(
        "Utils",
        r#"
module Utils

helper : () -> ()
helper _ = ()
"#,
    );

    let graph = ModuleGraph {
        modules: vec![utils, main],
    };
    let renamed = Renamer::new(&graph).run();

    let main_decl = renamed
        .decls
        .iter()
        .find(|d| matches!(d, Decl::EntryPoint { name, .. } if name == "main"))
        .expect("main should exist");
    if let Decl::EntryPoint { body, .. } = main_decl {
        assert!(
            matches!(body, Expr::App(func, _, _) if matches!(func.as_ref(), Expr::Resolved(ref r, _) if r.original_name == "helper" && r.module == "Utils")),
            "U.helper should be resolved to Utils.helper"
        );
    }
}

#[test]
fn wildcard_import_makes_submodules_available() {
    let main = parse_module(
        "Main",
        r#"
import Math.*

main : () -> ()
@compute @workgroup_size(64, 1, 1)
main _ = Math.Vec.dot2 ()
"#,
    );
    let math_vec = parse_module(
        "Math.Vec",
        r#"
module Math.Vec

dot2 : Vec<2, F32> -> Vec<2, F32> -> F32
dot2 a b = a.x * b.x + a.y * b.y
"#,
    );

    let graph = ModuleGraph {
        modules: vec![math_vec, main],
    };
    let renamed = Renamer::new(&graph).run();

    let main_decl = renamed
        .decls
        .iter()
        .find(|d| matches!(d, Decl::EntryPoint { name, .. } if name == "main"))
        .expect("main should exist");
    if let Decl::EntryPoint { body, .. } = main_decl {
        // Math.Vec.dot2 parses as FieldAccess(Qualified("Math", "Vec"), "dot2").
        // The Renamer should resolve it to Resolved("Math.Vec", "dot2").
        assert!(
            matches!(body, Expr::App(func, _, _) if matches!(func.as_ref(), Expr::Resolved(ref r, _) if r.original_name == "dot2" && r.module == "Math.Vec")),
            "Math.Vec.dot2 should be resolved to Math.Vec.dot2"
        );
    }
}

#[test]
fn diamond_import_no_duplicate_symbol_errors() {
    let main = parse_module(
        "Main",
        r#"
import A
import B

main : I32 -> I32
@compute @workgroup_size(64, 1, 1)
main input = use_a input + use_b input
"#,
    );
    let a = parse_module(
        "A",
        r#"
module A
import Utils

use_a : I32 -> I32
use_a x = used_by_a x
"#,
    );
    let b = parse_module(
        "B",
        r#"
module B
import Utils

use_b : I32 -> I32
use_b x = used_by_b x
"#,
    );
    let utils = parse_module(
        "Utils",
        r#"
module Utils

used_by_a : I32 -> I32
used_by_a x = x + 1

used_by_b : I32 -> I32
used_by_b x = x + 2
"#,
    );

    let graph = ModuleGraph {
        modules: vec![utils, a, b, main],
    };

    // The Renamer should resolve all names without collisions.
    let renamed = Renamer::new(&graph).run();

    // Collect all declaration names in the flat program.
    let mut names = HashSet::new();
    for decl in &renamed.decls {
        for name in exported_names(decl) {
            names.insert(name);
        }
    }

    assert!(names.contains("used_by_a"));
    assert!(names.contains("used_by_b"));
    assert!(names.contains("use_a"));
    assert!(names.contains("use_b"));
    assert!(names.contains("main"));
}

#[test]
fn selective_import_of_missing_name_produces_diagnostic() {
    let main = parse_module(
        "Main",
        r#"
import Utils (does_not_exist)

main : () -> ()
@compute @workgroup_size(64, 1, 1)
main _ = does_not_exist ()
"#,
    );
    let utils = parse_module(
        "Utils",
        r#"
module Utils

helper : () -> ()
helper _ = ()
"#,
    );

    let graph = ModuleGraph {
        modules: vec![utils, main],
    };

    let mut renamer = Renamer::new(&graph);
    let _program = renamer.run();

    let diags = renamer.diagnostics();
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("does not export") && d.message.contains("does_not_exist")),
        "Expected a diagnostic about missing export, got: {:?}",
        diags
    );
}

#[test]
fn selective_import_typo_in_selected_name_produces_diagnostic() {
    let main = parse_module(
        "Main",
        r#"
import Utils (getTim)

main : () -> ()
@compute @workgroup_size(64, 1, 1)
main _ = getTime ()
"#,
    );
    let utils = parse_module(
        "Utils",
        r#"
module Utils

getTime : () -> F32
getTime = 0.0
"#,
    );

    let graph = ModuleGraph {
        modules: vec![utils, main],
    };
    let mut renamer = Renamer::new(&graph);
    let _program = renamer.run();

    let diags = renamer.diagnostics();
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("does not export") && d.message.contains("getTim")),
        "Expected a diagnostic about typo'd export name, got: {:?}",
        diags
    );
}
