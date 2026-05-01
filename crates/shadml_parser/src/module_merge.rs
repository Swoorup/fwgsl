//! Module merging: combine a dependency-ordered set of modules into a single
//! flat program suitable for semantic analysis and compilation.
//!
//! All declarations are included. The output is a single `Program` with all
//! declarations from all modules, with module/import declarations stripped.

use crate::module_resolver::ModuleGraph;
use crate::parser::{Decl, Program};

/// Return every name that a declaration makes available to an importer.
///
/// Rules per `Decl` variant:
/// | Variant | Exported names |
/// |---|---|
/// | `FunDecl { name, .. }` | `[name]` |
/// | `DataDecl { name, constructors, .. }` | `[name] ++ constructors.map(|c| c.name)` |
/// | `TypeAlias { name, .. }` | `[name]` |
/// | `TraitDecl { name, methods, .. }` | `[name] ++ methods.map(|m| m.name)` |
/// | `ImplDecl { methods, .. }` | `methods.map(|m| m.name)` |
/// | `BindingDecl { name, .. }` | `[name]` |
/// | `EntryPoint { name, .. }` | `[name]` |
/// | `BuiltinImplDecl { methods, .. }` | `methods.map(|m| m.name)` |
/// | `ExternDecl { name, .. }` | `[name]` |
/// | `BuiltinExternDecl { name, .. }` | `[name]` |
/// | `RenderBlock { name, .. }` | `[name]` |
/// | `ModuleDecl \| ImportDecl \| CfgDecl \| TypeSig \| BuiltinTypeDecl` | `[]` |
pub fn exported_names(decl: &Decl) -> Vec<String> {
    match decl {
        Decl::FunDecl { name, .. }
        | Decl::TypeAlias { name, .. }
        | Decl::BindingDecl { name, .. }
        | Decl::EntryPoint { name, .. }
        | Decl::ExternDecl { name, .. }
        | Decl::BuiltinExternDecl { name, .. }
        | Decl::RenderBlock { name, .. } => vec![name.clone()],
        Decl::DataDecl {
            name, constructors, ..
        } => {
            let mut names = vec![name.clone()];
            names.extend(constructors.iter().map(|c| c.name.clone()));
            names
        }
        Decl::TraitDecl { name, methods, .. } => {
            let mut names = vec![name.clone()];
            names.extend(methods.iter().map(|m| m.name.clone()));
            names
        }
        Decl::ImplDecl { methods, .. } => methods.iter().map(|m| m.name.clone()).collect(),
        Decl::BuiltinImplDecl { methods, .. } => methods.iter().map(|m| m.name.clone()).collect(),
        Decl::ModuleDecl { .. }
        | Decl::ImportDecl { .. }
        | Decl::CfgDecl { .. }
        | Decl::TypeSig { .. }
        | Decl::BuiltinTypeDecl { .. }
        | Decl::ConstDecl { .. }
        | Decl::BitfieldDecl { .. } => vec![],
    }
}

/// Merge a module graph into a single flat program.
///
/// Processing order (dependency first):
/// For each module in topological order, collect all non-module/import
/// declarations into the merged program.
pub fn merge_modules(graph: &ModuleGraph) -> Program {
    let mut merged_decls: Vec<Decl> = Vec::new();

    for module in &graph.modules {
        for decl in &module.program.decls {
            match decl {
                Decl::ModuleDecl { .. } | Decl::ImportDecl { .. } => {
                    // Skip module/import declarations — they're metadata only
                }
                _ => {
                    merged_decls.push(decl.clone());
                }
            }
        }
    }

    Program {
        decls: merged_decls,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module_resolver::ParsedModule;
    use crate::parser::Parser;
    use std::path::PathBuf;

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
                    Some(crate::module_resolver::ModuleImport {
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
    fn merge_single_module() {
        let module = parse_module("Main", "add x y = x + y");
        let graph = ModuleGraph {
            modules: vec![module],
        };
        let merged = merge_modules(&graph);
        assert_eq!(merged.decls.len(), 1);
        assert!(matches!(&merged.decls[0], Decl::FunDecl { name, .. } if name == "add"));
    }

    #[test]
    fn merge_strips_module_and_import_decls() {
        let module = parse_module("Main", "module Main\nimport Utils\nadd x y = x + y");
        let graph = ModuleGraph {
            modules: vec![module],
        };
        let merged = merge_modules(&graph);
        // Only the FunDecl should remain
        assert_eq!(merged.decls.len(), 1);
        assert!(matches!(&merged.decls[0], Decl::FunDecl { name, .. } if name == "add"));
    }

    #[test]
    fn merge_two_modules_dependency_order() {
        let utils = parse_module("Utils", "module Utils\nhelper x = x");
        let main = parse_module("Main", "module Main\nimport Utils\nadd x y = x + y");
        let graph = ModuleGraph {
            modules: vec![utils, main],
        };
        let merged = merge_modules(&graph);
        assert_eq!(merged.decls.len(), 2);
        // Utils' helper should come first (dependency order)
        assert!(matches!(&merged.decls[0], Decl::FunDecl { name, .. } if name == "helper"));
        assert!(matches!(&merged.decls[1], Decl::FunDecl { name, .. } if name == "add"));
    }

    #[test]
    fn merge_includes_data_types_from_imports() {
        let types = parse_module("Types", "module Types\ndata Color = Red | Green | Blue");
        let main = parse_module("Main", "module Main\nimport Types\nf x = Red");
        let graph = ModuleGraph {
            modules: vec![types, main],
        };
        let merged = merge_modules(&graph);
        // Should contain DataDecl from Types + FunDecl from Main
        let has_data = merged
            .decls
            .iter()
            .any(|d| matches!(d, Decl::DataDecl { name, .. } if name == "Color"));
        let has_fun = merged
            .decls
            .iter()
            .any(|d| matches!(d, Decl::FunDecl { name, .. } if name == "f"));
        assert!(has_data, "should include Color data type");
        assert!(has_fun, "should include f function");
    }

    // ── exported_names tests ───────────────────────────────────────────────

    #[test]
    fn exported_names_fun_decl() {
        let module = parse_module("M", "f x = x");
        let decl = &module.program.decls[0];
        assert_eq!(exported_names(decl), vec!["f"]);
    }

    #[test]
    fn exported_names_data_decl() {
        let module = parse_module("M", "data Color = Red | Green | Blue");
        let decl = &module.program.decls[0];
        let names = exported_names(decl);
        assert!(names.contains(&"Color".to_string()));
        assert!(names.contains(&"Red".to_string()));
        assert!(names.contains(&"Green".to_string()));
        assert!(names.contains(&"Blue".to_string()));
        assert_eq!(names.len(), 4);
    }

    #[test]
    fn exported_names_type_alias() {
        let module = parse_module("M", "alias Int = I32");
        let decl = &module.program.decls[0];
        assert_eq!(exported_names(decl), vec!["Int"]);
    }

    #[test]
    fn exported_names_trait_decl() {
        let module = parse_module("M", "trait Num a where\n  (+) : a -> a -> a\n  zero : a");
        let decl = &module.program.decls[0];
        let names = exported_names(decl);
        assert!(names.contains(&"Num".to_string()));
        assert!(names.contains(&"+".to_string()));
        assert!(names.contains(&"zero".to_string()));
        assert_eq!(names.len(), 3);
    }

    #[test]
    fn exported_names_impl_decl() {
        let module = parse_module("M", "impl I32 where\n  half : I32 -> I32\n  half x = x");
        let decl = &module.program.decls[0];
        let names = exported_names(decl);
        assert!(names.contains(&"half".to_string()));
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn exported_names_binding_decl() {
        let module = parse_module("M", "@group(0) @binding(0) uniform globals : Globals");
        let decl = &module.program.decls[0];
        assert_eq!(exported_names(decl), vec!["globals"]);
    }

    #[test]
    fn exported_names_entry_point() {
        let module = parse_module("M", "@vertex\nmain x = x + 1");
        let decl = &module.program.decls[0];
        assert_eq!(exported_names(decl), vec!["main"]);
    }

    #[test]
    fn exported_names_builtin_impl_decl() {
        let module = parse_module("M", "builtin impl Add I32 where\n  (+) = native_binop(+)");
        let decl = &module.program.decls[0];
        assert_eq!(exported_names(decl), vec!["+"]);
    }

    #[test]
    fn exported_names_extern_decl() {
        let module = parse_module("M", "extern sin : F32 -> F32");
        let decl = &module.program.decls[0];
        assert_eq!(exported_names(decl), vec!["sin"]);
    }

    #[test]
    fn exported_names_builtin_extern_decl() {
        let module = parse_module("M", "builtin extern sin : F32 -> F32 = intrinsic(sin)");
        let decl = &module.program.decls[0];
        assert_eq!(exported_names(decl), vec!["sin"]);
    }

    #[test]
    fn exported_names_render_block() {
        let module = parse_module("M", "render test\n  @vertex\n  vsMain x = x");
        let decl = &module.program.decls[0];
        assert_eq!(exported_names(decl), vec!["test"]);
    }

    #[test]
    fn exported_names_module_decl_is_empty() {
        let module = parse_module("M", "module M");
        let decl = &module.program.decls[0];
        assert!(exported_names(decl).is_empty());
    }

    #[test]
    fn exported_names_import_decl_is_empty() {
        let module = parse_module("M", "import Foo");
        let decl = &module.program.decls[0];
        assert!(exported_names(decl).is_empty());
    }

    #[test]
    fn exported_names_type_sig_is_empty() {
        let module = parse_module("M", "f : I32 -> I32");
        let decl = &module.program.decls[0];
        assert!(exported_names(decl).is_empty());
    }

    #[test]
    fn exported_names_builtin_type_decl_is_empty() {
        let module = parse_module("M", "builtin type Vec 2");
        let decl = &module.program.decls[0];
        assert!(exported_names(decl).is_empty());
    }
}
