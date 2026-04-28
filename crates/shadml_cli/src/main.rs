use std::env;
use std::fs;
use std::process;

fn main() {
    let args: Vec<String> = env::args().collect();

    // Collect --feature flags from anywhere in the args
    let features = collect_features(&args);

    match args.get(1).map(|s| s.as_str()) {
        Some("compile") | Some("c") => {
            let file = args.get(2).unwrap_or_else(|| {
                eprintln!("Usage: shadml compile <file.shadml>");
                process::exit(1);
            });
            cmd_compile(
                file,
                args.contains(&"--emit-ast".to_string()),
                args.contains(&"--preserve-comments".to_string()),
                args.contains(&"--validate-wgsl".to_string()),
                &features,
            );
        }
        Some("bundle") | Some("b") => {
            cmd_bundle(&args, &features);
        }
        Some("check") => {
            let file = args.get(2).unwrap_or_else(|| {
                eprintln!("Usage: shadml check <file.shadml>");
                process::exit(1);
            });
            cmd_check(file, &features);
        }
        Some("fmt") => {
            let file = args.get(2).unwrap_or_else(|| {
                eprintln!("Usage: shadml fmt <file.shadml>");
                process::exit(1);
            });
            cmd_fmt(file);
        }
        Some("version") | Some("--version") | Some("-V") => {
            println!("shadml {}", env!("CARGO_PKG_VERSION"));
        }
        Some("help") | Some("--help") | Some("-h") | None => {
            print_help();
        }
        Some(cmd) => {
            eprintln!("Unknown command: {}", cmd);
            eprintln!();
            print_help();
            process::exit(1);
        }
    }
}

fn print_help() {
    println!(
        r#"shadml - Pure functional language for WebGPU

USAGE:
    shadml <COMMAND> [OPTIONS]

COMMANDS:
    compile, c  <file>    Compile .shadml to .wgsl
    bundle,  b  [opts]    Bundle a project (multi-file) to .wgsl
    check       <file>    Type-check without emitting
    fmt         <file>    Format source code
    version               Print version
    help                  Print this help

COMPILE OPTIONS:
    --emit-ast            Print AST debug output
    --preserve-comments   Preserve source comments in WGSL output
    --validate-wgsl       Validate emitted WGSL with naga before printing
    --feature <name>      Enable a compile-time feature flag (can be repeated)

BUNDLE OPTIONS:
    --config <file>       Use a shadml.toml project config file
    --entry <file>        Entry point .shadml file (can be repeated)
    --source-root <dir>   Module search directory (can be repeated)
    --output-dir <dir>    Output directory (default: dist)
    --split               Split each entry point into a separate .wgsl file
    --preserve-comments   Preserve source comments in WGSL output
    --feature <name>      Enable a compile-time feature flag (can be repeated)
"#
    );
}

/// Collect `--feature <name>` flags from the argument list.
fn collect_features(args: &[String]) -> Vec<String> {
    let mut features = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--feature" {
            if let Some(name) = iter.next() {
                features.push(name.clone());
            }
        }
    }
    features
}

fn cmd_compile(
    file: &str,
    emit_ast: bool,
    preserve_comments: bool,
    validate_wgsl: bool,
    feature_flags: &[String],
) {
    let source = read_file(file);

    // Parse the root file
    let mut parser = shadml_parser::parser::Parser::new(&source);
    let mut root_program = parser.parse_program();

    if emit_ast {
        println!("{:#?}", root_program);
        return;
    }

    // Check for parse errors
    if parser.diagnostics().has_errors() {
        print_diagnostics(
            &parser.diagnostics().iter().collect::<Vec<_>>(),
            file,
            &source,
        );
        process::exit(1);
    }

    // Evaluate feature flags (prune conditional declarations/imports)
    let features = shadml_parser::FeatureSet::from_flags(feature_flags);
    shadml_parser::evaluate_features(&mut root_program, &features);

    // If the program has imports, use the module resolver
    let mut program = if shadml_parser::has_imports(&root_program) {
        let root_path = std::path::Path::new(file);
        let source_root = root_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();

        let reader = shadml_parser::FsReader;
        match shadml_parser::resolve_modules(root_path, root_program, &[source_root], &reader) {
            Ok(graph) => shadml_parser::merge_modules(&graph),
            Err(errors) => {
                for e in &errors {
                    eprintln!("error: {}", e);
                }
                process::exit(1);
            }
        }
    } else {
        root_program
    };

    // Prepend prelude declarations unless we're compiling the prelude itself.
    if shadml_parser::should_prepend_prelude(file) {
        shadml_parser::with_prelude(&mut program, false);
    }

    // Semantic analysis
    let mut analyzer = shadml_semantic::SemanticAnalyzer::new();
    analyzer.analyze(&program);

    if analyzer.has_errors() {
        let diags: Vec<_> = analyzer.diagnostics().iter().collect();
        print_diagnostics(&diags, file, &source);
        process::exit(1);
    }

    // AST -> HIR lowering
    let mut lowering = shadml_ast_lowering::AstLowering::new(&analyzer);
    let hir = lowering.lower_program(&program);

    if lowering.has_errors() {
        let diags: Vec<_> = lowering.diagnostics().iter().collect();
        print_diagnostics(&diags, file, &source);
        process::exit(1);
    }

    // HIR -> MIR lowering
    let arena = shadml_allocator::Allocator::new();
    let mir = match shadml_mir::lower::lower_hir_to_mir(&arena, &hir) {
        Ok(mir) => mir,
        Err(errors) => {
            for e in &errors {
                eprintln!("error: {}", e);
            }
            process::exit(1);
        }
    };

    // Dead code elimination
    let mir = shadml_mir::reachability::eliminate_dead_code(&mir);

    if let Err(errors) = shadml_mir::validate::validate_program(&mir) {
        for e in &errors {
            eprintln!("error: {}", e);
        }
        process::exit(1);
    }

    // MIR -> WGSL codegen
    let wgsl = if preserve_comments {
        shadml_wgsl_codegen::emit_wgsl_with_comments(&mir)
    } else {
        shadml_wgsl_codegen::emit_wgsl(&mir)
    };

    if validate_wgsl {
        validate_wgsl_output(&wgsl);
    }

    println!(
        "// Generated by shadml compiler v{}",
        env!("CARGO_PKG_VERSION")
    );
    println!("// Source: {}", file);
    println!();
    print!("{}", wgsl);
}

fn validate_wgsl_output(wgsl: &str) {
    let module = match naga::front::wgsl::parse_str(wgsl) {
        Ok(module) => module,
        Err(error) => {
            eprintln!("error: naga parse: {}", error);
            process::exit(1);
        }
    };

    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );

    if let Err(error) = validator.validate(&module) {
        eprintln!("error: naga validate: {}", error);
        process::exit(1);
    }
}

fn cmd_check(file: &str, feature_flags: &[String]) {
    let source = read_file(file);

    let mut parser = shadml_parser::parser::Parser::new(&source);
    let mut root_program = parser.parse_program();

    let mut has_errors = false;

    if parser.diagnostics().has_errors() {
        print_diagnostics(
            &parser.diagnostics().iter().collect::<Vec<_>>(),
            file,
            &source,
        );
        has_errors = true;
    }

    // Evaluate feature flags (prune conditional declarations/imports)
    let features = shadml_parser::FeatureSet::from_flags(feature_flags);
    shadml_parser::evaluate_features(&mut root_program, &features);

    // If the program has imports, use the module resolver
    let mut program = if shadml_parser::has_imports(&root_program) {
        let root_path = std::path::Path::new(file);
        let source_root = root_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();

        let reader = shadml_parser::FsReader;
        match shadml_parser::resolve_modules(root_path, root_program, &[source_root], &reader) {
            Ok(graph) => shadml_parser::merge_modules(&graph),
            Err(errors) => {
                for e in &errors {
                    eprintln!("error: {}", e);
                }
                process::exit(1);
            }
        }
    } else {
        root_program
    };

    if shadml_parser::should_prepend_prelude(file) {
        shadml_parser::with_prelude(&mut program, false);
    }

    let mut analyzer = shadml_semantic::SemanticAnalyzer::new();
    analyzer.analyze(&program);

    if analyzer.has_errors() {
        let diags: Vec<_> = analyzer.diagnostics().iter().collect();
        print_diagnostics(&diags, file, &source);
        has_errors = true;
    }

    if has_errors {
        process::exit(1);
    } else {
        println!("No errors found in {}", file);
    }
}

fn cmd_fmt(file: &str) {
    let source = read_file(file);
    let config = resolve_formatter_config(file);
    let formatted = shadml_formatter::format(&source, &config);
    print!("{}", formatted);
}

/// Search for `shadml.toml` starting from `file`'s directory and walking up.
/// If found, parse the `[formatter]` section; otherwise return defaults.
fn resolve_formatter_config(file: &str) -> shadml_formatter::FormatConfig {
    let file_path = std::path::Path::new(file);
    let start_dir = file_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));

    // Search from the file's directory tree first.
    if let Some(cfg) = search_config_from(start_dir) {
        return cfg;
    }

    // Fall back to searching from the current working directory.
    if start_dir != std::path::Path::new(".") {
        if let Some(cfg) = search_config_from(std::path::Path::new(".")) {
            return cfg;
        }
    }

    shadml_formatter::FormatConfig::default()
}

fn search_config_from(start_dir: &std::path::Path) -> Option<shadml_formatter::FormatConfig> {
    let mut dir = Some(start_dir);
    while let Some(d) = dir {
        let candidate = d.join("shadml.toml");
        if candidate.is_file() {
            if let Ok(text) = std::fs::read_to_string(&candidate) {
                match shadml_formatter::load_formatter_config(&text) {
                    Ok(Some(cfg)) => return Some(cfg),
                    Ok(None) => return Some(shadml_formatter::FormatConfig::default()),
                    Err(e) => {
                        eprintln!(
                            "warning: {}: invalid [formatter] section: {}",
                            candidate.display(),
                            e
                        );
                        return Some(shadml_formatter::FormatConfig::default());
                    }
                }
            }
        }
        dir = d.parent();
    }
    None
}

fn cmd_bundle(args: &[String], feature_flags: &[String]) {
    // Check for --config flag first
    if let Some(config_path) = collect_flag_value(args, "--config") {
        let path = std::path::Path::new(&config_path);
        let mut config = match shadml_bundler::config::load_config(path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: {}", e);
                process::exit(1);
            }
        };
        // CLI feature flags override config
        if !feature_flags.is_empty() {
            config.features = feature_flags.to_vec();
        }
        // CLI flags can override config settings
        if args.contains(&"--split".to_string()) {
            config.split_entry_points = true;
        }
        if args.contains(&"--preserve-comments".to_string()) {
            config.preserve_comments = true;
        }
        if let Some(dir) = collect_flag_value(args, "--output-dir") {
            config.output_dir = std::path::PathBuf::from(dir);
        }
        run_bundle(config);
        return;
    }

    // Build config from CLI args
    let entries: Vec<std::path::PathBuf> = collect_flag_values(args, "--entry")
        .into_iter()
        .map(std::path::PathBuf::from)
        .collect();

    // If no --entry flags, try the positional argument
    let entries = if entries.is_empty() {
        match args.get(2) {
            Some(file) if !file.starts_with('-') => vec![std::path::PathBuf::from(file)],
            _ => {
                eprintln!("Usage: shadml bundle --entry <file.shadml> [--entry <file2.shadml>]");
                eprintln!("       shadml bundle --config <shadml.toml>");
                eprintln!("       shadml bundle <file.shadml>");
                process::exit(1);
            }
        }
    } else {
        entries
    };

    let source_roots: Vec<std::path::PathBuf> = collect_flag_values(args, "--source-root")
        .into_iter()
        .map(std::path::PathBuf::from)
        .collect();

    // Default source root: directory of the first entry file
    let source_roots = if source_roots.is_empty() {
        entries
            .first()
            .and_then(|e| e.parent())
            .map(|p| vec![p.to_path_buf()])
            .unwrap_or_else(|| vec![std::path::PathBuf::from(".")])
    } else {
        source_roots
    };

    let output_dir = collect_flag_value(args, "--output-dir")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("dist"));

    let config = shadml_bundler::BundleConfig {
        entries,
        source_roots,
        output_dir,
        features: feature_flags.to_vec(),
        preserve_comments: args.contains(&"--preserve-comments".to_string()),
        split_entry_points: args.contains(&"--split".to_string()),
    };

    run_bundle(config);
}

fn run_bundle(config: shadml_bundler::BundleConfig) {
    let output_dir = config.output_dir.clone();

    let output = match shadml_bundler::bundle(&config) {
        Ok(output) => output,
        Err(e) => {
            eprintln!("error: {}", e);
            process::exit(1);
        }
    };

    // Print warnings
    for diag in &output.diagnostics {
        let severity = match diag.severity {
            shadml_bundler::BundleSeverity::Error => "error",
            shadml_bundler::BundleSeverity::Warning => "warning",
            shadml_bundler::BundleSeverity::Info => "info",
        };
        if let Some(ref file) = diag.file {
            eprintln!("{}: {}: {}", file.display(), severity, diag.message);
        } else {
            eprintln!("{}: {}", severity, diag.message);
        }
    }

    // Write outputs
    if let Err(e) = shadml_bundler::write_bundle(&output, &output_dir) {
        eprintln!("error: {}", e);
        process::exit(1);
    }

    // Report results
    for entry in &output.entries {
        let filename = format!("{}.wgsl", entry.name);
        let path = output_dir.join(&filename);
        let stages: Vec<String> = entry
            .stages
            .iter()
            .map(|s| format!("@{} {}", s.stage, s.name))
            .collect();
        if stages.is_empty() {
            println!("  {} (library)", path.display());
        } else {
            println!("  {} [{}]", path.display(), stages.join(", "));
        }
    }

    println!(
        "Bundled {} output(s) to {}",
        output.entries.len(),
        output_dir.display()
    );
}

/// Collect a single value for a named flag (e.g. `--config <value>`).
fn collect_flag_value(args: &[String], flag: &str) -> Option<String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == flag {
            return iter.next().cloned();
        }
    }
    None
}

/// Collect all values for a repeated flag (e.g. `--entry a --entry b`).
fn collect_flag_values(args: &[String], flag: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == flag {
            if let Some(val) = iter.next() {
                values.push(val.clone());
            }
        }
    }
    values
}

fn read_file(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("Error reading {}: {}", path, e);
        process::exit(1);
    })
}

fn print_diagnostics(diagnostics: &[&shadml_diagnostics::Diagnostic], file: &str, source: &str) {
    for diag in diagnostics {
        let severity = match diag.severity {
            shadml_diagnostics::Severity::Error => "error",
            shadml_diagnostics::Severity::Warning => "warning",
            shadml_diagnostics::Severity::Info => "info",
            shadml_diagnostics::Severity::Hint => "hint",
        };

        // Find line/col from first label span
        let location = if let Some(label) = diag.labels.first() {
            let (line, col) = offset_to_line_col(source, label.span.start as usize);
            format!("{}:{}:{}", file, line, col)
        } else {
            file.to_string()
        };

        eprintln!("{}: {}: {}", location, severity, diag.message);

        if let Some(ref help) = diag.help {
            eprintln!("  help: {}", help);
        }
    }
}

fn offset_to_line_col(source: &str, offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 1;
    for (i, ch) in source.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}
