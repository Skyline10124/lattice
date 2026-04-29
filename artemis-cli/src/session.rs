use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub model: String,
    pub provider: String,
    pub messages: Vec<SessionMessage>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    pub role: String,
    pub content: String,
}

impl Session {
    pub fn new(model: String, provider: String) -> Self {
        let id = chrono::Local::now().format("%Y-%m-%d-%H%M%S").to_string();
        Self {
            id,
            model,
            provider,
            messages: vec![],
            created_at: chrono::Local::now().to_rfc3339(),
        }
    }

    pub fn dir() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("artemis")
            .join("sessions")
    }
}
