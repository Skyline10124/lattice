use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::fs;

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

    #[allow(dead_code)]
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub model: String,
    pub created_at: String,
    pub message_count: usize,
}

pub struct SessionManager {
    dir: PathBuf,
}

impl SessionManager {
    pub fn new() -> Self {
        let dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("lattice")
            .join("sessions");
        Self { dir }
    }

    #[allow(dead_code)]
    pub fn from_dir(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn save(&self, session: &Session) -> Result<()> {
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("Failed to create sessions directory: {:?}", self.dir))?;
        let path = self.dir.join(format!("{}.json", session.id));
        let content = serde_json::to_string_pretty(session)
            .with_context(|| format!("Failed to serialize session {}", session.id))?;
        fs::write(&path, content)
            .with_context(|| format!("Failed to write session file: {:?}", path))?;
        Ok(())
    }

    pub fn load(&self, id: &str) -> Result<Option<Session>> {
        let path = self.dir.join(format!("{}.json", id));
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read session file: {:?}", path))?;
        let session: Session = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse session file: {:?}", path))?;
        Ok(Some(session))
    }

    pub fn list(&self) -> Result<Vec<SessionSummary>> {
        if !self.dir.exists() {
            return Ok(vec![]);
        }
        let mut summaries = Vec::new();
        let entries = fs::read_dir(&self.dir)
            .with_context(|| format!("Failed to read sessions directory: {:?}", self.dir))?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(session) = serde_json::from_str::<Session>(&content) {
                        summaries.push(SessionSummary {
                            id: session.id,
                            model: session.model,
                            created_at: session.created_at,
                            message_count: session.messages.len(),
                        });
                    }
                }
            }
        }
        summaries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(summaries)
    }

    pub fn delete(&self, id: &str) -> Result<bool> {
        let path = self.dir.join(format!("{}.json", id));
        if !path.exists() {
            return Ok(false);
        }
        fs::remove_file(&path)
            .with_context(|| format!("Failed to delete session file: {:?}", path))?;
        Ok(true)
    }

    #[allow(dead_code)]
    pub fn latest(&self) -> Result<Option<Session>> {
        let summaries = self.list()?;
        match summaries.into_iter().next() {
            Some(summary) => self.load(&summary.id),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir()
            .join("lattice-cli-tests")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn test_session(id: &str) -> Session {
        Session {
            id: id.to_string(),
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            messages: vec![
                SessionMessage {
                    role: "user".to_string(),
                    content: "hello".to_string(),
                },
                SessionMessage {
                    role: "assistant".to_string(),
                    content: "hi".to_string(),
                },
            ],
            created_at: chrono::Local::now().to_rfc3339(),
        }
    }

    #[test]
    fn test_save_and_load() -> Result<()> {
        let dir = temp_dir("test_save_and_load");
        let manager = SessionManager::from_dir(dir.clone());
        let session = test_session("save-load-1");
        manager.save(&session)?;

        let loaded = manager.load("save-load-1")?.expect("should load saved session");
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.model, session.model);
        assert_eq!(loaded.provider, session.provider);
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.message_count(), 2);
        assert_eq!(loaded.messages[0].role, "user");
        assert_eq!(loaded.messages[1].content, "hi");

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn test_list() -> Result<()> {
        let dir = temp_dir("test_list");
        let manager = SessionManager::from_dir(dir.clone());
        manager.save(&test_session("list-a"))?;
        manager.save(&test_session("list-b"))?;

        let summaries = manager.list()?;
        assert_eq!(summaries.len(), 2);

        let ids: Vec<&str> = summaries.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"list-a"));
        assert!(ids.contains(&"list-b"));

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn test_delete() -> Result<()> {
        let dir = temp_dir("test_delete");
        let manager = SessionManager::from_dir(dir.clone());

        manager.save(&test_session("delete-me"))?;
        assert!(manager.load("delete-me")?.is_some());

            let deleted = manager.delete("delete-me")?;
            assert!(deleted);
            assert!(manager.load("delete-me")?.is_none());

            let deleted = manager.delete("does-not-exist")?;
        assert!(!deleted);

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn test_load_nonexistent() -> Result<()> {
        let dir = temp_dir("test_load_nonexistent");
        let manager = SessionManager::from_dir(dir.clone());

        let result = manager.load("ghost-session")?;
        assert!(result.is_none());

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn test_empty_dir_returns_empty_list() -> Result<()> {
        let dir = temp_dir("test_empty_dir_returns_empty_list");
        let manager = SessionManager::from_dir(dir.clone());

        let list = manager.list()?;
        assert!(list.is_empty());

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn test_latest() -> Result<()> {
        let dir = temp_dir("test_latest");
        let manager = SessionManager::from_dir(dir.clone());

        assert!(manager.latest()?.is_none());

        manager.save(&test_session("older"))?;
        manager.save(&test_session("newer"))?;
        let latest = manager.latest()?;
        assert!(latest.is_some());

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }
}
