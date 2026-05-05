use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_SESSION_ID_LEN: usize = 128;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub model: String,
    pub provider: String,
    #[serde(default)]
    pub title: Option<String>,
    pub messages: Vec<SessionMessage>,
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    pub role: String,
    pub content: String,
}

impl Session {
    pub fn new(model: String, provider: String) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let hash = Sha256::digest(format!("{}|{}|{}", model, provider, now));
        let id = format!("{:016x}", u64::from_be_bytes(hash[..8].try_into().unwrap()));
        let timestamp = now_rfc3339();
        Self {
            id,
            model,
            provider,
            title: None,
            messages: vec![],
            created_at: timestamp.clone(),
            updated_at: timestamp,
        }
    }

    #[allow(dead_code)]
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    pub fn append_turn(
        &mut self,
        model: String,
        provider: String,
        user: String,
        assistant: String,
    ) {
        self.model = model;
        self.provider = provider;
        self.push_message("user", user);
        self.push_message("assistant", assistant);
        self.touch();
    }

    pub fn push_message(&mut self, role: &str, content: String) {
        if role.eq_ignore_ascii_case("user") && self.title.is_none() {
            self.title = title_from_content(&content);
        }
        self.messages.push(SessionMessage {
            role: role.to_string(),
            content,
        });
    }

    pub fn normalize(&mut self) {
        if self.updated_at.is_empty() {
            self.updated_at = self.created_at.clone();
        }
        if self.title.is_none() {
            self.title = self
                .messages
                .iter()
                .find(|m| m.role.eq_ignore_ascii_case("user"))
                .and_then(|m| title_from_content(&m.content));
        }
    }

    fn touch(&mut self) {
        self.updated_at = now_rfc3339();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub model: String,
    pub provider: String,
    pub title: Option<String>,
    pub created_at: String,
    pub updated_at: String,
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
        validate_session_id(&session.id)?;
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("Failed to create sessions directory: {:?}", self.dir))?;
        let path = self.session_path(&session.id)?;
        let content = serde_json::to_string_pretty(session)
            .with_context(|| format!("Failed to serialize session {}", session.id))?;
        let tmp_path = self
            .dir
            .join(format!("{}.json.tmp-{}", session.id, std::process::id()));
        fs::write(&tmp_path, content)
            .with_context(|| format!("Failed to write temporary session file: {:?}", tmp_path))?;
        fs::rename(&tmp_path, &path)
            .with_context(|| format!("Failed to replace session file: {:?}", path))?;
        Ok(())
    }

    pub fn load(&self, id: &str) -> Result<Option<Session>> {
        let path = self.session_path(id)?;
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read session file: {:?}", path))?;
        let mut session: Session = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse session file: {:?}", path))?;
        session.normalize();
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
                    if let Ok(mut session) = serde_json::from_str::<Session>(&content) {
                        session.normalize();
                        summaries.push(SessionSummary {
                            id: session.id,
                            model: session.model,
                            provider: session.provider,
                            title: session.title,
                            created_at: session.created_at,
                            updated_at: session.updated_at,
                            message_count: session.messages.len(),
                        });
                    }
                }
            }
        }
        summaries.sort_by(|a, b| {
            session_sort_key(&b.updated_at)
                .cmp(&session_sort_key(&a.updated_at))
                .then_with(|| b.id.cmp(&a.id))
        });
        Ok(summaries)
    }

    pub fn delete(&self, id: &str) -> Result<bool> {
        let path = self.session_path(id)?;
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

    fn session_path(&self, id: &str) -> Result<PathBuf> {
        validate_session_id(id)?;
        Ok(self.dir.join(format!("{}.json", id)))
    }
}

pub fn build_prompt_with_history(session: Option<&Session>, prompt: &str) -> String {
    let Some(session) = session else {
        return prompt.to_string();
    };

    let mut ctx = String::from("Previous conversation:\n");
    for msg in &session.messages {
        let role = match msg.role.to_lowercase().as_str() {
            "user" => "User",
            "assistant" => "Assistant",
            "system" => "System",
            "tool" => "Tool",
            _ => &msg.role,
        };
        ctx.push_str(&format!("{}: {}\n", role, msg.content));
    }
    ctx.push_str("---\n");
    ctx.push_str(prompt);
    ctx
}

pub fn finalize_session_turn(
    previous_session: Option<Session>,
    model: String,
    provider: String,
    user: String,
    assistant: String,
) -> Session {
    let mut session =
        previous_session.unwrap_or_else(|| Session::new(model.clone(), provider.clone()));
    session.append_turn(model, provider, user, assistant);
    session
}

fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339()
}

fn validate_session_id(id: &str) -> Result<()> {
    let valid = !id.is_empty()
        && id.len() <= MAX_SESSION_ID_LEN
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid {
        Ok(())
    } else {
        anyhow::bail!("Invalid session id: {}", id)
    }
}

fn title_from_content(content: &str) -> Option<String> {
    let trimmed = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(80).collect())
}

fn session_sort_key(timestamp: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir().join("lattice-cli-tests").join(name);
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn test_session(id: &str) -> Session {
        let timestamp = chrono::Local::now().to_rfc3339();
        Session {
            id: id.to_string(),
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            title: None,
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
            created_at: timestamp.clone(),
            updated_at: timestamp,
        }
    }

    #[test]
    fn test_save_and_load() -> Result<()> {
        let dir = temp_dir("test_save_and_load");
        let manager = SessionManager::from_dir(dir.clone());
        let session = test_session("save-load-1");
        manager.save(&session)?;

        let loaded = manager
            .load("save-load-1")?
            .expect("should load saved session");
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.model, session.model);
        assert_eq!(loaded.provider, session.provider);
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.message_count(), 2);
        assert_eq!(loaded.title.as_deref(), Some("hello"));
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

        let mut older = test_session("older");
        older.created_at = "2026-05-01T00:00:00+08:00".into();
        older.updated_at = "2026-05-01T00:00:00+08:00".into();
        let mut newer = test_session("newer");
        newer.created_at = "2026-05-01T00:00:00+08:00".into();
        newer.updated_at = "2026-05-02T00:00:00+08:00".into();
        manager.save(&older)?;
        manager.save(&newer)?;
        let latest = manager.latest()?;
        assert!(latest.is_some());
        assert_eq!(latest.unwrap().id, "newer");

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn test_append_turn_updates_existing_session() -> Result<()> {
        let dir = temp_dir("test_append_turn_updates_existing_session");
        let manager = SessionManager::from_dir(dir.clone());

        let mut session = test_session("thread-1");
        session.append_turn(
            "next-model".into(),
            "next-provider".into(),
            "follow up".into(),
            "answer".into(),
        );
        manager.save(&session)?;

        let loaded = manager.load("thread-1")?.expect("session should exist");
        assert_eq!(loaded.id, "thread-1");
        assert_eq!(loaded.model, "next-model");
        assert_eq!(loaded.provider, "next-provider");
        assert_eq!(loaded.messages.len(), 4);
        assert_eq!(loaded.messages[2].content, "follow up");
        assert_eq!(loaded.messages[3].content, "answer");

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn test_invalid_session_id_is_rejected() -> Result<()> {
        let dir = temp_dir("test_invalid_session_id_is_rejected");
        let manager = SessionManager::from_dir(dir.clone());

        assert!(manager.load("../escape").is_err());
        assert!(manager.delete("bad/id").is_err());

        let mut session = test_session("bad.id");
        assert!(manager.save(&session).is_err());
        session.id = "good_id-1".into();
        assert!(manager.save(&session).is_ok());

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn test_build_prompt_with_history() {
        let session = test_session("history");
        let prompt = build_prompt_with_history(Some(&session), "again");
        assert!(prompt.contains("Previous conversation:"));
        assert!(prompt.contains("User: hello"));
        assert!(prompt.contains("Assistant: hi"));
        assert!(prompt.ends_with("again"));
        assert_eq!(build_prompt_with_history(None, "plain"), "plain");
    }
}
