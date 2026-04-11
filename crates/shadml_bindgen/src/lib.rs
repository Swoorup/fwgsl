mod config;
mod generator;
pub mod layout;

use std::path::{Path, PathBuf};

pub use config::{NamedProfile, RustBindgenConfig, SourceMode, TypeMapPreset};
use shadml_bundler::{bundle_manifest, BundleConfig, ShaderBundleManifest};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ShadmlBindgenError {
    #[error("bundle generation failed: {0}")]
    Bundle(#[from] shadml_bundler::BundleError),
    #[error("I/O error: {0}")]
    Io(String),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("unsupported configuration: {0}")]
    Unsupported(String),
    #[error("reflection error: {0}")]
    Reflection(String),
    #[error("code generation error: {0}")]
    Codegen(String),
}

#[derive(Debug, Clone, Default)]
pub struct ShadmlBindgenBuilder {
    project_root: Option<PathBuf>,
    config_path: Option<PathBuf>,
    entries: Vec<PathBuf>,
    source_roots: Vec<PathBuf>,
    features: Vec<String>,
    profiles: Vec<NamedProfile>,
    output: Option<PathBuf>,
    type_map: Option<TypeMapPreset>,
    source_mode: Option<SourceMode>,
    emit_rerun_if_changed: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct ShadmlBindgen {
    output: PathBuf,
    config_path: Option<PathBuf>,
    emit_rerun_if_changed: bool,
    source_mode: SourceMode,
    type_map: TypeMapPreset,
    manifest: shadml_bundler::ShaderBundleManifest,
}

impl ShadmlBindgenBuilder {
    pub fn project_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.project_root = Some(path.into());
        self
    }

    pub fn config(mut self, path: impl Into<PathBuf>) -> Self {
        self.config_path = Some(path.into());
        self
    }

    pub fn entry(mut self, path: impl Into<PathBuf>) -> Self {
        self.entries.push(path.into());
        self
    }

    pub fn source_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.source_roots.push(path.into());
        self
    }

    pub fn feature(mut self, name: impl Into<String>) -> Self {
        self.features.push(name.into());
        self
    }

    pub fn profile(mut self, name: impl Into<String>) -> Self {
        self.profiles.push(NamedProfile {
            name: name.into(),
            features: Vec::new(),
        });
        self
    }

    pub fn profile_with_features(
        mut self,
        name: impl Into<String>,
        features: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.profiles.push(NamedProfile {
            name: name.into(),
            features: features.into_iter().map(Into::into).collect(),
        });
        self
    }

    pub fn type_map(mut self, preset: TypeMapPreset) -> Self {
        self.type_map = Some(preset);
        self
    }

    pub fn source_mode(mut self, mode: SourceMode) -> Self {
        self.source_mode = Some(mode);
        self
    }

    pub fn emit_rerun_if_changed(mut self, yes: bool) -> Self {
        self.emit_rerun_if_changed = Some(yes);
        self
    }

    pub fn output(mut self, path: impl Into<PathBuf>) -> Self {
        self.output = Some(path.into());
        self
    }

    pub fn build(self) -> Result<ShadmlBindgen, ShadmlBindgenError> {
        let project_root = match self.project_root {
            Some(path) => path,
            None => std::env::current_dir()
                .map_err(|error| ShadmlBindgenError::Io(error.to_string()))?,
        };

        let config_path = self
            .config_path
            .map(|path| resolve_path(&project_root, &path));

        let rust_config = match config_path.as_ref() {
            Some(path) => {
                config::load_rust_bindgen_config(path).map_err(ShadmlBindgenError::Config)?
            }
            None => RustBindgenConfig::default(),
        };

        let mut bundle_config = if let Some(path) = config_path.as_ref() {
            shadml_bundler::config::load_config(path)?
        } else {
            BundleConfig::default()
        };

        if !self.entries.is_empty() {
            bundle_config.entries = self
                .entries
                .iter()
                .map(|path| resolve_path(&project_root, path))
                .collect();
        }

        if bundle_config.entries.is_empty() {
            return Err(ShadmlBindgenError::Config(
                "no entry files specified for bindgen".into(),
            ));
        }

        if !self.source_roots.is_empty() {
            bundle_config.source_roots = self
                .source_roots
                .iter()
                .map(|path| resolve_path(&project_root, path))
                .collect();
        }

        if bundle_config.source_roots.is_empty() {
            bundle_config.source_roots = default_source_roots(&bundle_config.entries);
        }

        if !self.features.is_empty() {
            bundle_config.features = self.features.clone();
        }

        let mut profiles = if !self.profiles.is_empty() {
            self.profiles
        } else {
            rust_config.profiles.clone()
        };

        if profiles.is_empty() {
            profiles.push(NamedProfile {
                name: "default".into(),
                features: bundle_config.features.clone(),
            });
        }

        for profile in &mut profiles {
            if profile.features.is_empty() {
                profile.features = bundle_config.features.clone();
            }
        }

        let mut seen_profiles = std::collections::BTreeSet::new();
        for profile in &mut profiles {
            profile.features.sort();
            profile.features.dedup();
            if !seen_profiles.insert(profile.name.clone()) {
                return Err(ShadmlBindgenError::Config(format!(
                    "duplicate rust profile '{}'",
                    profile.name
                )));
            }
        }
        let output = match self.output.or(rust_config.output) {
            Some(path) => resolve_path(&project_root, &path),
            None => {
                return Err(ShadmlBindgenError::Config(
                    "missing bindgen output path".into(),
                ))
            }
        };

        let emit_rerun_if_changed = self
            .emit_rerun_if_changed
            .unwrap_or(rust_config.emit_rerun_if_changed);
        let type_map = self
            .type_map
            .or(Some(rust_config.type_map))
            .unwrap_or_default();
        let source_mode = self.source_mode.unwrap_or(rust_config.source_mode);

        match source_mode {
            SourceMode::EmbeddedDebug | SourceMode::EmbeddedMinified => {}
            SourceMode::RuntimeBlob | SourceMode::ServerFetch => {
                return Err(ShadmlBindgenError::Unsupported(
                    "RuntimeBlob and ServerFetch are planned but not implemented yet".into(),
                ))
            }
        }

        let mut manifest = ShaderBundleManifest::default();
        for profile in profiles {
            bundle_config.features = profile.features.clone();
            let mut profile_manifest = bundle_manifest(&bundle_config, profile.name)?;
            manifest.profiles.append(&mut profile_manifest.profiles);
        }

        Ok(ShadmlBindgen {
            output,
            config_path,
            emit_rerun_if_changed,
            source_mode,
            type_map,
            manifest,
        })
    }
}

impl ShadmlBindgen {
    pub fn generate(self) -> Result<(), ShadmlBindgenError> {
        if self.manifest.profiles.is_empty() {
            return Err(ShadmlBindgenError::Codegen(
                "bundle manifest did not contain any compiled profiles".into(),
            ));
        }

        let reflections =
            layout::reflect_manifest(&self.manifest).map_err(ShadmlBindgenError::Reflection)?;
        let source = generator::generate_rust_source(
            &self.manifest,
            &reflections,
            self.source_mode,
            self.type_map,
        )
        .map_err(|e| ShadmlBindgenError::Codegen(e.to_string()))?;

        if let Some(parent) = self.output.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| ShadmlBindgenError::Io(error.to_string()))?;
        }

        std::fs::write(&self.output, source)
            .map_err(|error| ShadmlBindgenError::Io(error.to_string()))?;

        if self.emit_rerun_if_changed {
            if let Some(config_path) = &self.config_path {
                println!("cargo:rerun-if-changed={}", config_path.display());
            }
            let mut emitted = std::collections::BTreeSet::new();
            for profile in &self.manifest.profiles {
                for source_file in &profile.source_files {
                    if emitted.insert(source_file.clone()) {
                        println!("cargo:rerun-if-changed={}", source_file.display());
                    }
                }
            }
        }

        Ok(())
    }
}

fn default_source_roots(entries: &[PathBuf]) -> Vec<PathBuf> {
    entries
        .first()
        .and_then(|entry| entry.parent())
        .map(|path| vec![path.to_path_buf()])
        .unwrap_or_else(|| vec![PathBuf::from(".")])
}

fn resolve_path(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}
