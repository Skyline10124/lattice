use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    #[serde(skip)]
    pub path: PathBuf,

    #[serde(default)]
    pub core: CoreConfig,

    #[serde(default)]
    pub ui: UiConfig,

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

fn default_model() -> String { "sonnet".into() }
fn default_theme() -> String { "dark".into() }
fn default_true() -> bool { true }

impl Config {
    pub fn load(path: Option<&str>) -> Result<Self> {
        let path = path
            .map(PathBuf::from)
            .or_else(|| {
                dirs::config_dir().map(|d| d.join("artemis").join("config.toml"))
            })
            .unwrap_or_else(|| PathBuf::from("artemis.toml"));

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
        Self {
            path: PathBuf::from("artemis.toml"),
            core: Default::default(),
            ui: Default::default(),
            providers: Default::default(),
        }
    }
}
