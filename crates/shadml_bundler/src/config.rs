//! Configuration file support for the shadml bundler.
//!
//! Reads `shadml.toml` project configuration files.
//!
//! # Example
//!
//! ```toml
//! [bundle]
//! source_roots = ["."]
//! output_dir = "dist"
//! features = ["debug"]
//! preserve_comments = false
//! split_entry_points = false
//!
//! [[entry]]
//! file = "Main.shadml"
//!
//! [[entry]]
//! file = "Render.shadml"
//! ```

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{BundleConfig, BundleError};

/// Top-level project configuration from `shadml.toml`.
#[derive(Debug, Deserialize)]
pub struct ProjectConfig {
    /// Bundle-wide settings.
    #[serde(default)]
    pub bundle: BundleSettings,
    /// Entry point files.
    #[serde(default)]
    pub entry: Vec<EntryConfig>,
}

/// Bundle-wide settings.
#[derive(Debug, Deserialize)]
pub struct BundleSettings {
    /// Directories to search for imported modules.
    /// Default: `["."]` (the directory containing the config file).
    #[serde(default)]
    pub source_roots: Vec<String>,
    /// Output directory for generated `.wgsl` files.
    /// Default: `"dist"`.
    #[serde(default = "default_output_dir")]
    pub output_dir: String,
    /// Feature flags for conditional compilation.
    #[serde(default)]
    pub features: Vec<String>,
    /// Whether to preserve source comments in WGSL output.
    #[serde(default)]
    pub preserve_comments: bool,
    /// Whether to split each entry point into a separate `.wgsl` file.
    #[serde(default)]
    pub split_entry_points: bool,
}

impl Default for BundleSettings {
    fn default() -> Self {
        BundleSettings {
            source_roots: Vec::new(),
            output_dir: default_output_dir(),
            features: Vec::new(),
            preserve_comments: false,
            split_entry_points: false,
        }
    }
}

/// Configuration for a single entry point file.
#[derive(Debug, Deserialize)]
pub struct EntryConfig {
    /// Path to the entry point `.shadml` file, relative to the config file.
    pub file: String,
}

fn default_output_dir() -> String {
    "dist".to_string()
}

/// Load a project configuration from a `shadml.toml` file.
///
/// Paths in the config are resolved relative to the directory containing
/// the config file.
pub fn load_config(config_path: &Path) -> Result<BundleConfig, BundleError> {
    let content = std::fs::read_to_string(config_path)
        .map_err(|e| BundleError::Io(format!("{}: {}", config_path.display(), e)))?;

    parse_config(&content, config_path)
}

/// Parse a config string into a `BundleConfig`.
///
/// `config_path` is used to resolve relative paths.
pub fn parse_config(content: &str, config_path: &Path) -> Result<BundleConfig, BundleError> {
    let project: ProjectConfig = toml::from_str(content)
        .map_err(|e| BundleError::Config(format!("invalid config: {}", e)))?;

    let config_dir = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    // Resolve source roots relative to config directory
    let source_roots = if project.bundle.source_roots.is_empty() {
        vec![config_dir.clone()]
    } else {
        project
            .bundle
            .source_roots
            .iter()
            .map(|r| config_dir.join(r))
            .collect()
    };

    // Resolve entry files relative to config directory
    let entries: Vec<PathBuf> = project
        .entry
        .iter()
        .map(|e| config_dir.join(&e.file))
        .collect();

    if entries.is_empty() {
        return Err(BundleError::Config(
            "no [[entry]] sections in config".into(),
        ));
    }

    Ok(BundleConfig {
        entries,
        source_roots,
        output_dir: config_dir.join(&project.bundle.output_dir),
        features: project.bundle.features,
        preserve_comments: project.bundle.preserve_comments,
        split_entry_points: project.bundle.split_entry_points,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_config() {
        let toml = r#"
[[entry]]
file = "Main.shadml"
"#;
        let config = parse_config(toml, Path::new("/project/shadml.toml")).expect("should parse");

        assert_eq!(config.entries.len(), 1);
        assert_eq!(config.entries[0], PathBuf::from("/project/Main.shadml"));
        assert_eq!(config.output_dir, PathBuf::from("/project/dist"));
        assert!(config.features.is_empty());
        assert!(!config.preserve_comments);
        assert!(!config.split_entry_points);
    }

    #[test]
    fn parse_full_config() {
        let toml = r#"
[bundle]
source_roots = ["src", "lib"]
output_dir = "build/wgsl"
features = ["debug", "profiling"]
preserve_comments = true
split_entry_points = true

[[entry]]
file = "src/Main.shadml"

[[entry]]
file = "src/Render.shadml"
"#;
        let config = parse_config(toml, Path::new("/project/shadml.toml")).expect("should parse");

        assert_eq!(config.entries.len(), 2);
        assert_eq!(config.entries[0], PathBuf::from("/project/src/Main.shadml"));
        assert_eq!(
            config.entries[1],
            PathBuf::from("/project/src/Render.shadml")
        );
        assert_eq!(
            config.source_roots,
            vec![PathBuf::from("/project/src"), PathBuf::from("/project/lib"),]
        );
        assert_eq!(config.output_dir, PathBuf::from("/project/build/wgsl"));
        assert_eq!(config.features, vec!["debug", "profiling"]);
        assert!(config.preserve_comments);
        assert!(config.split_entry_points);
    }

    #[test]
    fn parse_config_default_source_roots() {
        let toml = r#"
[[entry]]
file = "Main.shadml"
"#;
        let config = parse_config(toml, Path::new("/project/shadml.toml")).expect("should parse");

        // Default source root is the config directory
        assert_eq!(config.source_roots, vec![PathBuf::from("/project")]);
    }

    #[test]
    fn parse_config_no_entries_is_error() {
        let toml = r#"
[bundle]
output_dir = "dist"
"#;
        let result = parse_config(toml, Path::new("/project/shadml.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_config_invalid_toml() {
        let toml = "this is not valid toml {{{}}}";
        let result = parse_config(toml, Path::new("/project/shadml.toml"));
        assert!(result.is_err());
    }
}
