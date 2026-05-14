use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::bundle::{BehaviorMode, PluginBundle, PluginMeta, YoloSandboxPolicy};
use crate::erased::ErasedPlugin;
use crate::PluginError;

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("TOML parse error at {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("plugin manifest at {path} is invalid: {message}")]
    Invalid { path: PathBuf, message: String },
}

#[derive(Debug, Deserialize)]
struct PluginToml {
    plugin: ManifestPlugin,
    #[serde(default)]
    prompt: ManifestPrompt,
    #[serde(default)]
    output: ManifestOutput,
    #[serde(default)]
    behavior: Option<ManifestBehavior>,
}

#[derive(Debug, Deserialize)]
struct ManifestPlugin {
    name: String,
    #[serde(default = "default_version")]
    version: String,
    #[serde(default)]
    description: String,
    #[serde(default = "default_author")]
    author: String,
    #[serde(default)]
    preferred_model: String,
}

#[derive(Debug, Default, Deserialize)]
struct ManifestPrompt {
    #[serde(default)]
    system: String,
    #[serde(default)]
    system_file: Option<String>,
    #[serde(default)]
    template: String,
    #[serde(default)]
    template_file: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ManifestOutput {
    #[serde(default = "default_parse")]
    parse: String,
    #[serde(default)]
    schema: Option<serde_json::Value>,
    #[serde(default)]
    schema_file: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ManifestBehavior {
    #[serde(default = "default_behavior_mode")]
    mode: String,
    #[serde(default = "default_confidence_threshold")]
    confidence_threshold: f64,
    #[serde(default = "default_max_retries")]
    max_retries: u32,
    #[serde(default)]
    escalate_to: Option<String>,
}

fn default_version() -> String {
    "0.1".into()
}

fn default_author() -> String {
    "local".into()
}

fn default_parse() -> String {
    "json".into()
}

fn default_behavior_mode() -> String {
    "yolo".into()
}

fn default_confidence_threshold() -> f64 {
    0.7
}

fn default_max_retries() -> u32 {
    3
}

/// A declarative local plugin loaded from `plugin.toml`.
pub struct ManifestPluginImpl {
    name: String,
    system_prompt: String,
    prompt_template: String,
    parse_mode: ParseMode,
    schema: Option<serde_json::Value>,
    preferred_model: String,
}

enum ParseMode {
    Json,
    Text,
}

impl ManifestPluginImpl {
    fn render_template(&self, context: &serde_json::Value) -> String {
        let mut rendered = self.prompt_template.clone();
        if rendered.is_empty() {
            rendered = "{{context}}".into();
        }
        rendered = rendered.replace("{{context}}", &context.to_string());
        if let Some(input) = context.get("input").and_then(|v| v.as_str()) {
            rendered = rendered.replace("{{input}}", input);
        }
        if let Some(request) = context.get("request").and_then(|v| v.as_str()) {
            rendered = rendered.replace("{{request}}", request);
        }
        rendered
    }
}

impl ErasedPlugin for ManifestPluginImpl {
    fn name(&self) -> &str {
        &self.name
    }

    fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    fn to_prompt_json(&self, context: &serde_json::Value) -> Result<String, PluginError> {
        Ok(self.render_template(context))
    }

    fn parse_output_json(&self, raw: &str) -> Result<serde_json::Value, PluginError> {
        match self.parse_mode {
            ParseMode::Json => crate::parse_utils::parse_json_from_response(raw),
            ParseMode::Text => Ok(serde_json::json!({ "content": raw })),
        }
    }

    fn tools(&self) -> &[lattice_core::types::ToolDefinition] {
        &[]
    }

    fn preferred_model(&self) -> &str {
        &self.preferred_model
    }

    fn output_schema(&self) -> Option<serde_json::Value> {
        self.schema.clone()
    }
}

pub fn load_manifest(path: &Path) -> Result<PluginBundle, ManifestError> {
    let content = std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let raw: PluginToml = toml::from_str(&content).map_err(|source| ManifestError::Toml {
        path: path.to_path_buf(),
        source,
    })?;
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));

    let system_prompt = read_inline_or_file(
        path,
        base_dir,
        raw.prompt.system,
        raw.prompt.system_file.as_deref(),
        "prompt.system_file",
    )?;
    let prompt_template = read_inline_or_file(
        path,
        base_dir,
        raw.prompt.template,
        raw.prompt.template_file.as_deref(),
        "prompt.template_file",
    )?;
    let schema = match (raw.output.schema, raw.output.schema_file.as_deref()) {
        (Some(schema), None) => Some(schema),
        (None, Some(schema_file)) => {
            let schema_path =
                safe_relative_path(path, base_dir, schema_file, "output.schema_file")?;
            let schema_content =
                std::fs::read_to_string(&schema_path).map_err(|source| ManifestError::Io {
                    path: schema_path.clone(),
                    source,
                })?;
            let schema =
                serde_json::from_str(&schema_content).map_err(|e| ManifestError::Invalid {
                    path: path.to_path_buf(),
                    message: format!("output.schema_file is not valid JSON: {e}"),
                })?;
            Some(schema)
        }
        (Some(_), Some(_)) => {
            return Err(ManifestError::Invalid {
                path: path.to_path_buf(),
                message: "use output.schema or output.schema_file, not both".into(),
            })
        }
        (None, None) => None,
    };

    let parse_mode = match raw.output.parse.as_str() {
        "json" => ParseMode::Json,
        "text" => ParseMode::Text,
        other => {
            return Err(ManifestError::Invalid {
                path: path.to_path_buf(),
                message: format!("unsupported output.parse '{other}'"),
            })
        }
    };

    let default_behavior = match raw.behavior {
        Some(b) if b.mode == "strict" => BehaviorMode::Strict {
            confidence_threshold: b.confidence_threshold,
            max_retries: b.max_retries,
            escalate_to: b.escalate_to,
        },
        Some(b) if b.mode == "yolo" => BehaviorMode::Yolo {
            enforce_sandbox: YoloSandboxPolicy::EnforceCommandAllowlist,
        },
        Some(b) => {
            return Err(ManifestError::Invalid {
                path: path.to_path_buf(),
                message: format!("unsupported behavior mode '{}'", b.mode),
            })
        }
        None => BehaviorMode::default(),
    };

    let plugin = ManifestPluginImpl {
        name: raw.plugin.name.clone(),
        system_prompt,
        prompt_template,
        parse_mode,
        schema,
        preferred_model: raw.plugin.preferred_model.clone(),
    };

    Ok(PluginBundle {
        meta: PluginMeta {
            name: raw.plugin.name,
            version: raw.plugin.version,
            description: raw.plugin.description,
            author: raw.plugin.author,
        },
        plugin: Box::new(plugin),
        default_behavior,
        default_tools: vec![],
    })
}

fn read_inline_or_file(
    manifest_path: &Path,
    base_dir: &Path,
    inline: String,
    file: Option<&str>,
    field: &str,
) -> Result<String, ManifestError> {
    match file {
        Some(_) if !inline.is_empty() => Err(ManifestError::Invalid {
            path: manifest_path.to_path_buf(),
            message: format!("use inline {field} content or file reference, not both"),
        }),
        Some(file) => {
            let path = safe_relative_path(manifest_path, base_dir, file, field)?;
            std::fs::read_to_string(&path).map_err(|source| ManifestError::Io { path, source })
        }
        None => Ok(inline),
    }
}

fn safe_relative_path(
    manifest_path: &Path,
    base_dir: &Path,
    relative: &str,
    field: &str,
) -> Result<PathBuf, ManifestError> {
    let path = Path::new(relative);
    if path.is_absolute() || relative.contains("..") {
        return Err(ManifestError::Invalid {
            path: manifest_path.to_path_buf(),
            message: format!("{field} must be a relative path without '..'"),
        });
    }
    Ok(base_dir.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_plugin_renders_and_parses_json() {
        let plugin = ManifestPluginImpl {
            name: "local:test".into(),
            system_prompt: "system".into(),
            prompt_template: "Request: {{input}} / {{context}}".into(),
            parse_mode: ParseMode::Json,
            schema: None,
            preferred_model: String::new(),
        };
        let prompt = plugin
            .to_prompt_json(&serde_json::json!({"input": "hello"}))
            .unwrap();
        assert!(prompt.contains("hello"));
        let parsed = plugin.parse_output_json(r#"{"ok": true}"#).unwrap();
        assert_eq!(parsed["ok"], true);
    }

    #[test]
    fn load_manifest_from_files() {
        let root =
            std::env::temp_dir().join(format!("lattice_manifest_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("system.md"), "system prompt").unwrap();
        std::fs::write(root.join("template.md"), "Hello {{input}}").unwrap();
        std::fs::write(
            root.join("plugin.toml"),
            r#"
[plugin]
name = "local:test"
description = "test plugin"

[prompt]
system_file = "system.md"
template_file = "template.md"

[output]
parse = "text"
"#,
        )
        .unwrap();

        let bundle = load_manifest(&root.join("plugin.toml")).unwrap();
        assert_eq!(bundle.meta.name, "local:test");
        assert_eq!(bundle.plugin.system_prompt(), "system prompt");
        let prompt = bundle
            .plugin
            .to_prompt_json(&serde_json::json!({"input": "world"}))
            .unwrap();
        assert_eq!(prompt, "Hello world");

        let _ = std::fs::remove_dir_all(&root);
    }
}
