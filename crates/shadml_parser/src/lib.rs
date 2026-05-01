pub mod lexer;
pub mod module_resolver;
pub mod parser;

pub(crate) mod cfg_eval;
pub(crate) mod layout;
pub(crate) mod module_merge;
pub(crate) mod prelude;
pub(crate) mod reachability;
pub(crate) mod renamer;
pub(crate) mod virtual_fs;

pub use cfg_eval::{evaluate_features, FeatureSet};
pub use layout::resolve_layout;
pub use lexer::{lex, Token};
pub use module_merge::{exported_names, merge_modules};
pub use module_resolver::{resolve_modules, FsReader, ModuleGraph, ParsedModule, SourceReader};
pub use parser::{CfgPredicate, Parser, Program};
pub use prelude::{
    has_imports, has_imports_in, prelude_path, prelude_program, prelude_source,
    should_prepend_prelude, with_prelude,
};
pub use reachability::{filter_live_program, find_live_decl_indices};
pub use renamer::Renamer;
pub use virtual_fs::{parse_bundle, VirtualFs};
