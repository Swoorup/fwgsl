use std::sync::OnceLock;
use std::path::{Path, PathBuf};

use crate::parser::{Parser, Program};

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
    std::fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../prelude/prelude.shadml"))
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../prelude/prelude.shadml"))
}
