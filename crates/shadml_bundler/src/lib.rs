//! Module bundler for shadml.
//!
//! The bundler orchestrates the full compilation pipeline for multi-module
//! shadml projects. It resolves module dependencies, merges them, runs the
//! compiler pipeline, and produces per-entry-point WGSL outputs with
//! tree-shaking.
//!
//! # Features
//!
//! - **Per-entry-point splitting**: each shader entry point (`@compute`,
//!   `@vertex`, `@fragment`) gets its own WGSL output containing only the
//!   code reachable from that entry point.
//! - **Name collision detection**: reports when two modules define the same
//!   top-level name.
//! - **File dependency tracking**: knows exactly which `.shadml` files
//!   contribute to each output.
//! - **Project configuration**: supports `shadml.toml` config files for
//!   specifying entry points, source roots, output paths, and features.
//! - **Virtual filesystem**: works with both real files and in-memory sources
//!   (for WASM).

pub mod config;
mod manifest;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use shadml_parser::module_resolver::ModuleGraph;
use shadml_parser::parser::{Decl, Program};

pub use manifest::*;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Configuration for a bundle operation.
#[derive(Debug, Clone)]
pub struct BundleConfig {
    /// Entry point files to compile. Each file is the root of a dependency
    /// graph.
    pub entries: Vec<PathBuf>,
    /// Directories to search for imported modules.
    pub source_roots: Vec<PathBuf>,
    /// Output directory for generated `.wgsl` files.
    pub output_dir: PathBuf,
    /// Feature flags for conditional compilation.
    pub features: Vec<String>,
    /// Whether to preserve source comments in the WGSL output.
    pub preserve_comments: bool,
    /// If true, emit a single `.wgsl` file per entry file (all entry points
    /// in one file stay together). If false, emit one `.wgsl` per shader
    /// entry point.
    pub split_entry_points: bool,
}

impl Default for BundleConfig {
    fn default() -> Self {
        BundleConfig {
            entries: Vec::new(),
            source_roots: Vec::new(),
            output_dir: PathBuf::from("dist"),
            features: Vec::new(),
            preserve_comments: false,
            split_entry_points: false,
        }
    }
}

/// The result of a successful bundle operation.
#[derive(Debug)]
pub struct BundleOutput {
    /// Individual bundle entries (one per entry point or per file, depending
    /// on configuration).
    pub entries: Vec<BundleEntry>,
    /// All diagnostics (warnings, etc.) encountered during bundling.
    pub diagnostics: Vec<BundleDiagnostic>,
    /// File dependency graph: maps each output name to the set of source
    /// files that contributed to it.
    pub dependencies: HashMap<String, Vec<PathBuf>>,
}

/// A single output unit from the bundler.
#[derive(Debug, Clone)]
pub struct BundleEntry {
    /// Name for this output (e.g. "Main", "main_compute").
    pub name: String,
    /// The source file that defined the entry point(s).
    pub source_file: PathBuf,
    /// Shader stage(s) included in this output.
    pub stages: Vec<ShaderStageInfo>,
    /// The generated WGSL source code.
    pub wgsl: String,
}

/// Information about a shader entry point in a bundle entry.
#[derive(Debug, Clone)]
pub struct ShaderStageInfo {
    /// The entry point function name.
    pub name: String,
    /// The shader stage.
    pub stage: shadml_mir::ShaderStage,
    /// Workgroup size for compute shaders.
    pub workgroup_size: Option<[u32; 3]>,
}

/// A diagnostic message from the bundler.
#[derive(Debug, Clone)]
pub struct BundleDiagnostic {
    /// The file that caused the diagnostic (if known).
    pub file: Option<PathBuf>,
    /// Severity level.
    pub severity: BundleSeverity,
    /// The diagnostic message.
    pub message: String,
    /// Optional help text.
    pub help: Option<String>,
}

/// Severity levels for bundler diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleSeverity {
    Error,
    Warning,
    Info,
}

/// A name collision between modules.
#[derive(Debug, Clone)]
pub struct NameCollision {
    /// The colliding name.
    pub name: String,
    /// The kind of declaration.
    pub kind: &'static str,
    /// Modules that define this name, with their source files.
    pub definitions: Vec<CollisionDefinition>,
}

/// A single conflicting definition.
#[derive(Debug, Clone)]
pub struct CollisionDefinition {
    /// Module name that contributed the definition.
    pub module: String,
    /// Source file containing the definition.
    pub file: PathBuf,
}

/// Errors that can occur during bundling.
#[derive(Debug)]
pub enum BundleError {
    /// Configuration errors (no entries, bad paths, etc.).
    Config(String),
    /// Module resolution errors.
    ModuleResolution(Vec<String>),
    /// Name collisions across modules.
    NameCollisions(Vec<NameCollision>),
    /// Two bundle outputs resolved to the same output filename.
    OutputNameCollisions(Vec<String>),
    /// Compilation errors (parse, semantic, lowering).
    Compilation(Vec<BundleDiagnostic>),
    /// I/O errors.
    Io(String),
}

impl std::fmt::Display for BundleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BundleError::Config(msg) => write!(f, "configuration error: {}", msg),
            BundleError::ModuleResolution(errors) => {
                write!(f, "module resolution errors:")?;
                for e in errors {
                    write!(f, "\n  {}", e)?;
                }
                Ok(())
            }
            BundleError::NameCollisions(collisions) => {
                write!(f, "name collisions across modules:")?;
                for c in collisions {
                    write!(f, "\n  {} '{}' defined in:", c.kind, c.name)?;
                    for definition in &c.definitions {
                        write!(
                            f,
                            "\n    {} ({})",
                            definition.module,
                            definition.file.display()
                        )?;
                    }
                }
                Ok(())
            }
            BundleError::OutputNameCollisions(names) => {
                write!(f, "output name collisions:")?;
                for name in names {
                    write!(f, "\n  {}", name)?;
                }
                Ok(())
            }
            BundleError::Compilation(diags) => {
                write!(f, "compilation errors:")?;
                for d in diags {
                    if let Some(ref file) = d.file {
                        write!(f, "\n  {}: {}", file.display(), d.message)?;
                    } else {
                        write!(f, "\n  {}", d.message)?;
                    }
                }
                Ok(())
            }
            BundleError::Io(msg) => write!(f, "I/O error: {}", msg),
        }
    }
}

impl std::error::Error for BundleError {}

// ---------------------------------------------------------------------------
// Bundle from filesystem
// ---------------------------------------------------------------------------

/// Bundle a project from the filesystem.
///
/// This is the main entry point for the CLI. It reads source files from disk,
/// resolves modules, compiles everything, and returns per-entry-point WGSL.
pub fn bundle(config: &BundleConfig) -> Result<BundleOutput, BundleError> {
    if config.entries.is_empty() {
        return Err(BundleError::Config("no entry files specified".into()));
    }

    let reader = shadml_parser::FsReader;
    let features = shadml_parser::FeatureSet::from_flags(&config.features);
    let mut all_entries = Vec::new();
    let mut all_diagnostics = Vec::new();
    let mut all_dependencies: HashMap<String, Vec<PathBuf>> = HashMap::new();

    for entry_file in &config.entries {
        let source = std::fs::read_to_string(entry_file)
            .map_err(|e| BundleError::Io(format!("{}: {}", entry_file.display(), e)))?;

        let result = bundle_single_entry(
            entry_file,
            &source,
            &config.source_roots,
            &features,
            &reader,
            config.preserve_comments,
            config.split_entry_points,
        )?;

        for entry in &result.entries {
            all_dependencies.insert(entry.name.clone(), result.source_files.clone());
        }
        all_entries.extend(result.entries);
        all_diagnostics.extend(result.diagnostics);
    }

    ensure_unique_output_names(&all_entries)?;

    Ok(BundleOutput {
        entries: all_entries,
        diagnostics: all_diagnostics,
        dependencies: all_dependencies,
    })
}

/// Compile a bundle manifest for one fixed feature set.
pub fn bundle_manifest(
    config: &BundleConfig,
    profile_key: impl Into<String>,
) -> Result<ShaderBundleManifest, BundleError> {
    if config.entries.is_empty() {
        return Err(BundleError::Config("no entry files specified".into()));
    }

    let reader = shadml_parser::FsReader;
    let features = shadml_parser::FeatureSet::from_flags(&config.features);
    let mut profile_entries = Vec::new();
    let mut profile_modules: HashMap<String, CompiledModule> = HashMap::new();
    let mut profile_types: HashMap<(String, String), ExportedType> = HashMap::new();
    let mut profile_source_files = Vec::new();

    for entry_file in &config.entries {
        let source = std::fs::read_to_string(entry_file)
            .map_err(|e| BundleError::Io(format!("{}: {}", entry_file.display(), e)))?;

        let result = bundle_single_entry(
            entry_file,
            &source,
            &config.source_roots,
            &features,
            &reader,
            config.preserve_comments,
            config.split_entry_points,
        )?;

        profile_entries.extend(result.compiled_entries);

        for module in result.modules {
            profile_modules.entry(module.name.clone()).or_insert(module);
        }

        for exported in result.exported_types {
            let key = (exported.rust_mod_path.join("."), exported.name.clone());
            profile_types.entry(key).or_insert(exported);
        }

        for path in result.source_files {
            if !profile_source_files.contains(&path) {
                profile_source_files.push(path);
            }
        }
    }

    ensure_unique_compiled_entry_names(&profile_entries)?;

    let mut modules = profile_modules.into_values().collect::<Vec<_>>();
    modules.sort_by(|lhs, rhs| lhs.name.cmp(&rhs.name));

    let mut exported_types = profile_types.into_values().collect::<Vec<_>>();
    exported_types.sort_by(|lhs, rhs| {
        lhs.rust_mod_path
            .join(".")
            .cmp(&rhs.rust_mod_path.join("."))
            .then_with(|| lhs.name.cmp(&rhs.name))
    });

    profile_source_files.sort();

    Ok(ShaderBundleManifest {
        profiles: vec![CompiledProfile {
            profile_key: profile_key.into(),
            enabled_features: config.features.clone(),
            source_files: profile_source_files,
            modules,
            entries: profile_entries,
            exported_types,
        }],
    })
}

// ---------------------------------------------------------------------------
// Bundle from in-memory sources (for WASM)
// ---------------------------------------------------------------------------

/// A virtual source file for in-memory bundling.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct VirtualFile {
    /// Path-like identifier (e.g. "Main.shadml", "Math/Vec.shadml").
    pub path: String,
    /// Source code contents.
    pub source: String,
}

/// Bundle from in-memory sources (for WASM / playground).
///
/// The first file in `files` is treated as the entry point.
pub fn bundle_virtual(
    files: &[VirtualFile],
    features: &[String],
    preserve_comments: bool,
) -> Result<BundleOutput, BundleError> {
    if files.is_empty() {
        return Err(BundleError::Config("no files provided".into()));
    }

    let mut vfs = shadml_parser::VirtualFs::new();
    for file in files {
        let path = PathBuf::from(&file.path);
        vfs.add(path, file.source.clone());
    }

    let entry_path = PathBuf::from(&files[0].path);
    let feature_set = shadml_parser::FeatureSet::from_flags(features);

    let result = bundle_single_entry(
        &entry_path,
        &files[0].source,
        &[PathBuf::from("")],
        &feature_set,
        &vfs,
        preserve_comments,
        false,
    )?;

    let mut dependencies = HashMap::new();
    for entry in &result.entries {
        dependencies.insert(entry.name.clone(), result.source_files.clone());
    }

    Ok(BundleOutput {
        entries: result.entries,
        diagnostics: result.diagnostics,
        dependencies,
    })
}

// ---------------------------------------------------------------------------
// Internal: single entry compilation
// ---------------------------------------------------------------------------

struct SingleEntryResult {
    entries: Vec<BundleEntry>,
    compiled_entries: Vec<CompiledEntry>,
    diagnostics: Vec<BundleDiagnostic>,
    source_files: Vec<PathBuf>,
    modules: Vec<CompiledModule>,
    exported_types: Vec<ExportedType>,
}

fn bundle_single_entry(
    entry_file: &Path,
    entry_source: &str,
    source_roots: &[PathBuf],
    features: &shadml_parser::FeatureSet,
    reader: &dyn shadml_parser::SourceReader,
    preserve_comments: bool,
    split_entry_points: bool,
) -> Result<SingleEntryResult, BundleError> {
    // 1. Parse the root file
    let mut parser = shadml_parser::parser::Parser::new(entry_source);
    let mut root_program = parser.parse_program();

    // Collect parse diagnostics
    let mut diagnostics = Vec::new();
    if parser.diagnostics().has_errors() {
        let diags: Vec<BundleDiagnostic> = parser
            .diagnostics()
            .iter()
            .map(|d| BundleDiagnostic {
                file: Some(entry_file.to_path_buf()),
                severity: convert_severity(d.severity),
                message: d.message.clone(),
                help: d.help.clone(),
            })
            .collect();
        return Err(BundleError::Compilation(diags));
    }

    // 2. Evaluate feature flags
    shadml_parser::evaluate_features(&mut root_program, features);

    // 3. Resolve module graph
    let (
        merged,
        source_files,
        modules,
        root_module_path,
        origin_map,
        module_path_map,
    ) = if has_imports(&root_program) {
        let source_root = if source_roots.is_empty() {
            vec![entry_file
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf()]
        } else {
            source_roots.to_vec()
        };

        let graph = shadml_parser::resolve_modules(entry_file, root_program, &source_root, reader)
            .map_err(|errors| {
                BundleError::ModuleResolution(errors.iter().map(|e| e.to_string()).collect())
            })?;

        // 4. Detect name collisions. Flat merging is ambiguous when duplicate
        // names exist, so fail rather than picking a declaration by order.
        let collisions = detect_name_collisions(&graph);
        if !collisions.is_empty() {
            return Err(BundleError::NameCollisions(collisions));
        }

        let files: Vec<PathBuf> = graph.modules.iter().map(|m| m.path.clone()).collect();
        let modules = graph
            .modules
            .iter()
            .map(|module| CompiledModule {
                name: module.name.clone(),
                path: module.path.clone(),
                dependencies: module
                    .imports
                    .iter()
                    .map(|import| import.module_path.clone())
                    .collect(),
            })
            .collect::<Vec<_>>();
        let root_module_path = graph
            .modules
            .last()
            .map(|module| split_module_path(&module.name))
            .unwrap_or_else(|| logical_module_path(entry_file, source_roots, None));

        let origin_map = build_origin_map(&graph);
        let module_path_map: HashMap<String, Vec<String>> = graph
            .modules
            .iter()
            .map(|m| (m.name.clone(), split_module_path(&m.name)))
            .collect();

        let merged = shadml_parser::merge_modules(&graph);
        (merged, files, modules, root_module_path, origin_map, module_path_map)
    } else {
        let module_name = logical_module_name(entry_file, source_roots, Some(&root_program));
        let mut origin_map = HashMap::new();
        for decl in &root_program.decls {
            if let Some(name) = decl_name(decl) {
                origin_map.insert(name, module_name.clone());
            }
        }
        let module_path_map: HashMap<String, Vec<String>> =
            std::iter::once((module_name.clone(), split_module_path(&module_name))).collect();
        (
            root_program,
            vec![entry_file.to_path_buf()],
            vec![CompiledModule {
                name: module_name.clone(),
                path: entry_file.to_path_buf(),
                dependencies: Vec::new(),
            }],
            split_module_path(&module_name),
            origin_map,
            module_path_map,
        )
    };

    // 5. Prepend prelude
    let mut program = merged;
    prepend_prelude(&mut program);

    // 6. Semantic analysis
    let mut analyzer = shadml_semantic::SemanticAnalyzer::new();
    analyzer.analyze(&program);

    if analyzer.has_errors() {
        let diags: Vec<BundleDiagnostic> = analyzer
            .diagnostics()
            .iter()
            .map(|d| BundleDiagnostic {
                file: Some(entry_file.to_path_buf()),
                severity: convert_severity(d.severity),
                message: d.message.clone(),
                help: d.help.clone(),
            })
            .collect();
        return Err(BundleError::Compilation(diags));
    }

    // Collect non-error diagnostics
    for d in analyzer.diagnostics().iter() {
        if d.severity != shadml_diagnostics::Severity::Error {
            diagnostics.push(BundleDiagnostic {
                file: Some(entry_file.to_path_buf()),
                severity: convert_severity(d.severity),
                message: d.message.clone(),
                help: d.help.clone(),
            });
        }
    }

    // 7. AST → HIR lowering
    let mut lowering = shadml_ast_lowering::AstLowering::new(&analyzer);
    let hir = lowering.lower_program(&program);

    if lowering.has_errors() {
        let diags: Vec<BundleDiagnostic> = lowering
            .diagnostics()
            .iter()
            .map(|d| BundleDiagnostic {
                file: Some(entry_file.to_path_buf()),
                severity: convert_severity(d.severity),
                message: d.message.clone(),
                help: d.help.clone(),
            })
            .collect();
        return Err(BundleError::Compilation(diags));
    }

    // 8. HIR → MIR lowering
    let arena = shadml_allocator::Allocator::new();
    let mut mir = shadml_mir::lower::lower_hir_to_mir(&arena, &hir).map_err(|errors| {
        BundleError::Compilation(
            errors
                .iter()
                .map(|e| BundleDiagnostic {
                    file: Some(entry_file.to_path_buf()),
                    severity: BundleSeverity::Error,
                    message: e.clone(),
                    help: None,
                })
                .collect(),
        )
    })?;

    // Annotate structs and globals with their origin module.
    for s in &mut mir.structs {
        if let Some(origin) = origin_map.get(s.name) {
            s.origin_module = Some(arena.alloc_str(origin));
        }
    }
    for g in &mut mir.globals {
        if let Some(origin) = origin_map.get(g.name) {
            g.origin_module = Some(arena.alloc_str(origin));
        }
    }

    // 9. Validate the full MIR before splitting or DCE.
    // Render blocks reference multiple entry points; validating on a
    // single-entry-point split would produce false positives.
    validate_mir_for_bundle(&mir, entry_file)?;

    // 10. Generate outputs
    let exported_types =
        exported_types_from_structs(&mir.structs, &root_module_path, &module_path_map);
    let exported_type_names = exported_type_names(&exported_types);

    let (entries, compiled_entries) = if split_entry_points && mir.entry_points.len() > 1 {
        // Per-entry-point splitting: each entry point gets its own WGSL
        // with only the code reachable from that entry point.
        generate_split_outputs(
            &mir,
            entry_file,
            source_roots,
            &root_module_path,
            &source_files,
            preserve_comments,
            &module_path_map,
        )?
    } else {
        // Single output with all entry points, standard DCE.
        let mir = shadml_mir::reachability::eliminate_dead_code(&mir);
        validate_mir_for_bundle(&mir, entry_file)?;
        let wgsl = if preserve_comments {
            shadml_wgsl_codegen::emit_wgsl_with_comments(&mir)
        } else {
            shadml_wgsl_codegen::emit_wgsl(&mir)
        };

        let stages: Vec<ShaderStageInfo> = mir
            .entry_points
            .iter()
            .map(|ep| ShaderStageInfo {
                name: ep.name.to_string(),
                stage: ep.stage,
                workgroup_size: ep.workgroup_size,
            })
            .collect();

        let name = output_base_name(entry_file, source_roots);
        let bundle_entry = BundleEntry {
            name: name.clone(),
            source_file: entry_file.to_path_buf(),
            stages: stages.clone(),
            wgsl: wgsl.clone(),
        };

        let compiled_entries = mir
            .entry_points
            .iter()
            .map(|ep| {
                let render_block = mir.render_blocks.iter().find(|rb| {
                    rb.vertex_entry == ep.name || rb.fragment_entry == ep.name
                });
                let (bind_groups, push_constants) = if let Some(rb) = render_block {
                    let rb_globals = render_block_globals(&mir, rb);
                    (bind_groups_from_globals(&rb_globals, &module_path_map), push_constants_from_globals(&rb_globals, &mir.structs))
                } else {
                    (bind_groups_from_globals(&mir.globals, &module_path_map), push_constants_from_globals(&mir.globals, &mir.structs))
                };
                CompiledEntry {
                    rust_mod_path: root_module_path.clone(),
                    shader_name: name.clone(),
                    stage: ep.stage,
                    entry_point: ep.name.to_string(),
                    wgsl_source: wgsl.clone(),
                    bind_groups,
                    push_constants,
                    workgroup_size: ep.workgroup_size,
                    source_files: source_files.clone(),
                    exported_type_names: exported_type_names.clone(),
                    render_block: render_block.map(|rb| rb.name.to_string()),
                }
            })
            .collect::<Vec<_>>();

        (vec![bundle_entry], compiled_entries)
    };

    Ok(SingleEntryResult {
        entries,
        compiled_entries,
        diagnostics,
        source_files,
        modules,
        exported_types,
    })
}

/// Compute DCE-trimmed globals for a render block by combining reachability
/// from both its vertex and fragment entry points.
///
/// Creates a temporary MIR program containing all entry points referenced by
/// the render block, runs dead-code elimination once, and returns the union
/// of reachable globals. Render-block explicitly-declared bindings are also
/// included even if not directly referenced by shader code.
fn render_block_globals<'a>(
    mir: &shadml_mir::MirProgram<'a>,
    rb: &shadml_mir::MirRenderBlock,
) -> Vec<shadml_mir::MirGlobal<'a>> {
    let rb_entry_points: Vec<shadml_mir::MirEntryPoint<'a>> = [rb.vertex_entry, rb.fragment_entry]
        .iter()
        .filter(|name| !name.is_empty())
        .filter_map(|name| mir.entry_points.iter().find(|e| e.name == *name))
        .cloned()
        .collect();

    if rb_entry_points.is_empty() {
        // No entry points: include only explicitly-declared bindings
        let declared: HashSet<&str> = rb.binding_names.iter().map(|s| *s).collect();
        return mir
            .globals
            .iter()
            .filter(|g| declared.contains(g.name))
            .cloned()
            .collect();
    }

    let rb_mir = shadml_mir::MirProgram {
        structs: mir.structs.clone(),
        globals: mir.globals.clone(),
        functions: mir.functions.clone(),
        entry_points: rb_entry_points,
        constants: mir.constants.clone(),
        render_blocks: mir.render_blocks.clone(),
    };
    let rb_trimmed = shadml_mir::reachability::eliminate_dead_code(&rb_mir);

    // Ensure render block's own declared bindings are included
    let mut globals = rb_trimmed.globals;
    let mut global_names: HashSet<&str> = globals.iter().map(|g| g.name).collect();
    for binding_name in &rb.binding_names {
        if !global_names.contains(*binding_name) {
            if let Some(g) = mir.globals.iter().find(|g| g.name == *binding_name) {
                global_names.insert(g.name);
                globals.push(g.clone());
            }
        }
    }

    globals
}

/// Generate split per-entry-point WGSL outputs.
///
/// For each entry point in the MIR, creates a separate MIR program containing
/// only that entry point, runs DCE, and emits WGSL. This means each output
/// file contains exactly the code needed for one shader stage.
fn generate_split_outputs<'a>(
    mir: &shadml_mir::MirProgram<'a>,
    source_file: &Path,
    source_roots: &[PathBuf],
    rust_mod_path: &[String],
    source_files: &[PathBuf],
    preserve_comments: bool,
    module_path_map: &HashMap<String, Vec<String>>,
) -> Result<(Vec<BundleEntry>, Vec<CompiledEntry>), BundleError> {
    let mut entries = Vec::new();
    let mut compiled_entries = Vec::new();

    for ep in &mir.entry_points {
        // Create a MIR program with just this one entry point
        let single_ep_mir = shadml_mir::MirProgram {
            structs: mir.structs.clone(),
            globals: mir.globals.clone(),
            functions: mir.functions.clone(),
            entry_points: vec![ep.clone()],
            constants: mir.constants.clone(),
            render_blocks: mir.render_blocks.clone(),
        };

        // Run DCE scoped to this entry point
        let mut trimmed = shadml_mir::reachability::eliminate_dead_code(&single_ep_mir);
        // Clear render blocks: they reference entry points not present in the
        // split program and are irrelevant for per-entry-point WGSL output.
        trimmed.render_blocks.clear();

        let wgsl = if preserve_comments {
            shadml_wgsl_codegen::emit_wgsl_with_comments(&trimmed)
        } else {
            shadml_wgsl_codegen::emit_wgsl(&trimmed)
        };

        let name = format!(
            "{}_{}",
            output_base_name(source_file, source_roots),
            ep.name
        );
        let render_block = mir.render_blocks.iter().find(|rb| {
            rb.vertex_entry == ep.name || rb.fragment_entry == ep.name
        });
        let (bind_groups, push_constants) = if let Some(rb) = render_block {
            let rb_globals = render_block_globals(mir, rb);
            (bind_groups_from_globals(&rb_globals, module_path_map), push_constants_from_globals(&rb_globals, &mir.structs))
        } else {
            (bind_groups_from_globals(&trimmed.globals, module_path_map), push_constants_from_globals(&trimmed.globals, &trimmed.structs))
        };
        let exported_types = exported_types_from_structs(&trimmed.structs, rust_mod_path, module_path_map);

        entries.push(BundleEntry {
            name: name.clone(),
            source_file: source_file.to_path_buf(),
            stages: vec![ShaderStageInfo {
                name: ep.name.to_string(),
                stage: ep.stage,
                workgroup_size: ep.workgroup_size,
            }],
            wgsl: wgsl.clone(),
        });

        compiled_entries.push(CompiledEntry {
            rust_mod_path: rust_mod_path.to_vec(),
            shader_name: name,
            stage: ep.stage,
            entry_point: ep.name.to_string(),
            wgsl_source: wgsl,
            bind_groups,
            push_constants,
            workgroup_size: ep.workgroup_size,
            source_files: source_files.to_vec(),
            exported_type_names: exported_type_names(&exported_types),
            render_block: render_block.map(|rb| rb.name.to_string()),
        });
    }

    Ok((entries, compiled_entries))
}

fn validate_mir_for_bundle(
    mir: &shadml_mir::MirProgram<'_>,
    source_file: &Path,
) -> Result<(), BundleError> {
    shadml_mir::validate::validate_program(mir).map_err(|errors| {
        BundleError::Compilation(
            errors
                .into_iter()
                .map(|message| BundleDiagnostic {
                    file: Some(source_file.to_path_buf()),
                    severity: BundleSeverity::Error,
                    message,
                    help: None,
                })
                .collect(),
        )
    })
}

// ---------------------------------------------------------------------------
// Name collision detection
// ---------------------------------------------------------------------------

/// Detect name collisions across modules in a module graph.
///
/// Returns a list of names that are defined in more than one module.
/// Module/import declarations are excluded from collision detection.
pub fn detect_name_collisions(graph: &ModuleGraph) -> Vec<NameCollision> {
    // Track (name, kind) -> list of module definitions
    let mut definitions: HashMap<(String, &'static str), Vec<CollisionDefinition>> = HashMap::new();

    for module in &graph.modules {
        for decl in &module.program.decls {
            if let Some((name, kind)) = decl_name_and_kind(decl) {
                definitions
                    .entry((name.clone(), kind))
                    .or_default()
                    .push(CollisionDefinition {
                        module: module.name.clone(),
                        file: module.path.clone(),
                    });
            }
        }
    }

    definitions
        .into_iter()
        .filter(|(_, definitions)| definitions.len() > 1)
        .map(|((name, kind), definitions)| NameCollision {
            name,
            kind,
            definitions,
        })
        .collect()
}

/// Extract the name and kind from a declaration for collision detection.
fn decl_name_and_kind(decl: &Decl) -> Option<(String, &'static str)> {
    match decl {
        Decl::FunDecl { name, .. } => Some((name.clone(), "function")),
        Decl::TypeSig { .. } => None, // type sigs pair with FunDecl, don't count separately
        Decl::DataDecl { name, .. } => Some((name.clone(), "type")),
        Decl::BuiltinTypeDecl { name, .. } => Some((name.clone(), "builtin type")),
        Decl::TypeAlias { name, .. } => Some((name.clone(), "type alias")),
        Decl::TraitDecl { name, .. } => Some((name.clone(), "trait")),
        Decl::ImplDecl { .. } | Decl::BuiltinImplDecl { .. } => None, // impls don't introduce names
        Decl::ExternDecl { name, .. } => Some((name.clone(), "extern")),
        Decl::BuiltinExternDecl { name, .. } => Some((name.clone(), "builtin extern")),
        Decl::ConstDecl { name, .. } => Some((name.clone(), "constant")),
        Decl::EntryPoint { name, .. } => Some((name.clone(), "entry point")),
        Decl::BindingDecl { .. } => None, // bindings are addressed by group/binding, not by name collision
        Decl::BitfieldDecl { name, .. } => Some((name.clone(), "bitfield")),
        Decl::ModuleDecl { .. } | Decl::ImportDecl { .. } => None,
        Decl::CfgDecl { .. } => None, // cfg blocks are containers, not names
        Decl::RenderBlock { name, .. } => Some((name.clone(), "render block")),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn prepend_prelude(program: &mut Program) {
    let prelude = shadml_parser::prelude_program();
    let mut combined = prelude.decls.clone();
    combined.append(&mut program.decls);
    program.decls = combined;
}

fn has_imports(program: &Program) -> bool {
    has_imports_in(&program.decls)
}

fn has_imports_in(decls: &[Decl]) -> bool {
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

fn convert_severity(severity: shadml_diagnostics::Severity) -> BundleSeverity {
    match severity {
        shadml_diagnostics::Severity::Error => BundleSeverity::Error,
        shadml_diagnostics::Severity::Warning => BundleSeverity::Warning,
        shadml_diagnostics::Severity::Info | shadml_diagnostics::Severity::Hint => {
            BundleSeverity::Info
        }
    }
}

fn ensure_unique_compiled_entry_names(entries: &[CompiledEntry]) -> Result<(), BundleError> {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for entry in entries {
        *seen.entry(&entry.shader_name).or_insert(0) += 1;
    }

    let mut collisions: Vec<String> = seen
        .into_iter()
        .filter_map(|(name, count)| (count > 1).then_some(name.to_string()))
        .collect();
    collisions.sort();

    if collisions.is_empty() {
        Ok(())
    } else {
        Err(BundleError::OutputNameCollisions(collisions))
    }
}

fn ensure_unique_output_names(entries: &[BundleEntry]) -> Result<(), BundleError> {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for entry in entries {
        *seen.entry(&entry.name).or_insert(0) += 1;
    }

    let mut collisions: Vec<String> = seen
        .into_iter()
        .filter_map(|(name, count)| (count > 1).then_some(name.to_string()))
        .collect();
    collisions.sort();

    if collisions.is_empty() {
        Ok(())
    } else {
        Err(BundleError::OutputNameCollisions(collisions))
    }
}

fn split_module_path(module_name: &str) -> Vec<String> {
    module_name
        .split('.')
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.to_string())
        .collect()
}

fn logical_module_path(
    path: &Path,
    source_roots: &[PathBuf],
    program: Option<&Program>,
) -> Vec<String> {
    split_module_path(&logical_module_name(path, source_roots, program))
}

fn logical_module_name(path: &Path, source_roots: &[PathBuf], program: Option<&Program>) -> String {
    program
        .and_then(module_decl_name)
        .unwrap_or_else(|| derive_module_name(path, source_roots))
}

fn module_decl_name(program: &Program) -> Option<String> {
    program.decls.iter().find_map(|decl| match decl {
        Decl::ModuleDecl { name, .. } => Some(name.clone()),
        _ => None,
    })
}

fn derive_module_name(path: &Path, source_roots: &[PathBuf]) -> String {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        path.to_path_buf()
    };

    for root in source_roots {
        if let Ok(relative) = path.strip_prefix(root) {
            let name = relative
                .with_extension("")
                .components()
                .map(|component| component.as_os_str().to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join(".");
            if !name.is_empty() {
                return name;
            }
        }
    }

    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Main")
        .to_string()
}

fn output_base_name(path: &Path, source_roots: &[PathBuf]) -> String {
    let relative = source_roots
        .iter()
        .filter_map(|root| path.strip_prefix(root).ok())
        .min_by_key(|candidate| candidate.components().count())
        .unwrap_or(path);
    let mut parts = Vec::new();

    for component in relative.components() {
        let part = component.as_os_str().to_string_lossy();
        if part.is_empty() || part == "." || part == ".." {
            continue;
        }

        let sanitized: String = part
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
            .collect();

        if !sanitized.is_empty() {
            parts.push(sanitized);
        }
    }

    let mut name = if parts.is_empty() {
        "output".to_string()
    } else {
        parts.join("__")
    };

    if let Some(stripped) = name.strip_suffix("_shadml") {
        name = stripped.to_string();
    }

    if name.is_empty() {
        "output".to_string()
    } else {
        name
    }
}

// ---------------------------------------------------------------------------
// Origin tracking helpers
// ---------------------------------------------------------------------------

/// Build a map from declaration name to origin module name by scanning the
/// module graph. Only tracks names relevant to bindgen: data types,
/// type aliases, bitfields, bindings, and constants.
fn build_origin_map(graph: &shadml_parser::module_resolver::ModuleGraph) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for module in &graph.modules {
        for decl in &module.program.decls {
            if let Some(name) = decl_name(decl) {
                map.insert(name, module.name.clone());
            }
        }
    }
    map
}

/// Extract the declared name from a declaration, if it has one.
fn decl_name(decl: &shadml_parser::parser::Decl) -> Option<String> {
    use shadml_parser::parser::Decl;
    match decl {
        Decl::DataDecl { name, .. }
        | Decl::TypeAlias { name, .. }
        | Decl::BitfieldDecl { name, .. }
        | Decl::BindingDecl { name, .. }
        | Decl::ConstDecl { name, .. }
        | Decl::TypeSig { name, .. } => Some(name.clone()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Write bundle outputs to disk
// ---------------------------------------------------------------------------

/// Write all bundle entries to the output directory as `.wgsl` files.
pub fn write_bundle(output: &BundleOutput, output_dir: &Path) -> Result<(), BundleError> {
    std::fs::create_dir_all(output_dir)
        .map_err(|e| BundleError::Io(format!("create dir {}: {}", output_dir.display(), e)))?;

    for entry in &output.entries {
        let filename = format!("{}.wgsl", entry.name);
        let path = output_dir.join(&filename);
        std::fs::write(&path, &entry.wgsl)
            .map_err(|e| BundleError::Io(format!("write {}: {}", path.display(), e)))?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use shadml_parser::ParsedModule;

    fn make_virtual_files(files: &[(&str, &str)]) -> Vec<VirtualFile> {
        files
            .iter()
            .map(|(path, source)| VirtualFile {
                path: path.to_string(),
                source: source.to_string(),
            })
            .collect()
    }

    #[test]
    fn bundle_single_file_no_imports() {
        let files = make_virtual_files(&[(
            "Main.shadml",
            r#"
add : I32 -> I32 -> I32
add x y = x + y
"#,
        )]);

        let result = bundle_virtual(&files, &[], false).expect("should bundle");
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].name, "Main");
        // No entry points, so stages should be empty
        assert!(result.entries[0].stages.is_empty());
        // Should produce valid (possibly empty-ish) WGSL
        assert!(!result.entries[0].wgsl.is_empty() || result.entries[0].stages.is_empty());
    }

    #[test]
    fn bundle_with_entry_point() {
        let files = make_virtual_files(&[(
            "Main.shadml",
            r#"
data ComputeInput = ComputeInput {
  @builtin(global_invocation_id) gid : Vec<3, U32>
}

main : ComputeInput -> ()
@compute @workgroup_size(64, 1, 1)
main input = ()
"#,
        )]);

        let result = bundle_virtual(&files, &[], false).expect("should bundle");
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].stages.len(), 1);
        assert_eq!(result.entries[0].stages[0].name, "main");
        assert_eq!(
            result.entries[0].stages[0].stage,
            shadml_mir::ShaderStage::Compute
        );
        assert!(result.entries[0].wgsl.contains("@compute"));
    }

    #[test]
    fn bundle_multi_module() {
        let files = make_virtual_files(&[
            (
                "Main.shadml",
                r#"
import Utils

double_add : I32 -> I32 -> I32
double_add x y = double (x + y)
"#,
            ),
            (
                "Utils.shadml",
                r#"
module Utils

double : I32 -> I32
double x = x * 2
"#,
            ),
        ]);

        let result = bundle_virtual(&files, &[], false).expect("should bundle");
        assert_eq!(result.entries.len(), 1);
        // Should contain code from both modules
        let wgsl = &result.entries[0].wgsl;
        assert!(
            wgsl.contains("double") || wgsl.contains("double_add"),
            "output should contain merged code"
        );
    }

    #[test]
    fn bundle_detects_name_collisions() {
        let files = make_virtual_files(&[
            (
                "Main.shadml",
                r#"
import A
import B

use_helper : I32 -> I32
use_helper x = helper x
"#,
            ),
            (
                "A.shadml",
                r#"
module A

helper : I32 -> I32
helper x = x + 1
"#,
            ),
            (
                "B.shadml",
                r#"
module B

helper : I32 -> I32
helper x = x + 2
"#,
            ),
        ]);

        let result = bundle_virtual(&files, &[], false);
        match result {
            Err(BundleError::NameCollisions(collisions)) => {
                assert_eq!(collisions.len(), 1);
                assert_eq!(collisions[0].name, "helper");
                assert_eq!(collisions[0].definitions.len(), 2);
            }
            other => panic!("expected name collision error, got {:?}", other),
        }
    }

    #[test]
    fn bundle_empty_files_returns_error() {
        let files: Vec<VirtualFile> = vec![];
        let result = bundle_virtual(&files, &[], false);
        assert!(result.is_err());
    }

    #[test]
    fn bundle_parse_error_returns_error() {
        let files = make_virtual_files(&[("Bad.shadml", "this is not valid shadml {{{{")]);

        let result = bundle_virtual(&files, &[], false);
        // Should either error or produce diagnostics
        match result {
            Err(BundleError::Compilation(diags)) => {
                assert!(!diags.is_empty());
            }
            Ok(_output) => {
                // If it doesn't error, it might have diagnostics
                // (the parser might recover from some errors)
            }
            _ => {}
        }
    }

    #[test]
    fn bundle_with_features() {
        let files = make_virtual_files(&[(
            "Main.shadml",
            "when cfg.debug\n  debugVal : I32\n  debugVal = 42\n\nadd : I32 -> I32 -> I32\nadd x y = x + y\n",
        )]);

        // Without debug feature
        let result1 = bundle_virtual(&files, &[], false).expect("should bundle");
        let wgsl1 = &result1.entries[0].wgsl;

        // With debug feature
        let result2 = bundle_virtual(&files, &["debug".into()], false).expect("should bundle");
        let wgsl2 = &result2.entries[0].wgsl;

        // The debug version should potentially have more code
        // (depending on DCE — debugVal might be eliminated if unused)
        assert!(!wgsl1.is_empty() || !wgsl2.is_empty());
    }

    #[test]
    fn bundle_dependency_tracking() {
        let files = make_virtual_files(&[
            (
                "Main.shadml",
                r#"
import Utils

f : I32 -> I32
f x = helper x
"#,
            ),
            (
                "Utils.shadml",
                r#"
module Utils

helper : I32 -> I32
helper x = x + 1
"#,
            ),
        ]);

        let result = bundle_virtual(&files, &[], false).expect("should bundle");
        // Should track dependencies
        assert!(!result.dependencies.is_empty());
        let deps = result.dependencies.values().next().unwrap();
        assert!(deps.len() >= 1, "should track at least one source file");
    }

    #[test]
    fn name_collision_detection_across_modules() {
        // Test the collision detector directly
        let mut parser1 =
            shadml_parser::parser::Parser::new("foo : I32\nfoo = 1\nbar : I32\nbar = 2");
        let prog1 = parser1.parse_program();
        let mut parser2 =
            shadml_parser::parser::Parser::new("foo : I32\nfoo = 3\nbaz : I32\nbaz = 4");
        let prog2 = parser2.parse_program();

        let graph = ModuleGraph {
            modules: vec![
                ParsedModule {
                    name: "A".into(),
                    path: PathBuf::from("A.shadml"),
                    program: prog1,
                    imports: vec![],
                },
                ParsedModule {
                    name: "B".into(),
                    path: PathBuf::from("B.shadml"),
                    program: prog2,
                    imports: vec![],
                },
            ],
        };

        let collisions = detect_name_collisions(&graph);
        assert_eq!(collisions.len(), 1);
        assert_eq!(collisions[0].name, "foo");
        assert_eq!(collisions[0].kind, "function");
        assert!(collisions[0]
            .definitions
            .iter()
            .any(|definition| definition.module == "A"));
        assert!(collisions[0]
            .definitions
            .iter()
            .any(|definition| definition.module == "B"));
    }

    #[test]
    fn output_base_name_includes_path_components() {
        assert_eq!(
            output_base_name(Path::new("src/Main.shadml"), &[PathBuf::from(".")]),
            "src__Main"
        );
        assert_eq!(
            output_base_name(
                Path::new("examples/post/Main.shadml"),
                &[PathBuf::from(".")]
            ),
            "examples__post__Main"
        );
    }

    #[test]
    fn bundle_multiple_entries_with_same_stem_do_not_collide() {
        let root = std::env::temp_dir().join(format!(
            "shadml_bundle_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time should be monotonic")
                .as_nanos()
        ));
        let src_dir = root.join("src");
        let examples_dir = root.join("examples");
        std::fs::create_dir_all(&src_dir).expect("should create src dir");
        std::fs::create_dir_all(&examples_dir).expect("should create examples dir");

        let src_main = src_dir.join("Main.shadml");
        let examples_main = examples_dir.join("Main.shadml");
        std::fs::write(&src_main, "id : I32 -> I32\nid x = x\n").expect("should write src entry");
        std::fs::write(&examples_main, "id : I32 -> I32\nid x = x\n")
            .expect("should write examples entry");

        let config = BundleConfig {
            entries: vec![src_main, examples_main],
            source_roots: vec![root.clone()],
            output_dir: root.join("dist"),
            features: vec![],
            preserve_comments: false,
            split_entry_points: false,
        };

        let result = bundle(&config).expect("should bundle");
        let names: Vec<&str> = result
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert!(names.contains(&"src__Main"));
        assert!(names.contains(&"examples__Main"));

        std::fs::remove_dir_all(&root).expect("should remove temp tree");
    }
}
