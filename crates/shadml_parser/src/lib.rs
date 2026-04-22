pub mod lexer;
pub mod parser;
pub mod module_resolver;

pub(crate) mod cfg_eval;
pub(crate) mod layout;
pub(crate) mod module_merge;
pub(crate) mod prelude;
pub(crate) mod virtual_fs;

pub use cfg_eval::{evaluate_features, FeatureSet};
pub use layout::resolve_layout;
pub use lexer::{lex, Token};
pub use module_merge::merge_modules;
pub use module_resolver::{resolve_modules, FsReader, ModuleGraph, ParsedModule, SourceReader};
pub use parser::{CfgPredicate, Parser, Program};
pub use prelude::{prelude_path, prelude_program, prelude_source};
pub use virtual_fs::{parse_bundle, VirtualFs};
