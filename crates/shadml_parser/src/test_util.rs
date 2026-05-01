use std::path::PathBuf;

use crate::module_resolver::{ModuleGraph, ModuleImport, ParsedModule};
use crate::parser::{Decl, Parser};

pub fn parse_module(name: &str, source: &str) -> ParsedModule {
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
                Some(ModuleImport {
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

pub fn build_graph(modules: Vec<ParsedModule>) -> ModuleGraph {
    ModuleGraph { modules }
}
