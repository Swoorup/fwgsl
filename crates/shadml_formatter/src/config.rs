use serde::{Deserialize, Serialize};

/// How to lay out attributes on record fields and bindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributeStyle {
    /// Keep attributes inline and align them with siblings.
    Inline,
    /// Always place each attribute on its own line.
    OwnLine,
    /// Inline when short, fall back to own-line when a line would exceed
    /// `max_width` or an individual attribute exceeds `attribute_threshold`.
    Auto,
}

impl Default for AttributeStyle {
    fn default() -> Self {
        AttributeStyle::Auto
    }
}

/// Formatting configuration for shadml source code.
///
/// All fields have sensible defaults so that `FormatConfig::default()`
/// produces reasonable output without any configuration file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatConfig {
    /// Number of spaces per indentation level.
    #[serde(default = "default_indent_width")]
    pub indent_width: usize,

    /// Maximum line width.  When `enforce_max_width` is `true`, the formatter
    /// will attempt to break lines that would exceed this limit.
    #[serde(default = "default_max_width")]
    pub max_width: usize,

    /// Whether to enforce `max_width` by breaking long lines.
    #[serde(default = "default_enforce_max_width")]
    pub enforce_max_width: bool,

    /// An attribute whose text representation is longer than this threshold
    /// (in bytes) is considered "long" and may trigger a fallback to
    /// `OwnLine` style when `record_attribute_style` or
    /// `binding_attribute_style` is `Auto`.
    #[serde(default = "default_attribute_threshold")]
    pub attribute_threshold: usize,

    /// How to lay out attributes on record fields.
    #[serde(default)]
    pub record_attribute_style: AttributeStyle,

    /// How to lay out attributes on render-block bindings.
    #[serde(default)]
    pub binding_attribute_style: AttributeStyle,
}

impl Default for FormatConfig {
    fn default() -> Self {
        Self {
            indent_width: default_indent_width(),
            max_width: default_max_width(),
            enforce_max_width: default_enforce_max_width(),
            attribute_threshold: default_attribute_threshold(),
            record_attribute_style: AttributeStyle::default(),
            binding_attribute_style: AttributeStyle::default(),
        }
    }
}

fn default_indent_width() -> usize {
    2
}

fn default_max_width() -> usize {
    100
}

fn default_enforce_max_width() -> bool {
    true
}

fn default_attribute_threshold() -> usize {
    25
}

/// Load a formatter configuration from a TOML string.
///
/// The TOML should contain a `[formatter]` table, e.g.:
///
/// ```toml
/// [formatter]
/// max_width = 100
/// indent_width = 2
/// record_attribute_style = "auto"
/// binding_attribute_style = "auto"
/// ```
///
/// Returns `Ok(None)` when the input contains no `[formatter]` section.
pub fn load_formatter_config(toml_text: &str) -> Result<Option<FormatConfig>, String> {
    #[derive(Debug, Deserialize)]
    struct Wrapper {
        formatter: Option<FormatConfig>,
    }

    let wrapper: Wrapper = toml::from_str(toml_text).map_err(|e| format!("invalid TOML: {}", e))?;

    Ok(wrapper.formatter)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_values() {
        let c = FormatConfig::default();
        assert_eq!(c.indent_width, 2);
        assert_eq!(c.max_width, 100);
        assert!(c.enforce_max_width);
        assert_eq!(c.attribute_threshold, 25);
        assert_eq!(c.record_attribute_style, AttributeStyle::Auto);
        assert_eq!(c.binding_attribute_style, AttributeStyle::Auto);
    }

    #[test]
    fn parse_full_config() {
        let toml = r#"
[formatter]
indent_width = 4
max_width = 120
enforce_max_width = false
attribute_threshold = 30
record_attribute_style = "inline"
binding_attribute_style = "own_line"
"#;
        let cfg = load_formatter_config(toml).unwrap().unwrap();
        assert_eq!(cfg.indent_width, 4);
        assert_eq!(cfg.max_width, 120);
        assert!(!cfg.enforce_max_width);
        assert_eq!(cfg.attribute_threshold, 30);
        assert_eq!(cfg.record_attribute_style, AttributeStyle::Inline);
        assert_eq!(cfg.binding_attribute_style, AttributeStyle::OwnLine);
    }

    #[test]
    fn parse_partial_config() {
        let toml = r#"
[formatter]
max_width = 80
"#;
        let cfg = load_formatter_config(toml).unwrap().unwrap();
        assert_eq!(cfg.max_width, 80);
        assert_eq!(cfg.indent_width, 2); // default
        assert!(cfg.enforce_max_width); // default
    }

    #[test]
    fn parse_missing_section() {
        let toml = r#"
[bundle]
source_roots = ["."]
"#;
        assert!(load_formatter_config(toml).unwrap().is_none());
    }

    #[test]
    fn parse_invalid_style() {
        let toml = r#"
[formatter]
record_attribute_style = "unknown"
"#;
        assert!(load_formatter_config(toml).is_err());
    }
}
