use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::parser::{Decl, Parser, Program};

const PRELUDE_SOURCE: &str = include_str!("../../../prelude/prelude.shadml");

static PRELUDE: OnceLock<Program> = OnceLock::new();

/// Returns the parsed prelude program (cached after first call).
pub fn prelude_program() -> &'static Program {
    PRELUDE.get_or_init(|| {
        let mut parser = Parser::with_builtin_decls(PRELUDE_SOURCE, true);
        let program = parser.parse_program();
        assert!(
            !parser.diagnostics().has_errors(),
            "prelude parse errors: {:?}",
            parser.diagnostics().iter().collect::<Vec<_>>()
        );
        program
    })
}

pub fn prelude_source() -> &'static str {
    PRELUDE_SOURCE
}

pub fn prelude_path() -> PathBuf {
    std::fs::canonicalize(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../prelude/prelude.shadml"),
    )
    .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../prelude/prelude.shadml"))
}

/// Prepend prelude declarations to a parsed program.
///
/// When `skip_if_compiler_prelude` is `true` and the program is the
/// prelude file itself, this is a no-op to avoid infinite recursion.
pub fn with_prelude(program: &mut Program, skip_if_compiler_prelude: bool) {
    if skip_if_compiler_prelude {
        return;
    }
    let prelude = prelude_program();
    let mut combined = prelude.decls.clone();
    combined.append(&mut program.decls);
    program.decls = combined;
}

/// Check whether `file` is the prelude file itself.
pub fn should_prepend_prelude(file: &str) -> bool {
    std::path::Path::new(file)
        .file_name()
        .and_then(|name| name.to_str())
        != Some("prelude.shadml")
}

/// Check if the program has import declarations (needs multi-file resolution).
pub fn has_imports(program: &Program) -> bool {
    has_imports_in(&program.decls)
}

/// Check if any declaration in the slice is an import (recursively).
pub fn has_imports_in(decls: &[Decl]) -> bool {
    decls.iter().any(|d| match d {
        Decl::ImportDecl { .. } => true,
        Decl::CfgDecl {
            then_decls,
            else_decls,
            ..
        } => has_imports_in(then_decls) || has_imports_in(else_decls),
        _ => false,
    })
}
