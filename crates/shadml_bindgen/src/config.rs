use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
pub enum SourceMode {
    #[default]
    EmbeddedDebug,
    EmbeddedMinified,
    RuntimeBlob,
    ServerFetch,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
pub enum TypeMapPreset {
    #[default]
    Plain,
    Glam,
    Nalgebra,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedProfile {
    pub name: String,
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RustBindgenConfig {
    pub output: Option<PathBuf>,
    pub emit_rerun_if_changed: bool,
    pub type_map: TypeMapPreset,
    pub source_mode: SourceMode,
    pub profiles: Vec<NamedProfile>,
}

#[derive(Debug, Deserialize, Default)]
struct ProjectConfigFile {
    #[serde(default)]
    rust: RustSectionConfig,
}

#[derive(Debug, Deserialize, Default)]
struct RustSectionConfig {
    output: Option<String>,
    emit_rerun_if_changed: Option<bool>,
    type_map: Option<TypeMapPreset>,
    source_mode: Option<SourceMode>,
    #[serde(default)]
    profiles: Vec<NamedProfileConfig>,
    #[serde(default)]
    profile: Vec<NamedProfileConfig>,
}

#[derive(Debug, Deserialize, Default, Clone)]
struct NamedProfileConfig {
    name: String,
    #[serde(default)]
    features: Vec<String>,
}

pub fn load_rust_bindgen_config(config_path: &Path) -> Result<RustBindgenConfig, String> {
    let content = std::fs::read_to_string(config_path)
        .map_err(|error| format!("{}: {}", config_path.display(), error))?;
    let parsed: ProjectConfigFile =
        toml::from_str(&content).map_err(|error| format!("invalid config: {}", error))?;
    let config_dir = config_path.parent().unwrap_or_else(|| Path::new("."));

    let mut profiles = parsed
        .rust
        .profiles
        .into_iter()
        .chain(parsed.rust.profile)
        .map(|profile| NamedProfile {
            name: profile.name,
            features: profile.features,
        })
        .collect::<Vec<_>>();
    profiles.sort_by(|lhs, rhs| lhs.name.cmp(&rhs.name));

    Ok(RustBindgenConfig {
        output: parsed.rust.output.map(|output| config_dir.join(output)),
        emit_rerun_if_changed: parsed.rust.emit_rerun_if_changed.unwrap_or(true),
        type_map: parsed.rust.type_map.unwrap_or_default(),
        source_mode: parsed.rust.source_mode.unwrap_or_default(),
        profiles,
    })
}
