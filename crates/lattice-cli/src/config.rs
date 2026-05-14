use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    #[serde(skip)]
    pub path: PathBuf,

    #[serde(default)]
    pub core: CoreConfig,

    #[serde(default)]
    pub ui: UiConfig,

    #[serde(default)]
    pub security: SecurityConfig,

    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct CoreConfig {
    #[serde(default = "default_model")]
    pub default_model: String,
    #[serde(default = "default_true")]
    pub stream: bool,
    #[serde(default = "default_true")]
    pub save_sessions: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct UiConfig {
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_true")]
    pub show_reasoning: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ProviderConfig {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SecurityConfig {
    #[serde(default = "default_sandbox_mode")]
    pub sandbox_mode: String,
    #[serde(default)]
    pub read_allowlist: Vec<String>,
    #[serde(default)]
    pub write_allowlist: Vec<String>,
    #[serde(default)]
    pub command_allowlist: Vec<String>,
    #[serde(default = "default_true")]
    pub hook_chain: bool,
    #[serde(default)]
    pub landlock: bool,
    #[serde(default)]
    pub audit: bool,
    pub audit_dir: Option<String>,
    pub max_command_timeout: Option<u32>,
    pub max_read_size: Option<usize>,
    pub max_write_size: Option<usize>,
    pub max_http_response_size: Option<usize>,
}

fn default_model() -> String {
    "sonnet".into()
}
fn default_theme() -> String {
    "dark".into()
}
fn default_sandbox_mode() -> String {
    "project".into()
}
fn default_true() -> bool {
    true
}

impl Config {
    pub fn load(path: Option<&str>) -> Result<Self> {
        let path = path
            .map(PathBuf::from)
            .or_else(|| dirs::config_dir().map(|d| d.join("lattice").join("config.toml")))
            .unwrap_or_else(|| PathBuf::from("lattice.toml"));

        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            let mut config: Config = toml::from_str(&content)?;
            config.path = path;
            Ok(config)
        } else {
            Ok(Config {
                path,
                ..Default::default()
            })
        }
    }

    pub fn default_model(&self) -> String {
        self.core.default_model.clone()
    }
}

impl Default for Config {
    fn default() -> Self {
        let path = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("lattice")
            .join("config.toml");
        Self {
            path,
            core: Default::default(),
            ui: Default::default(),
            security: Default::default(),
            providers: Default::default(),
        }
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            sandbox_mode: default_sandbox_mode(),
            read_allowlist: vec![],
            write_allowlist: vec![],
            command_allowlist: vec![],
            hook_chain: true,
            landlock: false,
            audit: false,
            audit_dir: None,
            max_command_timeout: None,
            max_read_size: None,
            max_write_size: None,
            max_http_response_size: None,
        }
    }
}
