use crate::{
    compact_json, object_id, NewTurnManifest, Result, SessionMessage, TURN_MANIFEST_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};

const SECRET_FIELD_NAMES: &[&str] = &[
    "api_key",
    "apiKey",
    "authorization",
    "credentials",
    "env",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnOutcomeStatus {
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "cancelled")]
    Cancelled,
}

impl TurnOutcomeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileSourceAudit {
    pub source_label: String,
    #[serde(default)]
    pub source_fingerprint: Option<String>,
}

impl ProfileSourceAudit {
    pub fn new(
        source_label: impl Into<String>,
        source_fingerprint: Option<impl Into<String>>,
    ) -> Self {
        Self {
            source_label: source_label.into(),
            source_fingerprint: source_fingerprint.map(Into::into),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileSnapshotSummary {
    pub profile_name: String,
    pub source_label: String,
    pub model: String,
    #[serde(default)]
    pub tool_bundles: Vec<String>,
    #[serde(default)]
    pub enabled_tools: Vec<String>,
    #[serde(default)]
    pub security_mode: Option<String>,
    #[serde(default)]
    pub context: serde_json::Value,
    #[serde(default)]
    pub behavior: serde_json::Value,
}

impl ProfileSnapshotSummary {
    pub fn sanitized(mut self) -> Self {
        self.context = sanitize_json(self.context);
        self.behavior = sanitize_json(self.behavior);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileSnapshotDraft {
    pub source: ProfileSourceAudit,
    pub canonical_spec: serde_json::Value,
    pub summary: ProfileSnapshotSummary,
    #[serde(default)]
    pub captured_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileSnapshot {
    pub hash: String,
    pub source: ProfileSourceAudit,
    pub canonical_spec: serde_json::Value,
    pub summary: ProfileSnapshotSummary,
    pub captured_at: String,
}

#[derive(Serialize)]
struct ProfileSnapshotHashPayload<'a> {
    schema_version: u32,
    source: &'a ProfileSourceAudit,
    canonical_spec: &'a serde_json::Value,
}

impl ProfileSnapshot {
    pub fn from_draft(draft: ProfileSnapshotDraft) -> Result<Self> {
        let canonical_spec = sanitize_json(draft.canonical_spec);
        let summary = draft.summary.sanitized();
        let source = draft.source;
        let payload = ProfileSnapshotHashPayload {
            schema_version: TURN_MANIFEST_SCHEMA_VERSION,
            source: &source,
            canonical_spec: &canonical_spec,
        };
        let json = compact_json("profile_snapshot_payload", &payload)?;
        let hash = object_id("profile_snapshot", json.as_bytes());
        Ok(Self {
            hash,
            source,
            canonical_spec,
            summary,
            captured_at: draft
                .captured_at
                .unwrap_or_else(|| chrono::Local::now().to_rfc3339()),
        })
    }

    pub fn drift_for(&self, current_source_fingerprint: Option<String>) -> Option<ProfileDrift> {
        let locked = self.source.source_fingerprint.clone();
        if locked == current_source_fingerprint {
            return None;
        }
        Some(ProfileDrift::new(
            self.hash.clone(),
            locked,
            current_source_fingerprint,
        ))
    }

    pub fn manifest_metadata(&self) -> serde_json::Value {
        sanitize_json(serde_json::json!({
            "hash": self.hash,
            "source_label": self.source.source_label,
            "source_fingerprint": self.source.source_fingerprint,
            "profile_name": self.summary.profile_name,
            "model": self.summary.model,
            "tool_bundles": self.summary.tool_bundles,
            "enabled_tools": self.summary.enabled_tools,
            "security_mode": self.summary.security_mode,
            "context": self.summary.context,
            "behavior": self.summary.behavior,
        }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileDrift {
    pub locked_hash: String,
    #[serde(default)]
    pub locked_source_fingerprint: Option<String>,
    #[serde(default)]
    pub current_source_fingerprint: Option<String>,
    pub warning: String,
}

impl ProfileDrift {
    pub fn new(
        locked_hash: String,
        locked_source_fingerprint: Option<String>,
        current_source_fingerprint: Option<String>,
    ) -> Self {
        let warning = format!(
            "profile changed on disk; current session remains locked to snapshot {locked_hash}"
        );
        Self {
            locked_hash,
            locked_source_fingerprint,
            current_source_fingerprint,
            warning,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenTurnRequest {
    #[serde(default)]
    pub session_id: Option<String>,
    pub profile_snapshot: ProfileSnapshot,
    #[serde(default)]
    pub current_source_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenedTurn {
    pub turn_id: String,
    pub session_id: String,
    #[serde(default)]
    pub base_commit_id: Option<String>,
    pub turn_index: usize,
    pub profile_snapshot: ProfileSnapshot,
    #[serde(default)]
    pub profile_drift: Option<ProfileDrift>,
    #[serde(default)]
    pub history: Vec<SessionMessage>,
    #[serde(default)]
    pub parent_manifest_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitTurnRequest {
    pub session_id: String,
    pub turn_id: String,
    #[serde(default)]
    pub base_commit_id: Option<String>,
    pub profile_hash: String,
    pub status: TurnOutcomeStatus,
    pub model: String,
    pub provider: String,
    #[serde(default)]
    pub messages: Vec<SessionMessage>,
    #[serde(default)]
    pub manifests: Vec<NewTurnManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommittedTurn {
    pub session_id: String,
    pub turn_id: String,
    pub commit_id: String,
    #[serde(default)]
    pub turn_manifest_ids: Vec<String>,
}

pub(crate) fn add_turn_metadata(
    manifest: &mut NewTurnManifest,
    status: &TurnOutcomeStatus,
    snapshot: &ProfileSnapshot,
) {
    let mut metadata = match sanitize_json(std::mem::take(&mut manifest.metadata)) {
        serde_json::Value::Object(map) => map,
        serde_json::Value::Null => serde_json::Map::new(),
        value => {
            let mut map = serde_json::Map::new();
            map.insert("data".to_string(), value);
            map
        }
    };
    metadata.insert(
        "turn_status".to_string(),
        serde_json::Value::String(status.as_str().to_string()),
    );
    metadata.insert("profile_snapshot".to_string(), snapshot.manifest_metadata());
    manifest.metadata = serde_json::Value::Object(metadata);
}

pub(crate) fn sanitize_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut sanitized = serde_json::Map::new();
            for (key, value) in map {
                if is_secret_key(&key) {
                    continue;
                }
                sanitized.insert(key, sanitize_json(value));
            }
            serde_json::Value::Object(sanitized)
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(sanitize_json).collect())
        }
        other => other,
    }
}

#[cfg(test)]
pub(crate) fn contains_secret_key(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => map
            .iter()
            .any(|(key, value)| is_secret_key(key) || contains_secret_key(value)),
        serde_json::Value::Array(values) => values.iter().any(contains_secret_key),
        _ => false,
    }
}

fn is_secret_key(key: &str) -> bool {
    SECRET_FIELD_NAMES.contains(&key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(captured_at: &str) -> ProfileSnapshotDraft {
        ProfileSnapshotDraft {
            source: ProfileSourceAudit::new("agent.toml", Some("fingerprint-a")),
            canonical_spec: serde_json::json!({
                "profile_name": "default",
                "model": "sonnet",
                "tools": {
                    "bundles": ["filesystem"],
                    "enabled": ["shell"]
                },
                "security": {"sandbox_mode": "workspace-write"},
                "context": {"project_instructions": true},
                "behavior": {"reasoning_effort": "medium"}
            }),
            summary: ProfileSnapshotSummary {
                profile_name: "default".to_string(),
                source_label: "agent.toml".to_string(),
                model: "sonnet".to_string(),
                tool_bundles: vec!["filesystem".to_string()],
                enabled_tools: vec!["shell".to_string()],
                security_mode: Some("workspace-write".to_string()),
                context: serde_json::json!({"project_instructions": true}),
                behavior: serde_json::json!({"reasoning_effort": "medium"}),
            },
            captured_at: Some(captured_at.to_string()),
        }
    }

    #[test]
    fn profile_snapshot_hash_ignores_capture_timestamp() -> Result<()> {
        let first = ProfileSnapshot::from_draft(draft("2026-05-14T00:00:00Z"))?;
        let second = ProfileSnapshot::from_draft(draft("2026-05-15T00:00:00Z"))?;
        assert_eq!(first.hash, second.hash);
        assert_ne!(first.captured_at, second.captured_at);
        Ok(())
    }

    #[test]
    fn profile_snapshot_hash_changes_when_execution_shape_changes() -> Result<()> {
        let first = ProfileSnapshot::from_draft(draft("2026-05-14T00:00:00Z"))?;
        let mut changed = draft("2026-05-14T00:00:00Z");
        changed.canonical_spec["model"] = serde_json::Value::String("opus".to_string());
        changed.summary.model = "opus".to_string();
        let second = ProfileSnapshot::from_draft(changed)?;
        assert_ne!(first.hash, second.hash);

        let mut source_changed = draft("2026-05-14T00:00:00Z");
        source_changed.source.source_fingerprint = Some("fingerprint-b".to_string());
        let third = ProfileSnapshot::from_draft(source_changed)?;
        assert_ne!(first.hash, third.hash);
        Ok(())
    }

    #[test]
    fn profile_snapshot_drift_keeps_locked_hash() -> Result<()> {
        let snapshot = ProfileSnapshot::from_draft(draft("2026-05-14T00:00:00Z"))?;
        let drift = snapshot
            .drift_for(Some("fingerprint-b".to_string()))
            .expect("changed source should report drift");
        assert_eq!(drift.locked_hash, snapshot.hash);
        assert_eq!(
            drift.warning,
            format!(
                "profile changed on disk; current session remains locked to snapshot {}",
                snapshot.hash
            )
        );
        Ok(())
    }

    #[test]
    fn profile_snapshot_summary_excludes_credentials() -> Result<()> {
        let mut draft = draft("2026-05-14T00:00:00Z");
        draft.summary.context = serde_json::json!({
            "project_instructions": true,
            "credentials": {"api_key": "secret"},
            "env": {"OPENAI_API_KEY": "secret"}
        });
        draft.summary.behavior = serde_json::json!({
            "reasoning_effort": "medium",
            "authorization": "Bearer secret"
        });
        let snapshot = ProfileSnapshot::from_draft(draft)?;
        let summary = serde_json::to_value(&snapshot.summary).expect("summary serializes");
        assert!(!contains_secret_key(&summary));
        Ok(())
    }

    #[test]
    fn profile_snapshot_source_audit_excludes_secret_fields() -> Result<()> {
        let snapshot = ProfileSnapshot::from_draft(draft("2026-05-14T00:00:00Z"))?;
        let source = serde_json::to_value(&snapshot.source).expect("source serializes");
        assert!(!contains_secret_key(&source));
        Ok(())
    }
}
