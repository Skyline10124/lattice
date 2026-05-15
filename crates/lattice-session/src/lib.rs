//! Session graph and git-style conversation refs for LATTICE.
//!
//! This crate owns the durable conversation primitives: session records,
//! branch refs, HEAD checkout state, lineage, and file-backed persistence.
//! It intentionally does not choose an application default directory.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

mod turn_tx;
pub use turn_tx::{
    CommitTurnRequest, CommittedTurn, OpenTurnRequest, OpenedTurn, ProfileDrift, ProfileSnapshot,
    ProfileSnapshotDraft, ProfileSnapshotSummary, ProfileSourceAudit, TurnOutcomeStatus,
};

const MAX_SESSION_ID_LEN: usize = 128;
const MAX_BRANCH_NAME_LEN: usize = 128;
const DEFAULT_BRANCH: &str = "main";
const TURN_MANIFEST_SCHEMA_VERSION: u32 = 1;

pub type Result<T> = std::result::Result<T, SessionError>;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("io error while {operation} '{path}': {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("json error in '{path}': {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("database error in '{path}': {source}")]
    Database {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("failed to serialize session '{id}': {source}")]
    Serialize {
        id: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to serialize {kind} object: {source}")]
    SerializeObject {
        kind: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid session id: format or length violation")]
    InvalidSessionId,
    #[error("invalid branch name: format or length violation")]
    InvalidBranchName,
    #[error("session '{0}' not found")]
    NotFound(String),
    #[error("session prefix '{0}' is ambiguous")]
    AmbiguousPrefix(String),
    #[error("branch '{0}' already exists")]
    BranchExists(String),
    #[error("tag '{0}' already exists")]
    TagExists(String),
    #[error("stash '{0}' not found")]
    StashNotFound(String),
    #[error("branch '{branch}' points to missing session '{session_id}'")]
    MissingBranchTarget { branch: String, session_id: String },
    #[error("no session found")]
    NoSession,
    #[error("path error: {0}")]
    Path(String),
    #[error("transaction conflict: {0}")]
    Conflict(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub model: String,
    pub provider: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    pub messages: Vec<SessionMessage>,
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMessage {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub reasoning_content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<SessionToolCall>>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub artifact_ids: Vec<String>,
    #[serde(default)]
    pub compact: Option<CompactBoundary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactBoundary {
    pub mode: String,
    pub parent_session_id: Option<String>,
    pub original_message_count: usize,
    pub summarized_message_count: usize,
    pub retained_message_count: usize,
    pub active_tokens_before: u32,
    pub active_tokens_after: u32,
    pub compacted_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl Session {
    pub fn new(model: String, provider: String) -> Self {
        let timestamp = now_rfc3339();
        Self {
            id: Uuid::new_v4().simple().to_string(),
            model,
            provider,
            title: None,
            parent_id: None,
            branch: None,
            messages: vec![],
            created_at: timestamp.clone(),
            updated_at: timestamp,
        }
    }

    pub fn fork_from(parent: &Session, branch: Option<String>) -> Self {
        let mut fork = Self::new(parent.model.clone(), parent.provider.clone());
        fork.parent_id = Some(parent.id.clone());
        fork.branch = branch;
        fork.title = parent.title.clone();
        fork.messages = parent.messages.clone();
        fork
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
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            artifact_ids: Vec::new(),
            compact: None,
        });
    }

    pub fn compact_with_summary(
        &mut self,
        summary: String,
        retained_suffix: Vec<SessionMessage>,
        boundary: CompactBoundary,
    ) {
        let parent_session_id = boundary
            .parent_session_id
            .clone()
            .or_else(|| Some(self.id.clone()));
        let mut boundary = boundary;
        boundary.parent_session_id = parent_session_id;
        self.messages = vec![SessionMessage {
            role: "user".to_string(),
            content: summary,
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            artifact_ids: Vec::new(),
            compact: Some(boundary),
        }];
        self.messages.extend(retained_suffix);
        self.touch();
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
        if self.branch.as_deref().is_some_and(str::is_empty) {
            self.branch = None;
        }
        if self.parent_id.as_deref().is_some_and(str::is_empty) {
            self.parent_id = None;
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
    pub parent_id: Option<String>,
    pub branch: Option<String>,
    pub branches: Vec<String>,
    pub is_current: bool,
    pub created_at: String,
    pub updated_at: String,
    pub message_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchSummary {
    pub name: String,
    pub session_id: String,
    pub title: Option<String>,
    pub updated_at: Option<String>,
    pub is_current: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTurnSummary {
    pub turn: usize,
    pub user: String,
    pub assistant: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCommit {
    pub id: String,
    pub session_id: String,
    pub parents: Vec<String>,
    #[serde(default)]
    pub parent_session_ids: Vec<String>,
    pub message_ids: Vec<String>,
    #[serde(default)]
    pub turn_manifest_ids: Vec<String>,
    pub model: String,
    pub provider: String,
    pub title: Option<String>,
    pub branch: Option<String>,
    pub created_at: String,
    pub committed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionCommitPayload {
    session_id: String,
    parents: Vec<String>,
    parent_session_ids: Vec<String>,
    message_ids: Vec<String>,
    #[serde(default)]
    turn_manifest_ids: Vec<String>,
    model: String,
    provider: String,
    title: Option<String>,
    branch: Option<String>,
    created_at: String,
    committed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionMessageObject {
    id: String,
    message: SessionMessage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnManifestObject {
    pub id: String,
    pub schema_version: u32,
    pub session_id: String,
    #[serde(default)]
    pub commit_id: Option<String>,
    pub turn_index: usize,
    pub request_id: String,
    #[serde(default)]
    pub parent_manifest_ids: Vec<String>,
    pub model: String,
    pub provider: String,
    pub created_at: String,
    pub prompt: PromptSnapshot,
    #[serde(default)]
    pub hooks: Vec<HookDecisionRecord>,
    #[serde(default)]
    pub tools: Vec<ToolDecisionRecord>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredTurnManifestObject {
    pub id: String,
    pub schema_version: u32,
    pub session_id: String,
    pub turn_index: usize,
    pub request_id: String,
    #[serde(default)]
    pub parent_manifest_ids: Vec<String>,
    pub model: String,
    pub provider: String,
    pub prompt: PromptSnapshot,
    #[serde(default)]
    pub hooks: Vec<HookDecisionRecord>,
    #[serde(default)]
    pub tools: Vec<ToolDecisionRecord>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewTurnManifest {
    pub session_id: String,
    #[serde(default)]
    pub commit_id: Option<String>,
    pub turn_index: usize,
    pub request_id: String,
    #[serde(default)]
    pub parent_manifest_ids: Vec<String>,
    pub model: String,
    pub provider: String,
    pub prompt: PromptSnapshot,
    #[serde(default)]
    pub hooks: Vec<HookDecisionRecord>,
    #[serde(default)]
    pub tools: Vec<ToolDecisionRecord>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptSnapshot {
    pub total_tokens: u32,
    pub effective_context_budget: u32,
    #[serde(default)]
    pub rendered_fragments: Vec<PromptFragmentRecord>,
    #[serde(default)]
    pub withheld_fragments: Vec<PromptFragmentRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptFragmentRecord {
    pub id: String,
    pub layer: String,
    pub role: String,
    pub authority: String,
    pub source: String,
    pub cache_scope: String,
    pub priority: u8,
    pub tokens: u32,
    pub content_hash: String,
    pub content_preview: String,
    pub rendered: bool,
    #[serde(default)]
    pub withheld_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookDecisionRecord {
    pub point: String,
    pub hook_name: String,
    pub order: usize,
    pub decision_kind: String,
    #[serde(default)]
    pub target_fragment_id: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub before_hash: Option<String>,
    #[serde(default)]
    pub after_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDecisionRecord {
    pub tool_name: String,
    pub decision_kind: String,
    #[serde(default)]
    pub command_hash: Option<String>,
    #[serde(default)]
    pub command_preview: Option<String>,
    #[serde(default)]
    pub result_hash: Option<String>,
    #[serde(default)]
    pub result_preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnManifestSummary {
    pub id: String,
    pub session_id: String,
    pub turn_index: usize,
    pub request_id: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflogEntry {
    pub timestamp: String,
    pub ref_name: String,
    pub old_session_id: Option<String>,
    pub new_session_id: Option<String>,
    pub old_commit_id: Option<String>,
    pub new_commit_id: Option<String>,
    pub action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagSummary {
    pub name: String,
    pub session_id: String,
    pub commit_id: Option<String>,
    pub title: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDiff {
    pub from: String,
    pub to: String,
    pub from_message_count: usize,
    pub to_message_count: usize,
    pub common_prefix_messages: usize,
    pub removed_messages: Vec<SessionMessage>,
    pub added_messages: Vec<SessionMessage>,
    pub changed_messages: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeResult {
    pub session: Session,
    pub base_session_id: Option<String>,
    pub appended_messages: usize,
    pub conflicts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StashEntry {
    pub id: String,
    pub session_id: String,
    pub commit_id: Option<String>,
    pub message: Option<String>,
    pub title: Option<String>,
    pub created_at: String,
    pub message_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcReport {
    pub reachable_sessions: usize,
    pub unreachable_sessions: usize,
    pub reachable_commits: usize,
    #[serde(default)]
    pub reachable_messages: usize,
    #[serde(default)]
    pub reachable_artifacts: usize,
    pub pruned_objects: usize,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationReport {
    pub scanned_sessions: usize,
    pub migrated_sessions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArtifactKind {
    FileSnapshot,
    FilePatch,
    CommandOutput,
    Image,
    Report,
    EvalResult,
}

impl ArtifactKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ArtifactKind::FileSnapshot => "file_snapshot",
            ArtifactKind::FilePatch => "file_patch",
            ArtifactKind::CommandOutput => "command_output",
            ArtifactKind::Image => "image",
            ArtifactKind::Report => "report",
            ArtifactKind::EvalResult => "eval_result",
        }
    }

    fn from_str(value: &str) -> Self {
        match value {
            "file_snapshot" => ArtifactKind::FileSnapshot,
            "file_patch" => ArtifactKind::FilePatch,
            "command_output" => ArtifactKind::CommandOutput,
            "image" => ArtifactKind::Image,
            "report" => ArtifactKind::Report,
            "eval_result" => ArtifactKind::EvalResult,
            _ => ArtifactKind::Report,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewArtifact {
    pub kind: ArtifactKind,
    pub content: String,
    pub path: Option<String>,
    pub mime: Option<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactObject {
    pub id: String,
    pub kind: ArtifactKind,
    pub content_hash: String,
    pub content: String,
    pub path: Option<String>,
    pub mime: Option<String>,
    pub size: usize,
    pub created_at: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactSummary {
    pub id: String,
    pub kind: ArtifactKind,
    pub path: Option<String>,
    pub mime: Option<String>,
    pub size: usize,
    pub created_at: String,
}

pub trait SessionStore: Send + Sync {
    fn save(&self, session: &Session) -> Result<()>;
    fn load(&self, id: &str) -> Result<Option<Session>>;
    fn list(&self) -> Result<Vec<SessionSummary>>;
    fn delete(&self, id: &str) -> Result<bool>;
    fn latest(&self) -> Result<Option<Session>>;
    fn current(&self) -> Result<Option<Session>>;
    fn current_session_id(&self) -> Result<Option<String>>;
    fn checkout(&self, target: &str) -> Result<Session>;
    fn create_branch(&self, name: &str, from: Option<&str>) -> Result<Session>;
    fn branches(&self) -> Result<Vec<BranchSummary>>;
    fn log(&self, id: Option<&str>) -> Result<Vec<SessionTurnSummary>>;
    fn resolve_session_id(&self, target: &str) -> Result<String>;
    fn reflog(&self, target: Option<&str>) -> Result<Vec<ReflogEntry>>;
    fn reset(&self, target: &str) -> Result<Session>;
    fn create_tag(&self, name: &str, target: Option<&str>) -> Result<TagSummary>;
    fn tags(&self) -> Result<Vec<TagSummary>>;
    fn diff(&self, from: &str, to: &str) -> Result<SessionDiff>;
    fn cherry_pick(&self, source: &str, turns: &[usize]) -> Result<Session>;
    fn merge(&self, target: &str) -> Result<MergeResult>;
    fn rebase_onto(&self, target: &str) -> Result<Session>;
    fn stash_push(&self, message: Option<&str>) -> Result<StashEntry>;
    fn stash_list(&self) -> Result<Vec<StashEntry>>;
    fn stash_apply(&self, id: &str) -> Result<Session>;
    fn gc(&self, prune: bool) -> Result<GcReport>;
    fn migrate_legacy_sessions(&self) -> Result<MigrationReport>;
    fn put_artifact(&self, artifact: NewArtifact) -> Result<ArtifactObject>;
    fn get_artifact(&self, id: &str) -> Result<Option<ArtifactObject>>;
    fn list_artifacts(&self) -> Result<Vec<ArtifactSummary>>;
    /// Save a session with associated turn manifests.
    /// The manifest IDs are stored in the commit record.
    /// Default: not implemented. Stores that support manifests override this.
    fn save_with_manifests(
        &self,
        _session: &Session,
        _manifests: Vec<NewTurnManifest>,
    ) -> Result<()> {
        Err(SessionError::Path(
            "save_with_manifests is not implemented for this store".into(),
        ))
    }
    fn put_turn_manifest(&self, _manifest: NewTurnManifest) -> Result<TurnManifestObject> {
        Err(SessionError::Path(
            "turn manifest storage is not implemented for this store".into(),
        ))
    }
    fn get_turn_manifest(&self, _id: &str) -> Result<Option<TurnManifestObject>> {
        Ok(None)
    }
    fn list_turn_manifests(&self, _session_id: &str) -> Result<Vec<TurnManifestSummary>> {
        Ok(Vec::new())
    }
    fn latest_turn_manifest_ids(&self, _session_id: &str) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    fn open_turn(&self, _request: OpenTurnRequest) -> Result<OpenedTurn> {
        Err(SessionError::Path(
            "open_turn is not implemented for this store".into(),
        ))
    }
    fn commit_turn(&self, _request: CommitTurnRequest) -> Result<CommittedTurn> {
        Err(SessionError::Path(
            "commit_turn is not implemented for this store".into(),
        ))
    }
}

#[derive(Debug, Clone)]
pub struct FileSessionStore {
    dir: PathBuf,
}

impl FileSessionStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn write_session(&self, session: &Session) -> Result<()> {
        validate_session_id(&session.id)?;
        create_dir_all(&self.dir)?;
        let path = self.session_path(&session.id)?;
        let content =
            serde_json::to_string_pretty(session).map_err(|source| SessionError::Serialize {
                id: session.id.clone(),
                source,
            })?;
        let tmp_path = self
            .dir
            .join(format!("{}.json.tmp-{}", session.id, std::process::id()));
        write_file(&tmp_path, content)?;
        rename(&tmp_path, &path)?;
        Ok(())
    }

    fn session_path(&self, id: &str) -> Result<PathBuf> {
        validate_session_id(id)?;
        Ok(self.dir.join(format!("{id}.json")))
    }

    fn head_path(&self) -> PathBuf {
        self.dir.join("HEAD")
    }

    fn refs_dir(&self) -> PathBuf {
        self.dir.join("refs").join("heads")
    }

    fn tags_dir(&self) -> PathBuf {
        self.dir.join("refs").join("tags")
    }

    fn logs_dir(&self) -> PathBuf {
        self.dir.join("logs")
    }

    fn objects_dir(&self) -> PathBuf {
        self.dir.join("objects")
    }

    fn messages_dir(&self) -> PathBuf {
        self.objects_dir().join("messages")
    }

    fn commits_dir(&self) -> PathBuf {
        self.objects_dir().join("commits")
    }

    fn stash_dir(&self) -> PathBuf {
        self.dir.join("stash")
    }

    fn artifacts_dir(&self) -> PathBuf {
        self.objects_dir().join("artifacts")
    }

    fn branch_path(&self, name: &str) -> Result<PathBuf> {
        validate_branch_name(name)?;
        Ok(self.refs_dir().join(name))
    }

    fn tag_path(&self, name: &str) -> Result<PathBuf> {
        validate_branch_name(name)?;
        Ok(self.tags_dir().join(name))
    }

    fn stash_path(&self, id: &str) -> Result<PathBuf> {
        validate_session_id(id)?;
        Ok(self.stash_dir().join(format!("{id}.json")))
    }

    fn read_head(&self) -> Result<Option<String>> {
        let path = self.head_path();
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(read_to_string(&path)?.trim().to_string()))
    }

    fn write_head_ref(&self, branch: &str) -> Result<()> {
        validate_branch_name(branch)?;
        create_dir_all(&self.dir)?;
        write_file(self.head_path(), format!("ref: refs/heads/{branch}\n"))?;
        Ok(())
    }

    fn write_head_detached(&self, session_id: &str) -> Result<()> {
        validate_session_id(session_id)?;
        create_dir_all(&self.dir)?;
        write_file(self.head_path(), format!("{session_id}\n"))?;
        Ok(())
    }

    fn clear_head(&self) -> Result<()> {
        let path = self.head_path();
        if path.exists() {
            remove_file(&path)?;
        }
        Ok(())
    }

    fn current_branch_name(&self) -> Result<Option<String>> {
        let Some(head) = self.read_head()? else {
            return Ok(None);
        };
        Ok(head
            .strip_prefix("ref: refs/heads/")
            .map(|branch| branch.trim().to_string()))
    }

    fn read_branch_ref(&self, branch: &str) -> Result<Option<String>> {
        let path = self.branch_path(branch)?;
        if !path.exists() {
            return Ok(None);
        }
        let id = read_to_string(&path)?.trim().to_string();
        if id.is_empty() {
            return Ok(None);
        }
        validate_session_id(&id)?;
        Ok(Some(id))
    }

    fn write_branch_ref(&self, branch: &str, session_id: &str) -> Result<()> {
        validate_session_id(session_id)?;
        let path = self.branch_path(branch)?;
        if let Some(parent) = path.parent() {
            create_dir_all(parent)?;
        }
        write_file(path, format!("{session_id}\n"))?;
        Ok(())
    }

    fn read_tag_ref(&self, tag: &str) -> Result<Option<String>> {
        let path = self.tag_path(tag)?;
        if !path.exists() {
            return Ok(None);
        }
        let id = read_to_string(&path)?.trim().to_string();
        if id.is_empty() {
            return Ok(None);
        }
        validate_session_id(&id)?;
        Ok(Some(id))
    }

    fn write_tag_ref(&self, tag: &str, session_id: &str) -> Result<()> {
        validate_session_id(session_id)?;
        let path = self.tag_path(tag)?;
        if let Some(parent) = path.parent() {
            create_dir_all(parent)?;
        }
        write_file(path, format!("{session_id}\n"))?;
        Ok(())
    }

    fn branch_refs(&self) -> Result<Vec<(String, String)>> {
        let refs_dir = self.refs_dir();
        if !refs_dir.exists() {
            return Ok(vec![]);
        }
        let mut refs = Vec::new();
        collect_branch_refs(&refs_dir, &refs_dir, &mut refs)?;
        refs.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(refs)
    }

    fn tag_refs(&self) -> Result<Vec<(String, String)>> {
        let tags_dir = self.tags_dir();
        if !tags_dir.exists() {
            return Ok(vec![]);
        }
        let mut refs = Vec::new();
        collect_branch_refs(&tags_dir, &tags_dir, &mut refs)?;
        refs.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(refs)
    }

    fn advance_head(&self, session_id: &str, action: &str) -> Result<()> {
        validate_session_id(session_id)?;
        match self.current_branch_name()? {
            Some(branch) => {
                let old_session_id = self.read_branch_ref(&branch)?;
                self.write_branch_ref(&branch, session_id)?;
                self.append_reflog(
                    &format!("refs/heads/{branch}"),
                    old_session_id.as_deref(),
                    Some(session_id),
                    action,
                )?;
                self.append_reflog("HEAD", old_session_id.as_deref(), Some(session_id), action)
            }
            None => {
                if self.read_head()?.is_some() {
                    let old_session_id = self.current_session_id()?;
                    self.write_head_detached(session_id)?;
                    self.append_reflog("HEAD", old_session_id.as_deref(), Some(session_id), action)
                } else {
                    self.write_branch_ref(DEFAULT_BRANCH, session_id)?;
                    self.write_head_ref(DEFAULT_BRANCH)?;
                    self.append_reflog(
                        &format!("refs/heads/{DEFAULT_BRANCH}"),
                        None,
                        Some(session_id),
                        action,
                    )?;
                    self.append_reflog("HEAD", None, Some(session_id), action)
                }
            }
        }
    }

    fn record_commit(&self, session: &Session) -> Result<SessionCommit> {
        create_dir_all(self.messages_dir())?;
        create_dir_all(self.commits_dir())?;
        let message_ids = session
            .messages
            .iter()
            .map(|message| self.write_message_object(message))
            .collect::<Result<Vec<_>>>()?;
        let parents = session
            .parent_id
            .as_deref()
            .and_then(|id| self.commit_id_for_session(id).ok().flatten())
            .into_iter()
            .collect::<Vec<_>>();
        let parent_session_ids = session.parent_id.clone().into_iter().collect::<Vec<_>>();
        let payload = SessionCommitPayload {
            session_id: session.id.clone(),
            parents,
            parent_session_ids,
            message_ids,
            turn_manifest_ids: Vec::new(),
            model: session.model.clone(),
            provider: session.provider.clone(),
            title: session.title.clone(),
            branch: session.branch.clone(),
            created_at: session.created_at.clone(),
            committed_at: now_rfc3339(),
        };
        let content = stable_json("commit", &payload)?;
        let id = object_id("commit", content.as_bytes());
        let commit = SessionCommit {
            id: id.clone(),
            session_id: payload.session_id,
            parents: payload.parents,
            parent_session_ids: payload.parent_session_ids,
            message_ids: payload.message_ids,
            turn_manifest_ids: payload.turn_manifest_ids,
            model: payload.model,
            provider: payload.provider,
            title: payload.title,
            branch: payload.branch,
            created_at: payload.created_at,
            committed_at: payload.committed_at,
        };
        let path = self.commits_dir().join(format!("{id}.json"));
        if !path.exists() {
            let commit_content = stable_json("commit", &commit)?;
            write_file(path, commit_content)?;
        }
        Ok(commit)
    }

    fn write_message_object(&self, message: &SessionMessage) -> Result<String> {
        let message_content = stable_json("message", message)?;
        let id = object_id("message", message_content.as_bytes());
        let path = self.messages_dir().join(format!("{id}.json"));
        if !path.exists() {
            let object = SessionMessageObject {
                id: id.clone(),
                message: message.clone(),
            };
            write_file(path, stable_json("message", &object)?)?;
        }
        Ok(id)
    }

    fn read_message_object(&self, id: &str) -> Result<Option<SessionMessage>> {
        validate_session_id(id)?;
        let path = self.messages_dir().join(format!("{id}.json"));
        if !path.exists() {
            return Ok(None);
        }
        let content = read_to_string(&path)?;
        let object: SessionMessageObject =
            serde_json::from_str(&content).map_err(|source| SessionError::Json { path, source })?;
        Ok(Some(object.message))
    }

    fn commit_id_for_session(&self, session_id: &str) -> Result<Option<String>> {
        validate_session_id(session_id)?;
        if !self.commits_dir().exists() {
            return Ok(None);
        }
        let mut latest: Option<SessionCommit> = None;
        for entry in read_dir(self.commits_dir())? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let content = read_to_string(&path)?;
            let commit: SessionCommit =
                serde_json::from_str(&content).map_err(|source| SessionError::Json {
                    path: path.clone(),
                    source,
                })?;
            if commit.session_id == session_id
                && latest
                    .as_ref()
                    .is_none_or(|current| commit.committed_at > current.committed_at)
            {
                latest = Some(commit);
            }
        }
        Ok(latest.map(|commit| commit.id))
    }

    fn session_from_commit(&self, commit: &SessionCommit) -> Result<Session> {
        let messages = commit
            .message_ids
            .iter()
            .map(|id| {
                self.read_message_object(id)?
                    .ok_or_else(|| SessionError::NotFound(id.clone()))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut session = Session {
            id: commit.session_id.clone(),
            model: commit.model.clone(),
            provider: commit.provider.clone(),
            title: commit.title.clone(),
            parent_id: commit.parent_session_ids.first().cloned(),
            branch: commit.branch.clone(),
            messages,
            created_at: commit.created_at.clone(),
            updated_at: commit.committed_at.clone(),
        };
        session.normalize();
        Ok(session)
    }

    fn read_commit(&self, id: &str) -> Result<Option<SessionCommit>> {
        validate_session_id(id)?;
        let path = self.commits_dir().join(format!("{id}.json"));
        if !path.exists() {
            return Ok(None);
        }
        let content = read_to_string(&path)?;
        serde_json::from_str(&content)
            .map(Some)
            .map_err(|source| SessionError::Json { path, source })
    }

    fn artifact_path(&self, id: &str) -> Result<PathBuf> {
        validate_session_id(id)?;
        Ok(self.artifacts_dir().join(format!("{id}.json")))
    }

    fn reachable_object_ids(
        &self,
    ) -> Result<(BTreeSet<String>, BTreeSet<String>, BTreeSet<String>)> {
        let reachable_sessions = self.reachable_session_ids()?;
        let mut commits = BTreeSet::new();
        let mut messages = BTreeSet::new();
        let mut artifacts = BTreeSet::new();
        for session_id in reachable_sessions {
            if let Some(commit_id) = self.commit_id_for_session(&session_id)? {
                if let Some(commit) = self.read_commit(&commit_id)? {
                    commits.insert(commit_id);
                    for message_id in commit.message_ids {
                        if messages.insert(message_id.clone()) {
                            if let Some(message) = self.read_message_object(&message_id)? {
                                artifacts.extend(message.artifact_ids);
                            }
                        }
                    }
                }
            }
        }
        Ok((commits, messages, artifacts))
    }

    fn append_reflog(
        &self,
        ref_name: &str,
        old_session_id: Option<&str>,
        new_session_id: Option<&str>,
        action: &str,
    ) -> Result<()> {
        let old_commit_id = old_session_id
            .map(|id| self.commit_id_for_session(id))
            .transpose()?
            .flatten();
        let new_commit_id = new_session_id
            .map(|id| self.commit_id_for_session(id))
            .transpose()?
            .flatten();
        let entry = ReflogEntry {
            timestamp: now_rfc3339(),
            ref_name: ref_name.to_string(),
            old_session_id: old_session_id.map(str::to_string),
            new_session_id: new_session_id.map(str::to_string),
            old_commit_id,
            new_commit_id,
            action: action.to_string(),
        };
        let path = self.log_path(ref_name)?;
        if let Some(parent) = path.parent() {
            create_dir_all(parent)?;
        }
        let line = compact_json("reflog", &entry)?;
        append_line(path, &line)?;
        Ok(())
    }

    fn log_path(&self, ref_name: &str) -> Result<PathBuf> {
        if ref_name == "HEAD" {
            return Ok(self.logs_dir().join("HEAD"));
        }
        if let Some(name) = ref_name.strip_prefix("refs/heads/") {
            validate_branch_name(name)?;
            return Ok(self.logs_dir().join("refs").join("heads").join(name));
        }
        if let Some(name) = ref_name.strip_prefix("refs/tags/") {
            validate_branch_name(name)?;
            return Ok(self.logs_dir().join("refs").join("tags").join(name));
        }
        validate_branch_name(ref_name)?;
        Ok(self.logs_dir().join("refs").join("heads").join(ref_name))
    }

    fn target_session_id(&self, target: &str) -> Result<String> {
        if let Some(id) = self.read_branch_ref(target)? {
            return Ok(id);
        }
        if let Some(id) = self.read_tag_ref(target)? {
            return Ok(id);
        }
        if let Some(commit) = self.read_commit(target)? {
            return Ok(commit.session_id);
        }
        self.resolve_session_id(target)
    }

    fn reachable_session_ids(&self) -> Result<BTreeSet<String>> {
        let mut reachable = BTreeSet::new();
        if let Some(id) = self.current_session_id()? {
            reachable.insert(id);
        }
        for (_, id) in self.branch_refs()? {
            reachable.insert(id);
        }
        for (_, id) in self.tag_refs()? {
            reachable.insert(id);
        }
        for entry in self.stash_list()? {
            reachable.insert(entry.session_id);
        }
        Ok(reachable)
    }

    fn prune_refs_to(&self, session_id: &str) -> Result<()> {
        for (branch, id) in self.branch_refs()? {
            if id == session_id {
                let path = self.branch_path(&branch)?;
                if path.exists() {
                    remove_file(path)?;
                }
            }
        }
        for (tag, id) in self.tag_refs()? {
            if id == session_id {
                let path = self.tag_path(&tag)?;
                if path.exists() {
                    remove_file(path)?;
                }
            }
        }
        Ok(())
    }
}

impl SessionStore for FileSessionStore {
    fn save(&self, session: &Session) -> Result<()> {
        self.write_session(session)?;
        self.record_commit(session)?;
        self.advance_head(&session.id, "commit")?;
        Ok(())
    }

    fn load(&self, id: &str) -> Result<Option<Session>> {
        let path = self.session_path(id)?;
        if !path.exists() {
            return Ok(None);
        }
        let content = read_to_string(&path)?;
        let mut session: Session =
            serde_json::from_str(&content).map_err(|source| SessionError::Json { path, source })?;
        session.normalize();
        Ok(Some(session))
    }

    fn list(&self) -> Result<Vec<SessionSummary>> {
        if !self.dir.exists() {
            return Ok(vec![]);
        }
        let current_id = self.current_session_id().ok().flatten();
        let refs = self.branch_refs()?.into_iter().collect::<BTreeMap<_, _>>();
        let entries = read_dir(&self.dir)?;
        let mut summaries = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(mut session) = serde_json::from_str::<Session>(&content) else {
                continue;
            };
            session.normalize();
            let branches = refs
                .iter()
                .filter_map(|(branch, id)| {
                    if id == &session.id {
                        Some(branch.clone())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            summaries.push(SessionSummary {
                is_current: current_id.as_deref() == Some(session.id.as_str()),
                id: session.id.clone(),
                model: session.model.clone(),
                provider: session.provider.clone(),
                title: session.title.clone(),
                parent_id: session.parent_id.clone(),
                branch: session.branch.clone(),
                branches,
                created_at: session.created_at.clone(),
                updated_at: session.updated_at.clone(),
                message_count: session.messages.len(),
            });
        }
        summaries.sort_by(|a, b| {
            session_sort_key(&b.updated_at)
                .cmp(&session_sort_key(&a.updated_at))
                .then_with(|| b.id.cmp(&a.id))
        });
        Ok(summaries)
    }

    fn delete(&self, id: &str) -> Result<bool> {
        let path = self.session_path(id)?;
        if !path.exists() {
            return Ok(false);
        }
        let was_current = self.current_session_id()?.as_deref() == Some(id);
        remove_file(&path)?;
        self.prune_refs_to(id)?;
        if was_current {
            self.clear_head()?;
        }
        Ok(true)
    }

    fn latest(&self) -> Result<Option<Session>> {
        match self.list()?.into_iter().next() {
            Some(summary) => self.load(&summary.id),
            None => Ok(None),
        }
    }

    fn current(&self) -> Result<Option<Session>> {
        match self.current_session_id()? {
            Some(id) => self.load(&id),
            None => Ok(None),
        }
    }

    fn current_session_id(&self) -> Result<Option<String>> {
        let Some(head) = self.read_head()? else {
            return Ok(None);
        };
        if let Some(branch) = head.strip_prefix("ref: refs/heads/") {
            let branch = branch.trim();
            validate_branch_name(branch)?;
            return self.read_branch_ref(branch);
        }
        let id = head.trim();
        if id.is_empty() {
            return Ok(None);
        }
        validate_session_id(id)?;
        Ok(Some(id.to_string()))
    }

    fn checkout(&self, target: &str) -> Result<Session> {
        if let Some(session_id) = self.read_branch_ref(target)? {
            let old_session_id = self.current_session_id()?;
            self.write_head_ref(target)?;
            self.append_reflog(
                "HEAD",
                old_session_id.as_deref(),
                Some(&session_id),
                "checkout",
            )?;
            return self
                .load(&session_id)?
                .ok_or_else(|| SessionError::MissingBranchTarget {
                    branch: target.to_string(),
                    session_id,
                });
        }

        if let Some(commit) = self.read_commit(target)? {
            let old_session_id = self.current_session_id()?;
            self.write_head_detached(&commit.session_id)?;
            self.append_reflog(
                "HEAD",
                old_session_id.as_deref(),
                Some(&commit.session_id),
                "checkout",
            )?;
            return match self.load(&commit.session_id)? {
                Some(session) => Ok(session),
                None => self.session_from_commit(&commit),
            };
        }

        let session_id = self.resolve_session_id(target)?;
        let old_session_id = self.current_session_id()?;
        self.write_head_detached(&session_id)?;
        self.append_reflog(
            "HEAD",
            old_session_id.as_deref(),
            Some(&session_id),
            "checkout",
        )?;
        self.load(&session_id)?
            .ok_or(SessionError::NotFound(session_id))
    }

    fn create_branch(&self, name: &str, from: Option<&str>) -> Result<Session> {
        validate_branch_name(name)?;
        if self.read_branch_ref(name)?.is_some() {
            return Err(SessionError::BranchExists(name.to_string()));
        }
        let base = match from {
            Some(target) => {
                let id = self.target_session_id(target)?;
                self.load(&id)?.ok_or(SessionError::NotFound(id))?
            }
            None => self
                .current()?
                .or(self.latest()?)
                .ok_or(SessionError::NoSession)?,
        };
        let fork = Session::fork_from(&base, Some(name.to_string()));
        self.write_session(&fork)?;
        self.record_commit(&fork)?;
        self.write_branch_ref(name, &fork.id)?;
        self.append_reflog(
            &format!("refs/heads/{name}"),
            None,
            Some(&fork.id),
            "branch",
        )?;
        Ok(fork)
    }

    fn branches(&self) -> Result<Vec<BranchSummary>> {
        let current_branch = self.current_branch_name()?;
        let mut branches = self
            .branch_refs()?
            .into_iter()
            .map(|(name, session_id)| {
                let session = self.load(&session_id).ok().flatten();
                BranchSummary {
                    is_current: current_branch.as_deref() == Some(name.as_str()),
                    name,
                    session_id,
                    title: session.as_ref().and_then(|s| s.title.clone()),
                    updated_at: session.as_ref().map(|s| s.updated_at.clone()),
                }
            })
            .collect::<Vec<_>>();
        branches.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(branches)
    }

    fn log(&self, id: Option<&str>) -> Result<Vec<SessionTurnSummary>> {
        let session = match id {
            Some(id) => {
                let full_id = self.target_session_id(id)?;
                self.load(&full_id)?
                    .ok_or(SessionError::NotFound(full_id))?
            }
            None => self
                .current()?
                .or(self.latest()?)
                .ok_or(SessionError::NoSession)?,
        };
        Ok(turn_summaries(&session))
    }

    fn resolve_session_id(&self, target: &str) -> Result<String> {
        validate_session_id(target)?;
        if self.load(target)?.is_some() {
            return Ok(target.to_string());
        }
        let matches = self
            .list()?
            .into_iter()
            .filter(|s| s.id.starts_with(target))
            .map(|s| s.id)
            .collect::<Vec<_>>();
        match matches.len() {
            0 => Err(SessionError::NotFound(target.to_string())),
            1 => Ok(matches[0].clone()),
            _ => Err(SessionError::AmbiguousPrefix(target.to_string())),
        }
    }

    fn reflog(&self, target: Option<&str>) -> Result<Vec<ReflogEntry>> {
        let ref_name = match target {
            None => "HEAD".to_string(),
            Some("HEAD") => "HEAD".to_string(),
            Some(target) if self.read_branch_ref(target)?.is_some() => {
                format!("refs/heads/{target}")
            }
            Some(target) if self.read_tag_ref(target)?.is_some() => format!("refs/tags/{target}"),
            Some(target) => target.to_string(),
        };
        let path = self.log_path(&ref_name)?;
        if !path.exists() {
            return Ok(vec![]);
        }
        let content = read_to_string(&path)?;
        let mut entries = Vec::new();
        for line in content.lines().filter(|line| !line.trim().is_empty()) {
            let entry =
                serde_json::from_str::<ReflogEntry>(line).map_err(|source| SessionError::Json {
                    path: path.clone(),
                    source,
                })?;
            entries.push(entry);
        }
        Ok(entries)
    }

    fn reset(&self, target: &str) -> Result<Session> {
        let session_id = self.target_session_id(target)?;
        let session = self
            .load(&session_id)?
            .ok_or_else(|| SessionError::NotFound(session_id.clone()))?;
        self.advance_head(&session_id, "reset")?;
        Ok(session)
    }

    fn create_tag(&self, name: &str, target: Option<&str>) -> Result<TagSummary> {
        validate_branch_name(name)?;
        if self.read_tag_ref(name)?.is_some() {
            return Err(SessionError::TagExists(name.to_string()));
        }
        let session = match target {
            Some(target) => {
                let id = self.target_session_id(target)?;
                self.load(&id)?.ok_or(SessionError::NotFound(id))?
            }
            None => self
                .current()?
                .or(self.latest()?)
                .ok_or(SessionError::NoSession)?,
        };
        self.write_tag_ref(name, &session.id)?;
        self.append_reflog(&format!("refs/tags/{name}"), None, Some(&session.id), "tag")?;
        Ok(TagSummary {
            name: name.to_string(),
            session_id: session.id.clone(),
            commit_id: self.commit_id_for_session(&session.id)?,
            title: session.title,
            updated_at: Some(session.updated_at),
        })
    }

    fn tags(&self) -> Result<Vec<TagSummary>> {
        self.tag_refs()?
            .into_iter()
            .map(|(name, session_id)| {
                let session = self.load(&session_id)?;
                Ok(TagSummary {
                    name,
                    commit_id: self.commit_id_for_session(&session_id)?,
                    session_id,
                    title: session.as_ref().and_then(|s| s.title.clone()),
                    updated_at: session.as_ref().map(|s| s.updated_at.clone()),
                })
            })
            .collect()
    }

    fn diff(&self, from: &str, to: &str) -> Result<SessionDiff> {
        let from_id = self.target_session_id(from)?;
        let to_id = self.target_session_id(to)?;
        let from_session = self
            .load(&from_id)?
            .ok_or_else(|| SessionError::NotFound(from_id.clone()))?;
        let to_session = self
            .load(&to_id)?
            .ok_or_else(|| SessionError::NotFound(to_id.clone()))?;
        Ok(session_diff(&from_session, &to_session))
    }

    fn cherry_pick(&self, source: &str, turns: &[usize]) -> Result<Session> {
        let source_id = self.target_session_id(source)?;
        let source = self
            .load(&source_id)?
            .ok_or_else(|| SessionError::NotFound(source_id.clone()))?;
        let mut current = self.current()?.ok_or(SessionError::NoSession)?;
        let picked = pick_turn_messages(&source, turns);
        if picked.is_empty() {
            return Ok(current);
        }
        current.parent_id = Some(current.id.clone());
        current.id = Uuid::new_v4().simple().to_string();
        current.messages.extend(picked);
        current.updated_at = now_rfc3339();
        current.normalize();
        self.save(&current)?;
        self.append_reflog(
            "HEAD",
            current.parent_id.as_deref(),
            Some(&current.id),
            "cherry-pick",
        )?;
        Ok(current)
    }

    fn merge(&self, target: &str) -> Result<MergeResult> {
        let target_id = self.target_session_id(target)?;
        let target = self
            .load(&target_id)?
            .ok_or_else(|| SessionError::NotFound(target_id.clone()))?;
        let mut current = self.current()?.ok_or(SessionError::NoSession)?;
        let base = common_prefix_len(&current.messages, &target.messages);
        let additions = target
            .messages
            .iter()
            .skip(base)
            .cloned()
            .collect::<Vec<_>>();
        if additions.is_empty() {
            return Ok(MergeResult {
                session: current,
                base_session_id: None,
                appended_messages: 0,
                conflicts: vec![],
            });
        }
        let old_id = current.id.clone();
        current.parent_id = Some(old_id.clone());
        current.id = Uuid::new_v4().simple().to_string();
        current.messages.extend(additions.clone());
        current.updated_at = now_rfc3339();
        current.normalize();
        self.save(&current)?;
        self.append_reflog("HEAD", Some(&old_id), Some(&current.id), "merge")?;
        Ok(MergeResult {
            session: current,
            base_session_id: Some(target.id),
            appended_messages: additions.len(),
            conflicts: vec![],
        })
    }

    fn rebase_onto(&self, target: &str) -> Result<Session> {
        let target_id = self.target_session_id(target)?;
        let target = self
            .load(&target_id)?
            .ok_or_else(|| SessionError::NotFound(target_id.clone()))?;
        let current = self.current()?.ok_or(SessionError::NoSession)?;
        let base = common_prefix_len(&target.messages, &current.messages);
        let replay = current
            .messages
            .iter()
            .skip(base)
            .cloned()
            .collect::<Vec<_>>();
        let old_id = current.id.clone();
        let mut rebased = Session::fork_from(&target, current.branch.clone());
        rebased.parent_id = Some(target.id.clone());
        rebased.messages.extend(replay);
        rebased.updated_at = now_rfc3339();
        rebased.normalize();
        self.save(&rebased)?;
        self.append_reflog("HEAD", Some(&old_id), Some(&rebased.id), "rebase")?;
        Ok(rebased)
    }

    fn stash_push(&self, message: Option<&str>) -> Result<StashEntry> {
        let session = self.current()?.ok_or(SessionError::NoSession)?;
        create_dir_all(self.stash_dir())?;
        let entry = StashEntry {
            id: Uuid::new_v4().simple().to_string(),
            session_id: session.id.clone(),
            commit_id: self.commit_id_for_session(&session.id)?,
            message: message.map(str::to_string),
            title: session.title.clone(),
            created_at: now_rfc3339(),
            message_count: session.messages.len(),
        };
        write_file(self.stash_path(&entry.id)?, stable_json("stash", &entry)?)?;
        Ok(entry)
    }

    fn stash_list(&self) -> Result<Vec<StashEntry>> {
        if !self.stash_dir().exists() {
            return Ok(vec![]);
        }
        let mut entries = Vec::new();
        for entry in read_dir(self.stash_dir())? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let content = read_to_string(&path)?;
            entries.push(
                serde_json::from_str::<StashEntry>(&content).map_err(|source| {
                    SessionError::Json {
                        path: path.clone(),
                        source,
                    }
                })?,
            );
        }
        entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(entries)
    }

    fn stash_apply(&self, id: &str) -> Result<Session> {
        let path = self.stash_path(id)?;
        if !path.exists() {
            return Err(SessionError::StashNotFound(id.to_string()));
        }
        let content = read_to_string(path)?;
        let entry: StashEntry =
            serde_json::from_str(&content).map_err(|source| SessionError::Json {
                path: self
                    .stash_path(id)
                    .unwrap_or_else(|_| self.stash_dir().join(id)),
                source,
            })?;
        self.load(&entry.session_id)?
            .ok_or_else(|| SessionError::NotFound(entry.session_id.clone()))?;
        self.checkout(&entry.session_id)
    }

    fn gc(&self, prune: bool) -> Result<GcReport> {
        let reachable_sessions = self.reachable_session_ids()?;
        let (reachable_commits, reachable_messages, reachable_artifacts) =
            self.reachable_object_ids()?;
        let all_sessions = self.list()?.len();
        let mut pruned_objects = 0usize;
        if prune && self.commits_dir().exists() {
            for entry in read_dir(self.commits_dir())? {
                let path = entry?.path();
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if !reachable_commits.iter().any(|id| id == stem) {
                    remove_file(path)?;
                    pruned_objects += 1;
                }
            }
        }
        if prune && self.messages_dir().exists() {
            for entry in read_dir(self.messages_dir())? {
                let path = entry?.path();
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if !reachable_messages.contains(stem) {
                    remove_file(path)?;
                    pruned_objects += 1;
                }
            }
        }
        if prune && self.artifacts_dir().exists() {
            for entry in read_dir(self.artifacts_dir())? {
                let path = entry?.path();
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if !reachable_artifacts.contains(stem) {
                    remove_file(path)?;
                    pruned_objects += 1;
                }
            }
        }
        Ok(GcReport {
            reachable_sessions: reachable_sessions.len(),
            unreachable_sessions: all_sessions.saturating_sub(reachable_sessions.len()),
            reachable_commits: reachable_commits.len(),
            reachable_messages: reachable_messages.len(),
            reachable_artifacts: reachable_artifacts.len(),
            pruned_objects,
            dry_run: !prune,
        })
    }

    fn migrate_legacy_sessions(&self) -> Result<MigrationReport> {
        let sessions = self.list()?;
        let mut migrated_sessions = 0usize;
        for summary in &sessions {
            if self.commit_id_for_session(&summary.id)?.is_none() {
                if let Some(session) = self.load(&summary.id)? {
                    self.record_commit(&session)?;
                    migrated_sessions += 1;
                }
            }
        }
        Ok(MigrationReport {
            scanned_sessions: sessions.len(),
            migrated_sessions,
        })
    }

    fn put_artifact(&self, artifact: NewArtifact) -> Result<ArtifactObject> {
        create_dir_all(self.artifacts_dir())?;
        let content_hash = object_id("artifact_content", artifact.content.as_bytes());
        let payload = (
            artifact.kind.as_str(),
            &content_hash,
            &artifact.path,
            &artifact.mime,
            &artifact.content,
            &artifact.metadata,
        );
        let payload_json = stable_json("artifact", &payload)?;
        let id = object_id("artifact", payload_json.as_bytes());
        let object = ArtifactObject {
            id,
            kind: artifact.kind,
            content_hash,
            size: artifact.content.len(),
            content: artifact.content,
            path: artifact.path,
            mime: artifact.mime,
            created_at: now_rfc3339(),
            metadata: artifact.metadata,
        };
        write_file(
            self.artifact_path(&object.id)?,
            stable_json("artifact", &object)?,
        )?;
        Ok(object)
    }

    fn get_artifact(&self, id: &str) -> Result<Option<ArtifactObject>> {
        let path = self.artifact_path(id)?;
        if !path.exists() {
            return Ok(None);
        }
        let content = read_to_string(&path)?;
        serde_json::from_str(&content)
            .map(Some)
            .map_err(|source| SessionError::Json { path, source })
    }

    fn list_artifacts(&self) -> Result<Vec<ArtifactSummary>> {
        if !self.artifacts_dir().exists() {
            return Ok(vec![]);
        }
        let mut artifacts = Vec::new();
        for entry in read_dir(self.artifacts_dir())? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let content = read_to_string(&path)?;
            let object: ArtifactObject =
                serde_json::from_str(&content).map_err(|source| SessionError::Json {
                    path: path.clone(),
                    source,
                })?;
            artifacts.push(ArtifactSummary {
                id: object.id,
                kind: object.kind,
                path: object.path,
                mime: object.mime,
                size: object.size,
                created_at: object.created_at,
            });
        }
        artifacts.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(artifacts)
    }
}

#[derive(Debug, Clone)]
pub struct SqliteCasSessionStore {
    dir: PathBuf,
    db_path: PathBuf,
}

/// Reachable CAS IDs: (commits, messages, artifacts, turn_manifests).
type ReachableCasIds = (
    BTreeSet<String>,
    BTreeSet<String>,
    BTreeSet<String>,
    BTreeSet<String>,
);

impl SqliteCasSessionStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let db_path = dir.join("session.db");
        Self { dir, db_path }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    fn legacy_store(&self) -> FileSessionStore {
        FileSessionStore::new(self.dir.clone())
    }

    fn conn(&self) -> Result<Connection> {
        create_dir_all(&self.dir)?;
        let conn = Connection::open(&self.db_path).map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;
        conn.execute_batch(
            "
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                model TEXT NOT NULL,
                provider TEXT NOT NULL,
                title TEXT,
                parent_id TEXT,
                branch TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                message_count INTEGER NOT NULL,
                commit_id TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS refs (
                name TEXT NOT NULL,
                kind TEXT NOT NULL,
                session_id TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (kind, name)
            );
            CREATE TABLE IF NOT EXISTS objects (
                id TEXT NOT NULL,
                kind TEXT NOT NULL,
                path TEXT NOT NULL,
                size INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (kind, id)
            );
            CREATE TABLE IF NOT EXISTS commits (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                parents_json TEXT NOT NULL,
                parent_session_ids_json TEXT NOT NULL,
                message_ids_json TEXT NOT NULL,
                turn_manifest_ids_json TEXT NOT NULL DEFAULT '[]',
                model TEXT NOT NULL,
                provider TEXT NOT NULL,
                title TEXT,
                branch TEXT,
                created_at TEXT NOT NULL,
                committed_at TEXT NOT NULL,
                object_path TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS turn_manifests (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                commit_id TEXT,
                turn_index INTEGER NOT NULL,
                request_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                object_path TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS profile_snapshots (
                session_id TEXT PRIMARY KEY,
                hash TEXT NOT NULL,
                source_label TEXT NOT NULL,
                source_fingerprint TEXT,
                captured_at TEXT NOT NULL,
                summary_json TEXT NOT NULL,
                canonical_spec_json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS reflog (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                ref_name TEXT NOT NULL,
                old_session_id TEXT,
                new_session_id TEXT,
                old_commit_id TEXT,
                new_commit_id TEXT,
                action TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS stash (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                commit_id TEXT,
                message TEXT,
                title TEXT,
                created_at TEXT NOT NULL,
                message_count INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS artifacts (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                path TEXT,
                mime TEXT,
                size INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                metadata_json TEXT NOT NULL,
                object_path TEXT NOT NULL
            );
            ",
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;
        // Migrate: add turn_manifest_ids_json to existing commits tables
        let has_column: bool = conn
            .prepare("PRAGMA table_info(commits)")
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?
            .filter_map(|r| r.ok())
            .any(|name| name == "turn_manifest_ids_json");
        if !has_column {
            conn.execute(
                "ALTER TABLE commits ADD COLUMN turn_manifest_ids_json TEXT NOT NULL DEFAULT '[]'",
                [],
            )
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        }
        Ok(conn)
    }

    fn cas_path(&self, kind: &str, id: &str) -> PathBuf {
        let first = id.get(0..2).unwrap_or("xx");
        let second = id.get(2..4).unwrap_or("xx");
        self.dir
            .join("objects")
            .join(kind)
            .join(first)
            .join(second)
            .join(format!("{id}.json"))
    }

    fn write_cas_object(
        &self,
        conn: &Connection,
        kind: &'static str,
        id: &str,
        content: &str,
    ) -> Result<PathBuf> {
        let path = self.cas_path(kind, id);
        if !path.exists() {
            if let Some(parent) = path.parent() {
                create_dir_all(parent)?;
            }
            write_file(&path, content.to_string())?;
        }
        conn.execute(
            "INSERT OR IGNORE INTO objects (id, kind, path, size, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id,
                kind,
                path.to_string_lossy().to_string(),
                content.len() as i64,
                now_rfc3339()
            ],
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;
        Ok(path)
    }

    fn write_message_object(&self, conn: &Connection, message: &SessionMessage) -> Result<String> {
        let message_content = stable_json("message", message)?;
        let id = object_id("message", message_content.as_bytes());
        let object = SessionMessageObject {
            id: id.clone(),
            message: message.clone(),
        };
        let content = stable_json("message", &object)?;
        self.write_cas_object(conn, "messages", &id, &content)?;
        Ok(id)
    }

    fn read_message_object(&self, id: &str) -> Result<Option<SessionMessage>> {
        validate_session_id(id)?;
        let conn = self.conn()?;
        let path: Option<String> = conn
            .query_row(
                "SELECT path FROM objects WHERE kind = 'messages' AND id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        let Some(path) = path else {
            return Ok(None);
        };
        let path = PathBuf::from(path);
        let content = read_to_string(&path)?;
        let object: SessionMessageObject =
            serde_json::from_str(&content).map_err(|source| SessionError::Json { path, source })?;
        Ok(Some(object.message))
    }

    fn record_session(&self, session: &Session) -> Result<SessionCommit> {
        self.record_session_with_manifests(session, Vec::new())
    }

    /// Record a session with turn manifest IDs attached to the commit.
    pub fn record_session_with_manifests(
        &self,
        session: &Session,
        turn_manifest_ids: Vec<String>,
    ) -> Result<SessionCommit> {
        validate_session_id(&session.id)?;
        let conn = self.conn()?;
        self.record_session_with_manifests_with_conn(&conn, session, turn_manifest_ids)
    }

    fn record_session_with_manifests_with_conn(
        &self,
        conn: &Connection,
        session: &Session,
        turn_manifest_ids: Vec<String>,
    ) -> Result<SessionCommit> {
        validate_session_id(&session.id)?;
        let commit = self.record_commit_with_conn(conn, session, turn_manifest_ids)?;
        conn.execute(
            "INSERT OR REPLACE INTO sessions
             (id, model, provider, title, parent_id, branch, created_at, updated_at, message_count, commit_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                &session.id,
                &session.model,
                &session.provider,
                &session.title,
                &session.parent_id,
                &session.branch,
                &session.created_at,
                &session.updated_at,
                session.messages.len() as i64,
                &commit.id,
            ],
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;
        Ok(commit)
    }

    fn record_commit_with_conn(
        &self,
        conn: &Connection,
        session: &Session,
        turn_manifest_ids: Vec<String>,
    ) -> Result<SessionCommit> {
        let message_ids = session
            .messages
            .iter()
            .map(|message| self.write_message_object(conn, message))
            .collect::<Result<Vec<_>>>()?;
        let parents = session
            .parent_id
            .as_deref()
            .and_then(|id| {
                self.commit_id_for_session_with_conn(conn, id)
                    .ok()
                    .flatten()
            })
            .into_iter()
            .collect::<Vec<_>>();
        let parent_session_ids = session.parent_id.clone().into_iter().collect::<Vec<_>>();
        let payload = SessionCommitPayload {
            session_id: session.id.clone(),
            parents,
            parent_session_ids,
            message_ids,
            turn_manifest_ids,
            model: session.model.clone(),
            provider: session.provider.clone(),
            title: session.title.clone(),
            branch: session.branch.clone(),
            created_at: session.created_at.clone(),
            committed_at: now_rfc3339(),
        };
        let content = stable_json("commit", &payload)?;
        let id = object_id("commit", content.as_bytes());
        let commit = SessionCommit {
            id: id.clone(),
            session_id: payload.session_id,
            parents: payload.parents,
            parent_session_ids: payload.parent_session_ids,
            message_ids: payload.message_ids,
            turn_manifest_ids: payload.turn_manifest_ids,
            model: payload.model,
            provider: payload.provider,
            title: payload.title,
            branch: payload.branch,
            created_at: payload.created_at,
            committed_at: payload.committed_at,
        };
        let commit_content = stable_json("commit", &commit)?;
        let object_path = self.write_cas_object(conn, "commits", &id, &commit_content)?;
        conn.execute(
            "INSERT OR IGNORE INTO commits
             (id, session_id, parents_json, parent_session_ids_json, message_ids_json,
              turn_manifest_ids_json, model, provider, title, branch, created_at, committed_at, object_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                &commit.id,
                &commit.session_id,
                stable_json("parents", &commit.parents)?,
                stable_json("parent_session_ids", &commit.parent_session_ids)?,
                stable_json("message_ids", &commit.message_ids)?,
                stable_json("turn_manifest_ids", &commit.turn_manifest_ids)?,
                &commit.model,
                &commit.provider,
                &commit.title,
                &commit.branch,
                &commit.created_at,
                &commit.committed_at,
                object_path.to_string_lossy().to_string(),
            ],
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;
        Ok(commit)
    }

    fn read_commit(&self, id: &str) -> Result<Option<SessionCommit>> {
        validate_session_id(id)?;
        let conn = self.conn()?;
        let path: Option<String> = conn
            .query_row(
                "SELECT object_path FROM commits WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        let Some(path) = path else {
            return Ok(None);
        };
        let path = PathBuf::from(path);
        let content = read_to_string(&path)?;
        serde_json::from_str(&content)
            .map(Some)
            .map_err(|source| SessionError::Json { path, source })
    }

    fn commit_id_for_session(&self, session_id: &str) -> Result<Option<String>> {
        validate_session_id(session_id)?;
        let conn = self.conn()?;
        self.commit_id_for_session_with_conn(&conn, session_id)
    }

    fn commit_id_for_session_with_conn(
        &self,
        conn: &Connection,
        session_id: &str,
    ) -> Result<Option<String>> {
        validate_session_id(session_id)?;
        conn.query_row(
            "SELECT commit_id FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })
    }

    fn session_from_commit(&self, commit: &SessionCommit) -> Result<Session> {
        let messages = commit
            .message_ids
            .iter()
            .map(|id| {
                self.read_message_object(id)?
                    .ok_or_else(|| SessionError::NotFound(id.clone()))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut session = Session {
            id: commit.session_id.clone(),
            model: commit.model.clone(),
            provider: commit.provider.clone(),
            title: commit.title.clone(),
            parent_id: commit.parent_session_ids.first().cloned(),
            branch: commit.branch.clone(),
            messages,
            created_at: commit.created_at.clone(),
            updated_at: commit.committed_at.clone(),
        };
        session.normalize();
        Ok(session)
    }

    fn read_head(&self) -> Result<Option<String>> {
        let conn = self.conn()?;
        self.read_head_with_conn(&conn)
    }

    fn read_head_with_conn(&self, conn: &Connection) -> Result<Option<String>> {
        conn.query_row("SELECT value FROM meta WHERE key = 'HEAD'", [], |row| {
            row.get(0)
        })
        .optional()
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })
    }

    fn write_head(&self, value: &str) -> Result<()> {
        let conn = self.conn()?;
        self.write_head_with_conn(&conn, value)
    }

    fn write_head_with_conn(&self, conn: &Connection, value: &str) -> Result<()> {
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('HEAD', ?1)",
            params![value],
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;
        Ok(())
    }

    fn clear_head(&self) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM meta WHERE key = 'HEAD'", [])
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        Ok(())
    }

    fn current_branch_name(&self) -> Result<Option<String>> {
        let conn = self.conn()?;
        self.current_branch_name_with_conn(&conn)
    }

    fn current_branch_name_with_conn(&self, conn: &Connection) -> Result<Option<String>> {
        let Some(head) = self.read_head_with_conn(conn)? else {
            return Ok(None);
        };
        Ok(head
            .strip_prefix("ref: refs/heads/")
            .map(|branch| branch.trim().to_string()))
    }

    fn read_ref(&self, kind: &str, name: &str) -> Result<Option<String>> {
        validate_branch_name(name)?;
        let conn = self.conn()?;
        self.read_ref_with_conn(&conn, kind, name)
    }

    fn read_ref_with_conn(
        &self,
        conn: &Connection,
        kind: &str,
        name: &str,
    ) -> Result<Option<String>> {
        validate_branch_name(name)?;
        conn.query_row(
            "SELECT session_id FROM refs WHERE kind = ?1 AND name = ?2",
            params![kind, name],
            |row| row.get(0),
        )
        .optional()
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })
    }

    fn write_ref(&self, kind: &str, name: &str, session_id: &str) -> Result<()> {
        validate_branch_name(name)?;
        validate_session_id(session_id)?;
        let conn = self.conn()?;
        self.write_ref_with_conn(&conn, kind, name, session_id)
    }

    fn write_ref_with_conn(
        &self,
        conn: &Connection,
        kind: &str,
        name: &str,
        session_id: &str,
    ) -> Result<()> {
        validate_branch_name(name)?;
        validate_session_id(session_id)?;
        conn.execute(
            "INSERT OR REPLACE INTO refs (name, kind, session_id, updated_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![name, kind, session_id, now_rfc3339()],
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;
        Ok(())
    }

    fn refs_by_kind(&self, kind: &str) -> Result<Vec<(String, String)>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare("SELECT name, session_id FROM refs WHERE kind = ?1 ORDER BY name")
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        let rows = stmt
            .query_map(params![kind], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })
    }

    fn append_reflog(
        &self,
        ref_name: &str,
        old_session_id: Option<&str>,
        new_session_id: Option<&str>,
        action: &str,
    ) -> Result<()> {
        let conn = self.conn()?;
        self.append_reflog_with_conn(&conn, ref_name, old_session_id, new_session_id, action)
    }

    fn append_reflog_with_conn(
        &self,
        conn: &Connection,
        ref_name: &str,
        old_session_id: Option<&str>,
        new_session_id: Option<&str>,
        action: &str,
    ) -> Result<()> {
        let old_commit_id = old_session_id
            .map(|id| self.commit_id_for_session_with_conn(conn, id))
            .transpose()?
            .flatten();
        let new_commit_id = new_session_id
            .map(|id| self.commit_id_for_session_with_conn(conn, id))
            .transpose()?
            .flatten();
        conn.execute(
            "INSERT INTO reflog
             (timestamp, ref_name, old_session_id, new_session_id, old_commit_id, new_commit_id, action)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                now_rfc3339(),
                ref_name,
                old_session_id,
                new_session_id,
                old_commit_id,
                new_commit_id,
                action,
            ],
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;
        Ok(())
    }

    fn advance_head(&self, session_id: &str, action: &str) -> Result<()> {
        validate_session_id(session_id)?;
        let conn = self.conn()?;
        self.advance_head_with_conn(&conn, session_id, action)
    }

    fn current_session_id_with_conn(&self, conn: &Connection) -> Result<Option<String>> {
        let Some(head) = self.read_head_with_conn(conn)? else {
            return Ok(None);
        };
        if let Some(branch) = head.strip_prefix("ref: refs/heads/") {
            return self.read_ref_with_conn(conn, "branch", branch.trim());
        }
        Ok(Some(head.trim().to_string()))
    }

    fn advance_head_with_conn(
        &self,
        conn: &Connection,
        session_id: &str,
        action: &str,
    ) -> Result<()> {
        validate_session_id(session_id)?;
        match self.current_branch_name_with_conn(conn)? {
            Some(branch) => {
                let old_session_id = self.read_ref_with_conn(conn, "branch", &branch)?;
                self.write_ref_with_conn(conn, "branch", &branch, session_id)?;
                self.append_reflog_with_conn(
                    conn,
                    &format!("refs/heads/{branch}"),
                    old_session_id.as_deref(),
                    Some(session_id),
                    action,
                )?;
                self.append_reflog_with_conn(
                    conn,
                    "HEAD",
                    old_session_id.as_deref(),
                    Some(session_id),
                    action,
                )
            }
            None => {
                if self.read_head_with_conn(conn)?.is_some() {
                    let old_session_id = self.current_session_id_with_conn(conn)?;
                    self.write_head_with_conn(conn, session_id)?;
                    self.append_reflog_with_conn(
                        conn,
                        "HEAD",
                        old_session_id.as_deref(),
                        Some(session_id),
                        action,
                    )
                } else {
                    self.write_ref_with_conn(conn, "branch", DEFAULT_BRANCH, session_id)?;
                    self.write_head_with_conn(conn, &format!("ref: refs/heads/{DEFAULT_BRANCH}"))?;
                    self.append_reflog_with_conn(
                        conn,
                        &format!("refs/heads/{DEFAULT_BRANCH}"),
                        None,
                        Some(session_id),
                        action,
                    )?;
                    self.append_reflog_with_conn(conn, "HEAD", None, Some(session_id), action)
                }
            }
        }
    }

    fn target_session_id(&self, target: &str) -> Result<String> {
        if let Some(id) = self.read_ref("branch", target)? {
            return Ok(id);
        }
        if let Some(id) = self.read_ref("tag", target)? {
            return Ok(id);
        }
        if let Some(commit) = self.read_commit(target)? {
            return Ok(commit.session_id);
        }
        self.resolve_session_id(target)
    }

    fn reachable_session_ids(&self) -> Result<BTreeSet<String>> {
        let mut reachable = BTreeSet::new();
        if let Some(id) = self.current_session_id()? {
            reachable.insert(id);
        }
        for (_, id) in self.refs_by_kind("branch")? {
            reachable.insert(id);
        }
        for (_, id) in self.refs_by_kind("tag")? {
            reachable.insert(id);
        }
        for entry in self.stash_list()? {
            reachable.insert(entry.session_id);
        }
        Ok(reachable)
    }

    fn reachable_cas_ids(&self) -> Result<ReachableCasIds> {
        let reachable_sessions = self.reachable_session_ids()?;
        let mut commits = BTreeSet::new();
        let mut messages = BTreeSet::new();
        let mut artifacts = BTreeSet::new();
        let mut manifests = BTreeSet::new();
        for session_id in reachable_sessions {
            if let Some(commit_id) = self.commit_id_for_session(&session_id)? {
                if let Some(commit) = self.read_commit(&commit_id)? {
                    commits.insert(commit_id);
                    for message_id in commit.message_ids {
                        if messages.insert(message_id.clone()) {
                            if let Some(message) = self.read_message_object(&message_id)? {
                                artifacts.extend(message.artifact_ids);
                            }
                        }
                    }
                    for manifest_id in commit.turn_manifest_ids {
                        manifests.insert(manifest_id);
                    }
                }
            }
        }
        Ok((commits, messages, artifacts, manifests))
    }

    fn prune_refs_to(&self, session_id: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM refs WHERE session_id = ?1",
            params![session_id],
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;
        Ok(())
    }

    fn backfill_turn_manifest_commit_ids_with_conn(
        &self,
        conn: &Connection,
        commit_id: &str,
        manifest_ids: &[String],
    ) -> Result<()> {
        if manifest_ids.is_empty() {
            return Ok(());
        }
        validate_session_id(commit_id)?;
        for manifest_id in manifest_ids {
            validate_session_id(manifest_id)?;
            conn.execute(
                "UPDATE turn_manifests
                 SET commit_id = COALESCE(commit_id, ?1)
                 WHERE id = ?2",
                params![commit_id, manifest_id],
            )
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        }
        Ok(())
    }

    fn write_profile_snapshot_with_conn(
        &self,
        conn: &Connection,
        session_id: &str,
        snapshot: &ProfileSnapshot,
    ) -> Result<()> {
        validate_session_id(session_id)?;
        conn.execute(
            "INSERT INTO profile_snapshots
             (session_id, hash, source_label, source_fingerprint, captured_at, summary_json, canonical_spec_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                session_id,
                &snapshot.hash,
                &snapshot.source.source_label,
                &snapshot.source.source_fingerprint,
                &snapshot.captured_at,
                stable_json("profile_snapshot_summary", &snapshot.summary)?,
                stable_json("profile_snapshot_canonical_spec", &snapshot.canonical_spec)?,
            ],
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;
        Ok(())
    }

    fn profile_snapshot_with_conn(
        &self,
        conn: &Connection,
        session_id: &str,
    ) -> Result<Option<ProfileSnapshot>> {
        validate_session_id(session_id)?;
        let row: Option<(
            String,
            String,
            Option<String>,
            String,
            String,
            String,
        )> = conn
            .query_row(
                "SELECT hash, source_label, source_fingerprint, captured_at, summary_json, canonical_spec_json
                 FROM profile_snapshots WHERE session_id = ?1",
                params![session_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        let Some((
            hash,
            source_label,
            source_fingerprint,
            captured_at,
            summary_json,
            canonical_spec_json,
        )) = row
        else {
            return Ok(None);
        };
        let summary = serde_json::from_str(&summary_json).map_err(|source| SessionError::Json {
            path: self.db_path.clone(),
            source,
        })?;
        let canonical_spec =
            serde_json::from_str(&canonical_spec_json).map_err(|source| SessionError::Json {
                path: self.db_path.clone(),
                source,
            })?;
        Ok(Some(ProfileSnapshot {
            hash,
            source: ProfileSourceAudit {
                source_label,
                source_fingerprint,
            },
            canonical_spec,
            summary,
            captured_at,
        }))
    }

    fn next_turn_index_with_conn(
        &self,
        conn: &Connection,
        session_id: &str,
        history: &[SessionMessage],
        parent_manifest_ids: &[String],
    ) -> Result<usize> {
        let mut max_parent_index = None;
        for manifest_id in parent_manifest_ids {
            if let Some(manifest) = self.get_turn_manifest_with_conn(conn, manifest_id)? {
                max_parent_index = Some(
                    max_parent_index
                        .map(|index: usize| index.max(manifest.turn_index))
                        .unwrap_or(manifest.turn_index),
                );
            }
        }
        if let Some(index) = max_parent_index {
            return Ok(index.saturating_add(1));
        }

        let next_from_db: Option<i64> = conn
            .query_row(
                "SELECT MAX(turn_index) + 1 FROM turn_manifests WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        if let Some(index) = next_from_db {
            return Ok(index.max(0) as usize);
        }

        Ok(history
            .iter()
            .filter(|message| {
                message.role.eq_ignore_ascii_case("user") && message.compact.is_none()
            })
            .count())
    }

    fn put_turn_manifest_with_conn(
        &self,
        conn: &Connection,
        manifest: NewTurnManifest,
    ) -> Result<TurnManifestObject> {
        // Compute ID from stable content fields only (not created_at).
        // This makes the ID a pure function of the manifest content + schema version,
        // so repeated puts with the same manifest produce the same ID.
        let id = turn_manifest_id(&manifest)?;
        // Idempotent: if the row already exists, return the stored object.
        if let Some(existing) = self.get_turn_manifest_with_conn(conn, &id)? {
            return Ok(existing);
        }
        let commit_id = manifest.commit_id;
        let created_at = now_rfc3339();
        let stored = StoredTurnManifestObject {
            id: id.clone(),
            schema_version: TURN_MANIFEST_SCHEMA_VERSION,
            session_id: manifest.session_id,
            turn_index: manifest.turn_index,
            request_id: manifest.request_id,
            parent_manifest_ids: manifest.parent_manifest_ids,
            model: manifest.model,
            provider: manifest.provider,
            prompt: manifest.prompt,
            hooks: manifest.hooks,
            tools: manifest.tools,
            metadata: manifest.metadata,
        };
        let content = stable_json("turn_manifest", &stored)?;
        let object_path = self.write_cas_object(conn, "turn_manifests", &id, &content)?;
        conn.execute(
            "INSERT OR IGNORE INTO turn_manifests
             (id, session_id, commit_id, turn_index, request_id, created_at, object_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                &stored.id,
                &stored.session_id,
                &commit_id,
                stored.turn_index as i64,
                &stored.request_id,
                &created_at,
                object_path.to_string_lossy().to_string(),
            ],
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;
        Ok(TurnManifestObject {
            id: stored.id,
            schema_version: stored.schema_version,
            session_id: stored.session_id,
            commit_id,
            turn_index: stored.turn_index,
            request_id: stored.request_id,
            parent_manifest_ids: stored.parent_manifest_ids,
            model: stored.model,
            provider: stored.provider,
            created_at,
            prompt: stored.prompt,
            hooks: stored.hooks,
            tools: stored.tools,
            metadata: stored.metadata,
        })
    }

    fn get_turn_manifest_with_conn(
        &self,
        conn: &Connection,
        id: &str,
    ) -> Result<Option<TurnManifestObject>> {
        validate_session_id(id)?;
        let row: Option<(String, Option<String>, String)> = conn
            .query_row(
                "SELECT object_path, commit_id, created_at FROM turn_manifests WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        let Some((path, commit_id, created_at)) = row else {
            return Ok(None);
        };
        let path = PathBuf::from(path);
        let content = read_to_string(&path)?;
        let stored: StoredTurnManifestObject =
            serde_json::from_str(&content).map_err(|source| SessionError::Json { path, source })?;
        Ok(Some(TurnManifestObject {
            id: stored.id,
            schema_version: stored.schema_version,
            session_id: stored.session_id,
            commit_id,
            turn_index: stored.turn_index,
            request_id: stored.request_id,
            parent_manifest_ids: stored.parent_manifest_ids,
            model: stored.model,
            provider: stored.provider,
            created_at,
            prompt: stored.prompt,
            hooks: stored.hooks,
            tools: stored.tools,
            metadata: stored.metadata,
        }))
    }
}

impl SessionStore for SqliteCasSessionStore {
    fn save(&self, session: &Session) -> Result<()> {
        self.record_session(session)?;
        self.advance_head(&session.id, "commit")
    }

    fn save_with_manifests(
        &self,
        session: &Session,
        manifests: Vec<NewTurnManifest>,
    ) -> Result<()> {
        validate_session_id(&session.id)?;
        // Pre-compute manifest IDs and write CAS objects first, so the commit
        // never references a manifest that doesn't exist on disk.
        let mut manifest_ids = Vec::with_capacity(manifests.len());
        for manifest in &manifests {
            manifest_ids.push(turn_manifest_id(manifest)?);
        }

        let conn = self.conn()?;
        // Write manifest CAS objects (idempotent INSERT OR IGNORE)
        for mut manifest in manifests {
            // save_with_manifests owns the commit binding. Ignore any stale
            // caller-supplied reverse index value and backfill the final commit
            // id after the commit has been created.
            manifest.commit_id = None;
            self.put_turn_manifest_with_conn(&conn, manifest)?;
        }

        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;

        let result = (|| {
            // Now record the authoritative forward references, then backfill the
            // convenience reverse index without affecting CAS identity.
            let commit =
                self.record_session_with_manifests_with_conn(&conn, session, manifest_ids.clone())?;
            self.backfill_turn_manifest_commit_ids_with_conn(&conn, &commit.id, &manifest_ids)?;
            self.advance_head_with_conn(&conn, &session.id, "commit")
        })();

        match result {
            Ok(()) => conn
                .execute_batch("COMMIT")
                .map_err(|source| SessionError::Database {
                    path: self.db_path.clone(),
                    source,
                }),
            Err(err) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(err)
            }
        }
    }

    fn open_turn(&self, request: OpenTurnRequest) -> Result<OpenedTurn> {
        let conn = self.conn()?;
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;

        let result = (|| {
            let session_id = request
                .session_id
                .clone()
                .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
            validate_session_id(&session_id)?;

            let base_commit_id = self.commit_id_for_session_with_conn(&conn, &session_id)?;
            let history = match &base_commit_id {
                Some(commit_id) => self
                    .read_commit(commit_id)?
                    .map(|commit| self.session_from_commit(&commit))
                    .transpose()?
                    .map(|session| session.messages)
                    .unwrap_or_default(),
                None => Vec::new(),
            };
            let parent_manifest_ids = match &base_commit_id {
                Some(commit_id) => self
                    .read_commit(commit_id)?
                    .map(|commit| commit.turn_manifest_ids)
                    .unwrap_or_default(),
                None => Vec::new(),
            };
            let existing_snapshot = self.profile_snapshot_with_conn(&conn, &session_id)?;
            let profile_snapshot = match existing_snapshot {
                Some(snapshot) => snapshot,
                None => {
                    self.write_profile_snapshot_with_conn(
                        &conn,
                        &session_id,
                        &request.profile_snapshot,
                    )?;
                    request.profile_snapshot
                }
            };
            let profile_drift =
                profile_snapshot.drift_for(request.current_source_fingerprint.clone());
            let turn_index =
                self.next_turn_index_with_conn(&conn, &session_id, &history, &parent_manifest_ids)?;
            Ok(OpenedTurn {
                turn_id: Uuid::new_v4().simple().to_string(),
                session_id,
                base_commit_id,
                turn_index,
                profile_snapshot,
                profile_drift,
                history,
                parent_manifest_ids,
            })
        })();

        match result {
            Ok(opened) => {
                conn.execute_batch("COMMIT")
                    .map_err(|source| SessionError::Database {
                        path: self.db_path.clone(),
                        source,
                    })?;
                Ok(opened)
            }
            Err(err) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(err)
            }
        }
    }

    fn commit_turn(&self, request: CommitTurnRequest) -> Result<CommittedTurn> {
        validate_session_id(&request.session_id)?;
        validate_session_id(&request.turn_id)?;
        let conn = self.conn()?;
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;

        let result = (|| {
            let current_head = self.commit_id_for_session_with_conn(&conn, &request.session_id)?;
            if current_head != request.base_commit_id {
                return Err(SessionError::Conflict(format!(
                    "base commit mismatch for session '{}': expected {:?}, found {:?}",
                    request.session_id, request.base_commit_id, current_head
                )));
            }
            let snapshot = self
                .profile_snapshot_with_conn(&conn, &request.session_id)?
                .ok_or_else(|| {
                    SessionError::Conflict(format!(
                        "session '{}' has no bound profile snapshot",
                        request.session_id
                    ))
                })?;
            if snapshot.hash != request.profile_hash {
                return Err(SessionError::Conflict(format!(
                    "profile snapshot mismatch for session '{}': expected {}, found {}",
                    request.session_id, snapshot.hash, request.profile_hash
                )));
            }

            let mut session = match &request.base_commit_id {
                Some(commit_id) => self
                    .read_commit(commit_id)?
                    .map(|commit| self.session_from_commit(&commit))
                    .transpose()?
                    .ok_or_else(|| SessionError::NotFound(commit_id.clone()))?,
                None => Session::new(request.model.clone(), request.provider.clone()),
            };
            session.id = request.session_id.clone();
            session.model = request.model.clone();
            session.provider = request.provider.clone();
            session.messages.extend(request.messages.clone());
            session.touch();

            let mut manifests = request.manifests;
            for manifest in &mut manifests {
                manifest.session_id = request.session_id.clone();
                turn_tx::add_turn_metadata(manifest, &request.status, &snapshot);
            }
            let mut manifest_ids = Vec::with_capacity(manifests.len());
            for manifest in &manifests {
                manifest_ids.push(turn_manifest_id(manifest)?);
            }
            for mut manifest in manifests {
                manifest.commit_id = None;
                self.put_turn_manifest_with_conn(&conn, manifest)?;
            }
            let commit = self.record_session_with_manifests_with_conn(
                &conn,
                &session,
                manifest_ids.clone(),
            )?;
            self.backfill_turn_manifest_commit_ids_with_conn(&conn, &commit.id, &manifest_ids)?;
            self.advance_head_with_conn(&conn, &session.id, "commit")?;
            Ok(CommittedTurn {
                session_id: session.id,
                turn_id: request.turn_id,
                commit_id: commit.id,
                turn_manifest_ids: manifest_ids,
            })
        })();

        match result {
            Ok(committed) => {
                conn.execute_batch("COMMIT")
                    .map_err(|source| SessionError::Database {
                        path: self.db_path.clone(),
                        source,
                    })?;
                Ok(committed)
            }
            Err(err) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(err)
            }
        }
    }

    fn load(&self, id: &str) -> Result<Option<Session>> {
        validate_session_id(id)?;
        let conn = self.conn()?;
        let commit_id: Option<String> = conn
            .query_row(
                "SELECT commit_id FROM sessions WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        let Some(commit_id) = commit_id else {
            return Ok(None);
        };
        let Some(commit) = self.read_commit(&commit_id)? else {
            return Ok(None);
        };
        Ok(Some(self.session_from_commit(&commit)?))
    }

    fn list(&self) -> Result<Vec<SessionSummary>> {
        let conn = self.conn()?;
        let current_id = self.current_session_id().ok().flatten();
        let branch_refs = self.refs_by_kind("branch")?;
        let mut stmt = conn
            .prepare(
                "SELECT id, model, provider, title, parent_id, branch, created_at, updated_at, message_count
                 FROM sessions ORDER BY updated_at DESC, id DESC",
            )
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        let rows = stmt
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let branches = branch_refs
                    .iter()
                    .filter_map(|(name, session_id)| {
                        if session_id == &id {
                            Some(name.clone())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                Ok(SessionSummary {
                    is_current: current_id.as_deref() == Some(id.as_str()),
                    id,
                    model: row.get(1)?,
                    provider: row.get(2)?,
                    title: row.get(3)?,
                    parent_id: row.get(4)?,
                    branch: row.get(5)?,
                    branches,
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                    message_count: row.get::<_, i64>(8)? as usize,
                })
            })
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })
    }

    fn delete(&self, id: &str) -> Result<bool> {
        validate_session_id(id)?;
        let conn = self.conn()?;
        let was_current = self.current_session_id()?.as_deref() == Some(id);
        let changed = conn
            .execute("DELETE FROM sessions WHERE id = ?1", params![id])
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        self.prune_refs_to(id)?;
        if was_current {
            self.clear_head()?;
        }
        Ok(changed > 0)
    }

    fn latest(&self) -> Result<Option<Session>> {
        match self.list()?.into_iter().next() {
            Some(summary) => self.load(&summary.id),
            None => Ok(None),
        }
    }

    fn current(&self) -> Result<Option<Session>> {
        match self.current_session_id()? {
            Some(id) => self.load(&id),
            None => Ok(None),
        }
    }

    fn current_session_id(&self) -> Result<Option<String>> {
        let Some(head) = self.read_head()? else {
            return Ok(None);
        };
        if let Some(branch) = head.strip_prefix("ref: refs/heads/") {
            return self.read_ref("branch", branch.trim());
        }
        if head.trim().is_empty() {
            return Ok(None);
        }
        validate_session_id(head.trim())?;
        Ok(Some(head.trim().to_string()))
    }

    fn checkout(&self, target: &str) -> Result<Session> {
        if let Some(session_id) = self.read_ref("branch", target)? {
            let old_session_id = self.current_session_id()?;
            self.write_head(&format!("ref: refs/heads/{target}"))?;
            self.append_reflog(
                "HEAD",
                old_session_id.as_deref(),
                Some(&session_id),
                "checkout",
            )?;
            return self
                .load(&session_id)?
                .ok_or_else(|| SessionError::MissingBranchTarget {
                    branch: target.to_string(),
                    session_id,
                });
        }
        if let Some(commit) = self.read_commit(target)? {
            let old_session_id = self.current_session_id()?;
            self.write_head(&commit.session_id)?;
            self.append_reflog(
                "HEAD",
                old_session_id.as_deref(),
                Some(&commit.session_id),
                "checkout",
            )?;
            return match self.load(&commit.session_id)? {
                Some(session) => Ok(session),
                None => self.session_from_commit(&commit),
            };
        }
        let session_id = self.resolve_session_id(target)?;
        let old_session_id = self.current_session_id()?;
        self.write_head(&session_id)?;
        self.append_reflog(
            "HEAD",
            old_session_id.as_deref(),
            Some(&session_id),
            "checkout",
        )?;
        self.load(&session_id)?
            .ok_or(SessionError::NotFound(session_id))
    }

    fn create_branch(&self, name: &str, from: Option<&str>) -> Result<Session> {
        validate_branch_name(name)?;
        if self.read_ref("branch", name)?.is_some() {
            return Err(SessionError::BranchExists(name.to_string()));
        }
        let base = match from {
            Some(target) => {
                let id = self.target_session_id(target)?;
                self.load(&id)?.ok_or(SessionError::NotFound(id))?
            }
            None => self
                .current()?
                .or(self.latest()?)
                .ok_or(SessionError::NoSession)?,
        };
        let fork = Session::fork_from(&base, Some(name.to_string()));
        self.record_session(&fork)?;
        self.write_ref("branch", name, &fork.id)?;
        self.append_reflog(
            &format!("refs/heads/{name}"),
            None,
            Some(&fork.id),
            "branch",
        )?;
        Ok(fork)
    }

    fn branches(&self) -> Result<Vec<BranchSummary>> {
        let current_branch = self.current_branch_name()?;
        self.refs_by_kind("branch")?
            .into_iter()
            .map(|(name, session_id)| {
                let session = self.load(&session_id)?;
                Ok(BranchSummary {
                    is_current: current_branch.as_deref() == Some(name.as_str()),
                    name,
                    session_id,
                    title: session.as_ref().and_then(|s| s.title.clone()),
                    updated_at: session.as_ref().map(|s| s.updated_at.clone()),
                })
            })
            .collect()
    }

    fn log(&self, id: Option<&str>) -> Result<Vec<SessionTurnSummary>> {
        let session = match id {
            Some(id) => {
                let full_id = self.target_session_id(id)?;
                self.load(&full_id)?
                    .ok_or(SessionError::NotFound(full_id))?
            }
            None => self
                .current()?
                .or(self.latest()?)
                .ok_or(SessionError::NoSession)?,
        };
        Ok(turn_summaries(&session))
    }

    fn resolve_session_id(&self, target: &str) -> Result<String> {
        validate_session_id(target)?;
        if self.load(target)?.is_some() {
            return Ok(target.to_string());
        }
        let matches = self
            .list()?
            .into_iter()
            .filter(|s| s.id.starts_with(target))
            .map(|s| s.id)
            .collect::<Vec<_>>();
        match matches.len() {
            0 => Err(SessionError::NotFound(target.to_string())),
            1 => Ok(matches[0].clone()),
            _ => Err(SessionError::AmbiguousPrefix(target.to_string())),
        }
    }

    fn reflog(&self, target: Option<&str>) -> Result<Vec<ReflogEntry>> {
        let ref_name = match target {
            None => "HEAD".to_string(),
            Some("HEAD") => "HEAD".to_string(),
            Some(target) if self.read_ref("branch", target)?.is_some() => {
                format!("refs/heads/{target}")
            }
            Some(target) if self.read_ref("tag", target)?.is_some() => {
                format!("refs/tags/{target}")
            }
            Some(target) => target.to_string(),
        };
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT timestamp, ref_name, old_session_id, new_session_id, old_commit_id, new_commit_id, action
                 FROM reflog WHERE ref_name = ?1 ORDER BY seq ASC",
            )
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        let rows = stmt
            .query_map(params![ref_name], |row| {
                Ok(ReflogEntry {
                    timestamp: row.get(0)?,
                    ref_name: row.get(1)?,
                    old_session_id: row.get(2)?,
                    new_session_id: row.get(3)?,
                    old_commit_id: row.get(4)?,
                    new_commit_id: row.get(5)?,
                    action: row.get(6)?,
                })
            })
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })
    }

    fn reset(&self, target: &str) -> Result<Session> {
        let session_id = self.target_session_id(target)?;
        let session = self
            .load(&session_id)?
            .ok_or_else(|| SessionError::NotFound(session_id.clone()))?;
        self.advance_head(&session_id, "reset")?;
        Ok(session)
    }

    fn create_tag(&self, name: &str, target: Option<&str>) -> Result<TagSummary> {
        validate_branch_name(name)?;
        if self.read_ref("tag", name)?.is_some() {
            return Err(SessionError::TagExists(name.to_string()));
        }
        let session = match target {
            Some(target) => {
                let id = self.target_session_id(target)?;
                self.load(&id)?.ok_or(SessionError::NotFound(id))?
            }
            None => self
                .current()?
                .or(self.latest()?)
                .ok_or(SessionError::NoSession)?,
        };
        self.write_ref("tag", name, &session.id)?;
        self.append_reflog(&format!("refs/tags/{name}"), None, Some(&session.id), "tag")?;
        Ok(TagSummary {
            name: name.to_string(),
            session_id: session.id.clone(),
            commit_id: self.commit_id_for_session(&session.id)?,
            title: session.title,
            updated_at: Some(session.updated_at),
        })
    }

    fn tags(&self) -> Result<Vec<TagSummary>> {
        self.refs_by_kind("tag")?
            .into_iter()
            .map(|(name, session_id)| {
                let session = self.load(&session_id)?;
                Ok(TagSummary {
                    name,
                    commit_id: self.commit_id_for_session(&session_id)?,
                    session_id,
                    title: session.as_ref().and_then(|s| s.title.clone()),
                    updated_at: session.as_ref().map(|s| s.updated_at.clone()),
                })
            })
            .collect()
    }

    fn diff(&self, from: &str, to: &str) -> Result<SessionDiff> {
        let from_id = self.target_session_id(from)?;
        let to_id = self.target_session_id(to)?;
        let from_session = self
            .load(&from_id)?
            .ok_or_else(|| SessionError::NotFound(from_id.clone()))?;
        let to_session = self
            .load(&to_id)?
            .ok_or_else(|| SessionError::NotFound(to_id.clone()))?;
        Ok(session_diff(&from_session, &to_session))
    }

    fn cherry_pick(&self, source: &str, turns: &[usize]) -> Result<Session> {
        let source_id = self.target_session_id(source)?;
        let source = self
            .load(&source_id)?
            .ok_or_else(|| SessionError::NotFound(source_id.clone()))?;
        let mut current = self.current()?.ok_or(SessionError::NoSession)?;
        let picked = pick_turn_messages(&source, turns);
        if picked.is_empty() {
            return Ok(current);
        }
        current.parent_id = Some(current.id.clone());
        current.id = Uuid::new_v4().simple().to_string();
        current.messages.extend(picked);
        current.updated_at = now_rfc3339();
        current.normalize();
        self.save(&current)?;
        self.append_reflog(
            "HEAD",
            current.parent_id.as_deref(),
            Some(&current.id),
            "cherry-pick",
        )?;
        Ok(current)
    }

    fn merge(&self, target: &str) -> Result<MergeResult> {
        let target_id = self.target_session_id(target)?;
        let target = self
            .load(&target_id)?
            .ok_or_else(|| SessionError::NotFound(target_id.clone()))?;
        let mut current = self.current()?.ok_or(SessionError::NoSession)?;
        let base = common_prefix_len(&current.messages, &target.messages);
        let additions = target
            .messages
            .iter()
            .skip(base)
            .cloned()
            .collect::<Vec<_>>();
        if additions.is_empty() {
            return Ok(MergeResult {
                session: current,
                base_session_id: None,
                appended_messages: 0,
                conflicts: vec![],
            });
        }
        let old_id = current.id.clone();
        current.parent_id = Some(old_id.clone());
        current.id = Uuid::new_v4().simple().to_string();
        current.messages.extend(additions.clone());
        current.updated_at = now_rfc3339();
        current.normalize();
        self.save(&current)?;
        self.append_reflog("HEAD", Some(&old_id), Some(&current.id), "merge")?;
        Ok(MergeResult {
            session: current,
            base_session_id: Some(target.id),
            appended_messages: additions.len(),
            conflicts: vec![],
        })
    }

    fn rebase_onto(&self, target: &str) -> Result<Session> {
        let target_id = self.target_session_id(target)?;
        let target = self
            .load(&target_id)?
            .ok_or_else(|| SessionError::NotFound(target_id.clone()))?;
        let current = self.current()?.ok_or(SessionError::NoSession)?;
        let base = common_prefix_len(&target.messages, &current.messages);
        let replay = current
            .messages
            .iter()
            .skip(base)
            .cloned()
            .collect::<Vec<_>>();
        let old_id = current.id.clone();
        let mut rebased = Session::fork_from(&target, current.branch.clone());
        rebased.parent_id = Some(target.id.clone());
        rebased.messages.extend(replay);
        rebased.updated_at = now_rfc3339();
        rebased.normalize();
        self.save(&rebased)?;
        self.append_reflog("HEAD", Some(&old_id), Some(&rebased.id), "rebase")?;
        Ok(rebased)
    }

    fn stash_push(&self, message: Option<&str>) -> Result<StashEntry> {
        let session = self.current()?.ok_or(SessionError::NoSession)?;
        let entry = StashEntry {
            id: Uuid::new_v4().simple().to_string(),
            session_id: session.id.clone(),
            commit_id: self.commit_id_for_session(&session.id)?,
            message: message.map(str::to_string),
            title: session.title.clone(),
            created_at: now_rfc3339(),
            message_count: session.messages.len(),
        };
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO stash (id, session_id, commit_id, message, title, created_at, message_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                &entry.id,
                &entry.session_id,
                &entry.commit_id,
                &entry.message,
                &entry.title,
                &entry.created_at,
                entry.message_count as i64,
            ],
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;
        Ok(entry)
    }

    fn stash_list(&self) -> Result<Vec<StashEntry>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, session_id, commit_id, message, title, created_at, message_count
                 FROM stash ORDER BY created_at DESC",
            )
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        let rows = stmt
            .query_map([], |row| {
                Ok(StashEntry {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    commit_id: row.get(2)?,
                    message: row.get(3)?,
                    title: row.get(4)?,
                    created_at: row.get(5)?,
                    message_count: row.get::<_, i64>(6)? as usize,
                })
            })
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })
    }

    fn stash_apply(&self, id: &str) -> Result<Session> {
        validate_session_id(id)?;
        let conn = self.conn()?;
        let session_id: Option<String> = conn
            .query_row(
                "SELECT session_id FROM stash WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        let Some(session_id) = session_id else {
            return Err(SessionError::StashNotFound(id.to_string()));
        };
        self.checkout(&session_id)
    }

    fn gc(&self, prune: bool) -> Result<GcReport> {
        let reachable_sessions = self.reachable_session_ids()?;
        let (reachable_commits, reachable_messages, reachable_artifacts, reachable_manifests) =
            self.reachable_cas_ids()?;
        let all_sessions = self.list()?.len();
        let conn = self.conn()?;
        let mut pruned_objects = 0usize;
        if prune {
            let mut stmt = conn
                .prepare("SELECT id, kind, path FROM objects WHERE kind IN ('commits', 'messages', 'turn_manifests')")
                .map_err(|source| SessionError::Database {
                    path: self.db_path.clone(),
                    source,
                })?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(|source| SessionError::Database {
                    path: self.db_path.clone(),
                    source,
                })?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|source| SessionError::Database {
                    path: self.db_path.clone(),
                    source,
                })?;
            drop(stmt);
            for (id, kind, path) in rows {
                let reachable = match kind.as_str() {
                    "commits" => reachable_commits.contains(&id),
                    "messages" => reachable_messages.contains(&id),
                    "turn_manifests" => reachable_manifests.contains(&id),
                    _ => true,
                };
                if !reachable {
                    let path_buf = PathBuf::from(&path);
                    if path_buf.exists() {
                        remove_file(&path_buf)?;
                    }
                    conn.execute(
                        "DELETE FROM objects WHERE kind = ?1 AND id = ?2",
                        params![&kind, &id],
                    )
                    .map_err(|source| SessionError::Database {
                        path: self.db_path.clone(),
                        source,
                    })?;
                    if kind == "commits" {
                        conn.execute("DELETE FROM commits WHERE id = ?1", params![&id])
                            .map_err(|source| SessionError::Database {
                                path: self.db_path.clone(),
                                source,
                            })?;
                    }
                    if kind == "turn_manifests" {
                        conn.execute("DELETE FROM turn_manifests WHERE id = ?1", params![&id])
                            .map_err(|source| SessionError::Database {
                                path: self.db_path.clone(),
                                source,
                            })?;
                    }
                    pruned_objects += 1;
                }
            }
            let mut stmt = conn
                .prepare("SELECT id, object_path FROM artifacts")
                .map_err(|source| SessionError::Database {
                    path: self.db_path.clone(),
                    source,
                })?;
            let artifacts = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|source| SessionError::Database {
                    path: self.db_path.clone(),
                    source,
                })?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|source| SessionError::Database {
                    path: self.db_path.clone(),
                    source,
                })?;
            drop(stmt);
            for (id, path) in artifacts {
                if !reachable_artifacts.contains(&id) {
                    let path_buf = PathBuf::from(&path);
                    if path_buf.exists() {
                        remove_file(&path_buf)?;
                    }
                    conn.execute("DELETE FROM artifacts WHERE id = ?1", params![id])
                        .map_err(|source| SessionError::Database {
                            path: self.db_path.clone(),
                            source,
                        })?;
                    conn.execute(
                        "DELETE FROM objects WHERE kind = 'artifacts' AND id = ?1",
                        params![id],
                    )
                    .map_err(|source| SessionError::Database {
                        path: self.db_path.clone(),
                        source,
                    })?;
                    pruned_objects += 1;
                }
            }
        }
        Ok(GcReport {
            reachable_sessions: reachable_sessions.len(),
            unreachable_sessions: all_sessions.saturating_sub(reachable_sessions.len()),
            reachable_commits: reachable_commits.len(),
            reachable_messages: reachable_messages.len(),
            reachable_artifacts: reachable_artifacts.len(),
            pruned_objects,
            dry_run: !prune,
        })
    }

    fn migrate_legacy_sessions(&self) -> Result<MigrationReport> {
        let legacy = self.legacy_store();
        let sessions = legacy.list()?;
        let mut migrated_sessions = 0usize;
        for summary in &sessions {
            if self.load(&summary.id)?.is_none() {
                if let Some(session) = legacy.load(&summary.id)? {
                    self.save(&session)?;
                    migrated_sessions += 1;
                }
            }
        }
        Ok(MigrationReport {
            scanned_sessions: sessions.len(),
            migrated_sessions,
        })
    }

    fn put_artifact(&self, artifact: NewArtifact) -> Result<ArtifactObject> {
        let conn = self.conn()?;
        let content_hash = object_id("artifact_content", artifact.content.as_bytes());
        let payload = (
            artifact.kind.as_str(),
            &content_hash,
            &artifact.path,
            &artifact.mime,
            &artifact.content,
            &artifact.metadata,
        );
        let payload_json = stable_json("artifact", &payload)?;
        let id = object_id("artifact", payload_json.as_bytes());
        let object = ArtifactObject {
            id,
            kind: artifact.kind,
            content_hash,
            size: artifact.content.len(),
            content: artifact.content,
            path: artifact.path,
            mime: artifact.mime,
            created_at: now_rfc3339(),
            metadata: artifact.metadata,
        };
        let content = stable_json("artifact", &object)?;
        let object_path = self.write_cas_object(&conn, "artifacts", &object.id, &content)?;
        conn.execute(
            "INSERT OR REPLACE INTO artifacts
             (id, kind, content_hash, path, mime, size, created_at, metadata_json, object_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                &object.id,
                object.kind.as_str(),
                &object.content_hash,
                &object.path,
                &object.mime,
                object.size as i64,
                &object.created_at,
                stable_json("metadata", &object.metadata)?,
                object_path.to_string_lossy().to_string(),
            ],
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;
        Ok(object)
    }

    fn get_artifact(&self, id: &str) -> Result<Option<ArtifactObject>> {
        validate_session_id(id)?;
        let conn = self.conn()?;
        let path: Option<String> = conn
            .query_row(
                "SELECT object_path FROM artifacts WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        let Some(path) = path else {
            return Ok(None);
        };
        let path = PathBuf::from(path);
        let content = read_to_string(&path)?;
        serde_json::from_str(&content)
            .map(Some)
            .map_err(|source| SessionError::Json { path, source })
    }

    fn list_artifacts(&self) -> Result<Vec<ArtifactSummary>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, kind, path, mime, size, created_at
                 FROM artifacts ORDER BY created_at DESC",
            )
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        let rows = stmt
            .query_map([], |row| {
                let kind: String = row.get(1)?;
                Ok(ArtifactSummary {
                    id: row.get(0)?,
                    kind: ArtifactKind::from_str(&kind),
                    path: row.get(2)?,
                    mime: row.get(3)?,
                    size: row.get::<_, i64>(4)? as usize,
                    created_at: row.get(5)?,
                })
            })
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })
    }

    fn put_turn_manifest(&self, manifest: NewTurnManifest) -> Result<TurnManifestObject> {
        let conn = self.conn()?;
        self.put_turn_manifest_with_conn(&conn, manifest)
    }

    fn get_turn_manifest(&self, id: &str) -> Result<Option<TurnManifestObject>> {
        validate_session_id(id)?;
        let conn = self.conn()?;
        self.get_turn_manifest_with_conn(&conn, id)
    }

    fn list_turn_manifests(&self, session_id: &str) -> Result<Vec<TurnManifestSummary>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, session_id, turn_index, request_id, created_at
                 FROM turn_manifests WHERE session_id = ?1 ORDER BY turn_index ASC",
            )
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        let rows = stmt
            .query_map(params![session_id], |row| {
                Ok(TurnManifestSummary {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    turn_index: row.get::<_, i64>(2)? as usize,
                    request_id: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })
    }

    fn latest_turn_manifest_ids(&self, session_id: &str) -> Result<Vec<String>> {
        let Some(commit_id) = self.commit_id_for_session(session_id)? else {
            return Ok(Vec::new());
        };
        let Some(commit) = self.read_commit(&commit_id)? else {
            return Ok(Vec::new());
        };
        Ok(commit.turn_manifest_ids)
    }
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
        Err(SessionError::InvalidSessionId)
    }
}

fn validate_branch_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= MAX_BRANCH_NAME_LEN
        && !name.starts_with('-')
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.contains("..")
        && !name.contains("//")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '.'));
    if valid {
        Ok(())
    } else {
        Err(SessionError::InvalidBranchName)
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

fn turn_summaries(session: &Session) -> Vec<SessionTurnSummary> {
    let mut turns = Vec::new();
    let mut pending_user: Option<String> = None;
    for msg in &session.messages {
        match msg.role.to_ascii_lowercase().as_str() {
            "user" => pending_user = Some(msg.content.clone()),
            "assistant" => {
                if let Some(user) = pending_user.take() {
                    turns.push(SessionTurnSummary {
                        turn: turns.len() + 1,
                        user,
                        assistant: msg.content.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    turns
}

fn collect_branch_refs(root: &Path, dir: &Path, refs: &mut Vec<(String, String)>) -> Result<()> {
    for entry in read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_branch_refs(root, &path, refs)?;
            continue;
        }
        let name = path
            .strip_prefix(root)
            .map_err(|err| SessionError::Path(err.to_string()))?
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        validate_branch_name(&name)?;
        let id = read_to_string(&path)?.trim().to_string();
        validate_session_id(&id)?;
        refs.push((name, id));
    }
    Ok(())
}

fn stable_json<T: Serialize>(kind: &'static str, value: &T) -> Result<String> {
    serde_json::to_string_pretty(value)
        .map_err(|source| SessionError::SerializeObject { kind, source })
}

fn compact_json<T: Serialize>(kind: &'static str, value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|source| SessionError::SerializeObject { kind, source })
}

fn object_id(kind: &str, bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update(b"\0");
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Stable fields that define a manifest's logical identity.
///
/// Used to compute the CAS key.  Excludes:
/// - `id` (derived from this struct)
/// - `created_at` (set at write time)
/// - `commit_id` (reverse-index metadata, stored in SQL index only)
///
/// `schema_version` is included so that a schema bump produces a different
/// logical ID even when the payload content is byte-identical.
#[derive(Serialize)]
struct ManifestPayload<'a> {
    schema_version: u32,
    session_id: &'a str,
    turn_index: usize,
    request_id: &'a str,
    parent_manifest_ids: &'a [String],
    model: &'a str,
    provider: &'a str,
    prompt: &'a PromptSnapshot,
    hooks: &'a [HookDecisionRecord],
    tools: &'a [ToolDecisionRecord],
    metadata: &'a serde_json::Value,
}

/// Compute a deterministic manifest CAS key from the stable logical payload.
///
/// The key is `sha256("turn_manifest\0" || compact_json(payload))`.
fn turn_manifest_id(manifest: &NewTurnManifest) -> Result<String> {
    let payload = ManifestPayload {
        schema_version: TURN_MANIFEST_SCHEMA_VERSION,
        session_id: &manifest.session_id,
        turn_index: manifest.turn_index,
        request_id: &manifest.request_id,
        parent_manifest_ids: &manifest.parent_manifest_ids,
        model: &manifest.model,
        provider: &manifest.provider,
        prompt: &manifest.prompt,
        hooks: &manifest.hooks,
        tools: &manifest.tools,
        metadata: &manifest.metadata,
    };
    let json = compact_json("turn_manifest_payload", &payload)?;
    Ok(object_id("turn_manifest", json.as_bytes()))
}

fn common_prefix_len(left: &[SessionMessage], right: &[SessionMessage]) -> usize {
    left.iter()
        .zip(right)
        .take_while(|(left, right)| left == right)
        .count()
}

fn session_diff(from: &Session, to: &Session) -> SessionDiff {
    let common = common_prefix_len(&from.messages, &to.messages);
    let removed_messages = from
        .messages
        .iter()
        .skip(common)
        .cloned()
        .collect::<Vec<_>>();
    let added_messages = to.messages.iter().skip(common).cloned().collect::<Vec<_>>();
    let changed_messages = removed_messages.len().max(added_messages.len());
    SessionDiff {
        from: from.id.clone(),
        to: to.id.clone(),
        from_message_count: from.messages.len(),
        to_message_count: to.messages.len(),
        common_prefix_messages: common,
        removed_messages,
        added_messages,
        changed_messages,
    }
}

fn pick_turn_messages(session: &Session, turns: &[usize]) -> Vec<SessionMessage> {
    let requested = turns.iter().copied().collect::<BTreeSet<_>>();
    if requested.is_empty() {
        return Vec::new();
    }
    let mut picked = Vec::new();
    let mut current_turn = 0usize;
    let mut pending_user: Option<SessionMessage> = None;
    for msg in &session.messages {
        match msg.role.to_ascii_lowercase().as_str() {
            "user" => pending_user = Some(msg.clone()),
            "assistant" => {
                if let Some(user) = pending_user.take() {
                    current_turn += 1;
                    if requested.contains(&current_turn) {
                        picked.push(user);
                        picked.push(msg.clone());
                    }
                }
            }
            _ => {}
        }
    }
    picked
}

fn create_dir_all(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    fs::create_dir_all(path).map_err(|source| SessionError::Io {
        operation: "creating directory",
        path: path.to_path_buf(),
        source,
    })
}

fn read_to_string(path: impl AsRef<Path>) -> Result<String> {
    let path = path.as_ref();
    fs::read_to_string(path).map_err(|source| SessionError::Io {
        operation: "reading",
        path: path.to_path_buf(),
        source,
    })
}

fn write_file(path: impl AsRef<Path>, content: String) -> Result<()> {
    let path = path.as_ref();
    fs::write(path, content).map_err(|source| SessionError::Io {
        operation: "writing",
        path: path.to_path_buf(),
        source,
    })
}

fn append_line(path: impl AsRef<Path>, line: &str) -> Result<()> {
    let path = path.as_ref();
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| SessionError::Io {
            operation: "opening",
            path: path.to_path_buf(),
            source,
        })?;
    writeln!(file, "{line}").map_err(|source| SessionError::Io {
        operation: "writing",
        path: path.to_path_buf(),
        source,
    })
}

fn rename(from: impl AsRef<Path>, to: impl AsRef<Path>) -> Result<()> {
    let from = from.as_ref();
    let to = to.as_ref();
    fs::rename(from, to).map_err(|source| SessionError::Io {
        operation: "renaming",
        path: to.to_path_buf(),
        source,
    })
}

fn remove_file(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    fs::remove_file(path).map_err(|source| SessionError::Io {
        operation: "removing",
        path: path.to_path_buf(),
        source,
    })
}

fn read_dir(path: impl AsRef<Path>) -> Result<fs::ReadDir> {
    let path = path.as_ref();
    fs::read_dir(path).map_err(|source| SessionError::Io {
        operation: "reading directory",
        path: path.to_path_buf(),
        source,
    })
}

impl From<std::io::Error> for SessionError {
    fn from(source: std::io::Error) -> Self {
        SessionError::Io {
            operation: "accessing",
            path: PathBuf::new(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir().join("lattice-session-tests").join(name);
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
            parent_id: None,
            branch: None,
            messages: vec![
                SessionMessage {
                    role: "user".to_string(),
                    content: "hello".to_string(),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                    artifact_ids: Vec::new(),
                    compact: None,
                },
                SessionMessage {
                    role: "assistant".to_string(),
                    content: "hi".to_string(),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                    artifact_ids: Vec::new(),
                    compact: None,
                },
            ],
            created_at: timestamp.clone(),
            updated_at: timestamp,
        }
    }

    fn test_artifact(content: &str, path: Option<&str>) -> NewArtifact {
        NewArtifact {
            kind: ArtifactKind::FilePatch,
            content: content.to_string(),
            path: path.map(str::to_string),
            mime: Some("text/x-diff".to_string()),
            metadata: serde_json::json!({ "source": "test" }),
        }
    }

    #[test]
    fn save_and_load() -> Result<()> {
        let dir = temp_dir("save_and_load");
        let store = FileSessionStore::new(dir.clone());
        let session = test_session("save-load-1");
        store.save(&session)?;

        let loaded = store
            .load("save-load-1")?
            .expect("should load saved session");
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.title.as_deref(), Some("hello"));

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn list_sessions() -> Result<()> {
        let dir = temp_dir("list_sessions");
        let store = FileSessionStore::new(dir.clone());
        store.save(&test_session("list-a"))?;
        store.save(&test_session("list-b"))?;

        let summaries = store.list()?;
        assert_eq!(summaries.len(), 2);
        let ids: Vec<&str> = summaries.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"list-a"));
        assert!(ids.contains(&"list-b"));

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn delete_session_prunes_refs() -> Result<()> {
        let dir = temp_dir("delete_session_prunes_refs");
        let store = FileSessionStore::new(dir.clone());
        store.save(&test_session("delete-me"))?;
        assert_eq!(store.current_session_id()?.as_deref(), Some("delete-me"));

        assert!(store.delete("delete-me")?);
        assert!(store.load("delete-me")?.is_none());
        assert!(store.current_session_id()?.is_none());
        assert!(!store.delete("does-not-exist")?);

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn empty_dir_returns_empty_list() -> Result<()> {
        let dir = temp_dir("empty_dir_returns_empty_list");
        let store = FileSessionStore::new(dir.clone());

        assert!(store.list()?.is_empty());

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn latest_uses_updated_at() -> Result<()> {
        let dir = temp_dir("latest_uses_updated_at");
        let store = FileSessionStore::new(dir.clone());

        assert!(store.latest()?.is_none());
        let mut older = test_session("older");
        older.updated_at = "2026-05-01T00:00:00+08:00".into();
        let mut newer = test_session("newer");
        newer.updated_at = "2026-05-02T00:00:00+08:00".into();
        store.save(&older)?;
        store.save(&newer)?;

        assert_eq!(store.latest()?.unwrap().id, "newer");

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn append_turn_updates_existing_session() -> Result<()> {
        let dir = temp_dir("append_turn_updates_existing_session");
        let store = FileSessionStore::new(dir.clone());

        let mut session = test_session("thread-1");
        session.append_turn(
            "next-model".into(),
            "next-provider".into(),
            "follow up".into(),
            "answer".into(),
        );
        store.save(&session)?;

        let loaded = store.load("thread-1")?.expect("session should exist");
        assert_eq!(loaded.model, "next-model");
        assert_eq!(loaded.provider, "next-provider");
        assert_eq!(loaded.messages.len(), 4);

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn invalid_session_id_is_rejected() -> Result<()> {
        let dir = temp_dir("invalid_session_id_is_rejected");
        let store = FileSessionStore::new(dir.clone());

        assert!(store.load("../escape").is_err());
        assert!(store.delete("bad/id").is_err());

        let mut session = test_session("bad.id");
        assert!(store.save(&session).is_err());
        session.id = "good_id-1".into();
        assert!(store.save(&session).is_ok());

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn save_initializes_head_and_main_ref() -> Result<()> {
        let dir = temp_dir("save_initializes_head_and_main_ref");
        let store = FileSessionStore::new(dir.clone());
        store.save(&test_session("first"))?;

        assert_eq!(store.current_session_id()?.as_deref(), Some("first"));
        let branches = store.branches()?;
        assert_eq!(branches.len(), 1);
        assert_eq!(branches[0].name, DEFAULT_BRANCH);
        assert!(branches[0].is_current);

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn checkout_branch_and_advance_ref() -> Result<()> {
        let dir = temp_dir("checkout_branch_and_advance_ref");
        let store = FileSessionStore::new(dir.clone());
        store.save(&test_session("base"))?;
        let fork = store.create_branch("feature/test", None)?;
        assert_eq!(fork.parent_id.as_deref(), Some("base"));
        assert_eq!(fork.branch.as_deref(), Some("feature/test"));

        store.checkout("feature/test")?;
        assert_eq!(
            store.current_session_id()?.as_deref(),
            Some(fork.id.as_str())
        );

        let mut next = fork.clone();
        next.id = "feature-head".into();
        store.save(&next)?;
        assert_eq!(store.current_session_id()?.as_deref(), Some("feature-head"));
        assert_eq!(
            store.read_branch_ref("feature/test")?.as_deref(),
            Some("feature-head")
        );

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn checkout_detached_session_prefix() -> Result<()> {
        let dir = temp_dir("checkout_detached_session_prefix");
        let store = FileSessionStore::new(dir.clone());
        store.save(&test_session("abcdef123456"))?;
        let session = store.checkout("abcdef")?;

        assert_eq!(session.id, "abcdef123456");
        assert_eq!(store.current_session_id()?.as_deref(), Some("abcdef123456"));
        assert!(store.current_branch_name()?.is_none());

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn log_returns_turn_summaries() -> Result<()> {
        let dir = temp_dir("log_returns_turn_summaries");
        let store = FileSessionStore::new(dir.clone());
        let mut session = test_session("loggy");
        session.append_turn(
            "test-model".into(),
            "test-provider".into(),
            "second question".into(),
            "second answer".into(),
        );
        store.save(&session)?;

        let log = store.log(Some("loggy"))?;
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].turn, 1);
        assert_eq!(log[0].user, "hello");
        assert_eq!(log[1].assistant, "second answer");

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn save_records_commit_objects_and_reflog() -> Result<()> {
        let dir = temp_dir("save_records_commit_objects_and_reflog");
        let store = FileSessionStore::new(dir.clone());
        store.save(&test_session("object-session"))?;

        assert!(store.commits_dir().exists());
        assert!(store.messages_dir().exists());
        let commit_id = store
            .commit_id_for_session("object-session")?
            .expect("commit should exist");
        let commit = store.read_commit(&commit_id)?.expect("commit should load");
        let restored = store.session_from_commit(&commit)?;
        assert_eq!(restored.id, "object-session");
        assert_eq!(restored.messages.len(), 2);

        let reflog = store.reflog(None)?;
        assert_eq!(reflog.len(), 1);
        assert_eq!(reflog[0].action, "commit");

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn tag_reset_and_diff_work_on_refs() -> Result<()> {
        let dir = temp_dir("tag_reset_and_diff_work_on_refs");
        let store = FileSessionStore::new(dir.clone());
        let base = test_session("base");
        store.save(&base)?;
        store.create_tag("checkpoint", None)?;

        let mut next = base.clone();
        next.id = "next".into();
        next.append_turn(
            "test-model".into(),
            "test-provider".into(),
            "new question".into(),
            "new answer".into(),
        );
        store.save(&next)?;

        let diff = store.diff("checkpoint", "next")?;
        assert_eq!(diff.common_prefix_messages, 2);
        assert_eq!(diff.added_messages.len(), 2);

        let reset = store.reset("checkpoint")?;
        assert_eq!(reset.id, "base");
        assert_eq!(store.current_session_id()?.as_deref(), Some("base"));

        let tags = store.tags()?;
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name, "checkpoint");

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn cherry_pick_merge_rebase_and_stash_have_git_style_effects() -> Result<()> {
        let dir = temp_dir("cherry_pick_merge_rebase_and_stash_have_git_style_effects");
        let store = FileSessionStore::new(dir.clone());
        let mut base = test_session("base");
        base.append_turn(
            "test-model".into(),
            "test-provider".into(),
            "base q2".into(),
            "base a2".into(),
        );
        store.save(&base)?;
        store.create_branch("feature", None)?;

        let mut feature = store.checkout("feature")?;
        feature.id = "feature-head".into();
        feature.append_turn(
            "test-model".into(),
            "test-provider".into(),
            "feature q".into(),
            "feature a".into(),
        );
        store.save(&feature)?;

        store.checkout("main")?;
        let picked = store.cherry_pick("feature", &[3])?;
        assert!(picked.messages.iter().any(|m| m.content == "feature q"));

        let stash = store.stash_push(Some("before merge"))?;
        assert_eq!(store.stash_list()?.len(), 1);
        let applied = store.stash_apply(&stash.id)?;
        assert_eq!(applied.id, picked.id);

        let merged = store.merge("feature")?;
        assert!(merged.appended_messages <= feature.messages.len());

        let rebased = store.rebase_onto("feature")?;
        assert_eq!(rebased.parent_id.as_deref(), Some("feature-head"));

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn gc_and_migration_report_session_graph_state() -> Result<()> {
        let dir = temp_dir("gc_and_migration_report_session_graph_state");
        let store = FileSessionStore::new(dir.clone());
        store.save(&test_session("kept"))?;

        let report = store.migrate_legacy_sessions()?;
        assert_eq!(report.scanned_sessions, 1);
        assert_eq!(report.migrated_sessions, 0);

        let gc = store.gc(false)?;
        assert_eq!(gc.reachable_sessions, 1);
        assert_eq!(gc.reachable_commits, 1);
        assert!(gc.dry_run);

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn file_store_artifacts_round_trip_and_gc_by_message_reachability() -> Result<()> {
        let dir = temp_dir("file_store_artifacts_round_trip_and_gc_by_message_reachability");
        let store = FileSessionStore::new(dir.clone());
        let kept = store.put_artifact(test_artifact(
            "--- a/notes.txt\n+++ b/notes.txt\n@@\n-old\n+new\n",
            Some("notes.txt"),
        ))?;
        let orphan = store.put_artifact(test_artifact("orphan patch", Some("orphan.txt")))?;

        let mut session = test_session("artifact-session");
        session.messages[1].artifact_ids.push(kept.id.clone());
        store.save(&session)?;

        let loaded = store
            .get_artifact(&kept.id)?
            .expect("artifact should round-trip");
        assert_eq!(loaded.content, kept.content);
        assert_eq!(store.list_artifacts()?.len(), 2);

        let dry_run = store.gc(false)?;
        assert_eq!(dry_run.reachable_artifacts, 1);
        assert_eq!(dry_run.pruned_objects, 0);

        let pruned = store.gc(true)?;
        assert_eq!(pruned.reachable_artifacts, 1);
        assert!(pruned.pruned_objects >= 1);
        assert!(store.get_artifact(&kept.id)?.is_some());
        assert!(store.get_artifact(&orphan.id)?.is_none());

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn file_store_gc_prunes_unreachable_commit_and_message_objects() -> Result<()> {
        let dir = temp_dir("file_store_gc_prunes_unreachable_commit_and_message_objects");
        let store = FileSessionStore::new(dir.clone());
        store.save(&test_session("kept"))?;
        store.create_tag("keep", Some("kept"))?;

        let mut orphan = test_session("orphan");
        orphan.messages[0].content = "unique orphan user".to_string();
        orphan.messages[1].content = "unique orphan assistant".to_string();
        store.save(&orphan)?;
        let orphan_commit_id = store
            .commit_id_for_session("orphan")?
            .expect("orphan commit should exist");
        let orphan_commit = store
            .read_commit(&orphan_commit_id)?
            .expect("orphan commit should load");
        let orphan_message_paths = orphan_commit
            .message_ids
            .iter()
            .map(|id| store.messages_dir().join(format!("{id}.json")))
            .collect::<Vec<_>>();

        assert!(store.delete("orphan")?);
        let pruned = store.gc(true)?;

        assert!(pruned.pruned_objects >= 3);
        assert!(!store
            .commits_dir()
            .join(format!("{orphan_commit_id}.json"))
            .exists());
        assert!(orphan_message_paths.iter().all(|path| !path.exists()));
        assert!(store.load("kept")?.is_some());

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn sqlite_cas_store_persists_sessions_refs_and_objects() -> Result<()> {
        let dir = temp_dir("sqlite_cas_store_persists_sessions_refs_and_objects");
        let store = SqliteCasSessionStore::new(dir.clone());
        store.save(&test_session("sqlite-first"))?;

        assert!(store.db_path().exists());
        assert!(dir.join("objects").join("messages").exists());
        assert!(dir.join("objects").join("commits").exists());
        assert_eq!(store.current_session_id()?.as_deref(), Some("sqlite-first"));

        let commit_id = store
            .commit_id_for_session("sqlite-first")?
            .expect("commit should exist");
        let commit = store.read_commit(&commit_id)?.expect("commit should load");
        assert_eq!(commit.session_id, "sqlite-first");

        let loaded = store.load("sqlite-first")?.expect("session should load");
        assert_eq!(loaded.messages.len(), 2);

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn sqlite_cas_store_migrates_legacy_json_sessions() -> Result<()> {
        let dir = temp_dir("sqlite_cas_store_migrates_legacy_json_sessions");
        let legacy = FileSessionStore::new(dir.clone());
        legacy.save(&test_session("legacy-one"))?;

        let store = SqliteCasSessionStore::new(dir.clone());
        let report = store.migrate_legacy_sessions()?;
        assert_eq!(report.scanned_sessions, 1);
        assert_eq!(report.migrated_sessions, 1);
        assert!(store.load("legacy-one")?.is_some());
        assert!(store.commit_id_for_session("legacy-one")?.is_some());

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn sqlite_cas_store_artifacts_round_trip_and_gc_by_message_reachability() -> Result<()> {
        let dir = temp_dir("sqlite_cas_store_artifacts_round_trip_and_gc_by_message_reachability");
        let store = SqliteCasSessionStore::new(dir.clone());
        let kept = store.put_artifact(test_artifact(
            "--- a/main.rs\n+++ b/main.rs\n@@\n-false\n+true\n",
            Some("main.rs"),
        ))?;
        let orphan = store.put_artifact(test_artifact("orphan sqlite patch", Some("old.rs")))?;

        let mut session = test_session("sqlite-artifact-session");
        session.messages[1].artifact_ids.push(kept.id.clone());
        store.save(&session)?;

        assert_eq!(
            store
                .get_artifact(&kept.id)?
                .expect("artifact should load")
                .content,
            kept.content
        );
        assert_eq!(store.list_artifacts()?.len(), 2);

        let dry_run = store.gc(false)?;
        assert_eq!(dry_run.reachable_artifacts, 1);
        assert_eq!(dry_run.pruned_objects, 0);

        let pruned = store.gc(true)?;
        assert_eq!(pruned.reachable_artifacts, 1);
        assert!(pruned.pruned_objects >= 1);
        assert!(store.get_artifact(&kept.id)?.is_some());
        assert!(store.get_artifact(&orphan.id)?.is_none());

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn sqlite_cas_store_gc_prunes_unreachable_commit_and_message_objects() -> Result<()> {
        let dir = temp_dir("sqlite_cas_store_gc_prunes_unreachable_commit_and_message_objects");
        let store = SqliteCasSessionStore::new(dir.clone());
        store.save(&test_session("sqlite-kept"))?;
        store.create_tag("keep", Some("sqlite-kept"))?;

        let mut orphan = test_session("sqlite-orphan");
        orphan.messages[0].content = "unique sqlite orphan user".to_string();
        orphan.messages[1].content = "unique sqlite orphan assistant".to_string();
        store.save(&orphan)?;
        let orphan_commit_id = store
            .commit_id_for_session("sqlite-orphan")?
            .expect("orphan commit should exist");
        let orphan_commit = store
            .read_commit(&orphan_commit_id)?
            .expect("orphan commit should load");
        let orphan_message_ids = orphan_commit.message_ids.clone();

        assert!(store.delete("sqlite-orphan")?);
        let pruned = store.gc(true)?;

        assert!(pruned.pruned_objects >= 3);
        assert!(store.read_commit(&orphan_commit_id)?.is_none());
        for id in orphan_message_ids {
            assert!(store.read_message_object(&id)?.is_none());
        }
        assert!(store.load("sqlite-kept")?.is_some());

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn sqlite_cas_store_supports_git_style_operations() -> Result<()> {
        let dir = temp_dir("sqlite_cas_store_supports_git_style_operations");
        let store = SqliteCasSessionStore::new(dir.clone());
        let mut base = test_session("sqlite-base");
        base.append_turn(
            "test-model".into(),
            "test-provider".into(),
            "base q2".into(),
            "base a2".into(),
        );
        store.save(&base)?;
        store.create_tag("checkpoint", None)?;
        let fork = store.create_branch("feature/sqlite", None)?;
        assert_eq!(fork.parent_id.as_deref(), Some("sqlite-base"));

        let mut feature = store.checkout("feature/sqlite")?;
        feature.id = "sqlite-feature".into();
        feature.append_turn(
            "test-model".into(),
            "test-provider".into(),
            "feature q".into(),
            "feature a".into(),
        );
        store.save(&feature)?;

        let diff = store.diff("checkpoint", "sqlite-feature")?;
        assert_eq!(diff.added_messages.len(), 2);
        assert!(!store.reflog(None)?.is_empty());

        let stash = store.stash_push(Some("sqlite stash"))?;
        assert_eq!(store.stash_list()?.len(), 1);
        assert_eq!(store.stash_apply(&stash.id)?.id, "sqlite-feature");

        let gc = store.gc(false)?;
        assert!(gc.reachable_commits >= 1);

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    fn test_manifest(session_id: &str, request_id: &str) -> NewTurnManifest {
        NewTurnManifest {
            session_id: session_id.to_string(),
            commit_id: None,
            turn_index: 0,
            request_id: request_id.to_string(),
            parent_manifest_ids: Vec::new(),
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            prompt: PromptSnapshot {
                total_tokens: 100,
                effective_context_budget: 4096,
                rendered_fragments: Vec::new(),
                withheld_fragments: Vec::new(),
            },
            hooks: Vec::new(),
            tools: Vec::new(),
            metadata: serde_json::Value::Null,
        }
    }

    fn test_profile_snapshot(source_fingerprint: &str) -> ProfileSnapshot {
        ProfileSnapshot::from_draft(ProfileSnapshotDraft {
            source: ProfileSourceAudit::new("agent.toml", Some(source_fingerprint)),
            canonical_spec: serde_json::json!({
                "profile_name": "default",
                "model": "test-model",
                "tools": {"bundles": ["filesystem"], "enabled": ["shell"]},
                "security": {"sandbox_mode": "workspace-write"},
                "context": {"project_instructions": true, "workspace_context": false},
                "behavior": {"reasoning_effort": "medium"}
            }),
            summary: ProfileSnapshotSummary {
                profile_name: "default".to_string(),
                source_label: "agent.toml".to_string(),
                model: "test-model".to_string(),
                tool_bundles: vec!["filesystem".to_string()],
                enabled_tools: vec!["shell".to_string()],
                security_mode: Some("workspace-write".to_string()),
                context: serde_json::json!({
                    "project_instructions": true,
                    "workspace_context": false
                }),
                behavior: serde_json::json!({"reasoning_effort": "medium"}),
            },
            captured_at: Some("2026-05-14T00:00:00Z".to_string()),
        })
        .expect("profile snapshot should be valid")
    }

    fn test_messages(user: &str, assistant: &str) -> Vec<SessionMessage> {
        vec![
            SessionMessage {
                role: "user".to_string(),
                content: user.to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
                artifact_ids: Vec::new(),
                compact: None,
            },
            SessionMessage {
                role: "assistant".to_string(),
                content: assistant.to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
                artifact_ids: Vec::new(),
                compact: None,
            },
        ]
    }

    #[test]
    fn open_turn_binds_profile_snapshot_for_new_session() -> Result<()> {
        let dir = temp_dir("open_turn_binds_profile_snapshot");
        let store = SqliteCasSessionStore::new(dir.clone());
        let snapshot = test_profile_snapshot("fingerprint-a");

        let opened = store.open_turn(OpenTurnRequest {
            session_id: Some("open-turn-session".to_string()),
            profile_snapshot: snapshot.clone(),
            current_source_fingerprint: Some("fingerprint-a".to_string()),
        })?;

        assert_eq!(opened.session_id, "open-turn-session");
        assert_eq!(opened.base_commit_id, None);
        assert_eq!(opened.turn_index, 0);
        assert_eq!(opened.profile_snapshot.hash, snapshot.hash);
        assert!(opened.profile_drift.is_none());
        assert!(opened.history.is_empty());
        assert!(opened.parent_manifest_ids.is_empty());

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn open_turn_reuses_locked_snapshot_and_reports_drift() -> Result<()> {
        let dir = temp_dir("open_turn_reuses_locked_snapshot");
        let store = SqliteCasSessionStore::new(dir.clone());
        let snapshot = test_profile_snapshot("fingerprint-a");
        let first = store.open_turn(OpenTurnRequest {
            session_id: Some("drift-session".to_string()),
            profile_snapshot: snapshot.clone(),
            current_source_fingerprint: Some("fingerprint-a".to_string()),
        })?;
        let committed = store.commit_turn(CommitTurnRequest {
            session_id: first.session_id.clone(),
            turn_id: first.turn_id.clone(),
            base_commit_id: first.base_commit_id.clone(),
            profile_hash: first.profile_snapshot.hash.clone(),
            status: TurnOutcomeStatus::Completed,
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            messages: test_messages("hello", "hi"),
            manifests: vec![test_manifest(&first.session_id, "req-drift-1")],
        })?;

        let changed_snapshot = test_profile_snapshot("fingerprint-b");
        let second = store.open_turn(OpenTurnRequest {
            session_id: Some("drift-session".to_string()),
            profile_snapshot: changed_snapshot,
            current_source_fingerprint: Some("fingerprint-b".to_string()),
        })?;

        assert_eq!(
            second.base_commit_id.as_deref(),
            Some(committed.commit_id.as_str())
        );
        assert_eq!(second.profile_snapshot.hash, snapshot.hash);
        assert_eq!(second.history.len(), 2);
        assert_eq!(second.parent_manifest_ids, committed.turn_manifest_ids);
        let drift = second.profile_drift.expect("changed source should drift");
        assert_eq!(drift.locked_hash, snapshot.hash);
        assert!(drift.warning.contains(&snapshot.hash));

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn open_turn_derives_next_index_from_manifests_not_message_pairs() -> Result<()> {
        let dir = temp_dir("open_turn_derives_next_index_from_manifests");
        let store = SqliteCasSessionStore::new(dir.clone());
        let snapshot = test_profile_snapshot("fingerprint-a");
        let first = store.open_turn(OpenTurnRequest {
            session_id: Some("manifest-index-session".to_string()),
            profile_snapshot: snapshot.clone(),
            current_source_fingerprint: Some("fingerprint-a".to_string()),
        })?;
        let mut messages = test_messages("hello", "hi");
        messages.insert(
            0,
            SessionMessage {
                role: "system".to_string(),
                content: "system context".to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
                artifact_ids: Vec::new(),
                compact: None,
            },
        );
        messages.push(SessionMessage {
            role: "tool".to_string(),
            content: "tool output".to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some("call-1".to_string()),
            name: Some("read_file".to_string()),
            artifact_ids: Vec::new(),
            compact: None,
        });
        let committed = store.commit_turn(CommitTurnRequest {
            session_id: first.session_id.clone(),
            turn_id: first.turn_id.clone(),
            base_commit_id: first.base_commit_id.clone(),
            profile_hash: first.profile_snapshot.hash.clone(),
            status: TurnOutcomeStatus::Completed,
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            messages,
            manifests: vec![test_manifest(&first.session_id, "req-index-1")],
        })?;

        let second = store.open_turn(OpenTurnRequest {
            session_id: Some(first.session_id.clone()),
            profile_snapshot: snapshot,
            current_source_fingerprint: Some("fingerprint-a".to_string()),
        })?;

        assert_eq!(second.history.len(), 4);
        assert_eq!(second.parent_manifest_ids, committed.turn_manifest_ids);
        assert_eq!(second.turn_index, 1);

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn commit_turn_rejects_stale_base_commit() -> Result<()> {
        let dir = temp_dir("commit_turn_rejects_stale_base_commit");
        let store = SqliteCasSessionStore::new(dir.clone());
        let opened = store.open_turn(OpenTurnRequest {
            session_id: Some("stale-base-session".to_string()),
            profile_snapshot: test_profile_snapshot("fingerprint-a"),
            current_source_fingerprint: Some("fingerprint-a".to_string()),
        })?;
        let _ = store.commit_turn(CommitTurnRequest {
            session_id: opened.session_id.clone(),
            turn_id: opened.turn_id.clone(),
            base_commit_id: opened.base_commit_id.clone(),
            profile_hash: opened.profile_snapshot.hash.clone(),
            status: TurnOutcomeStatus::Completed,
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            messages: test_messages("hello", "hi"),
            manifests: vec![test_manifest(&opened.session_id, "req-stale-1")],
        })?;

        let err = store
            .commit_turn(CommitTurnRequest {
                session_id: opened.session_id.clone(),
                turn_id: Uuid::new_v4().simple().to_string(),
                base_commit_id: opened.base_commit_id,
                profile_hash: opened.profile_snapshot.hash,
                status: TurnOutcomeStatus::Completed,
                model: "test-model".to_string(),
                provider: "test-provider".to_string(),
                messages: test_messages("again", "done"),
                manifests: vec![test_manifest(&opened.session_id, "req-stale-2")],
            })
            .expect_err("stale base must fail");
        assert!(matches!(err, SessionError::Conflict(_)));

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn commit_turn_rejects_profile_hash_mismatch() -> Result<()> {
        let dir = temp_dir("commit_turn_rejects_profile_hash_mismatch");
        let store = SqliteCasSessionStore::new(dir.clone());
        let opened = store.open_turn(OpenTurnRequest {
            session_id: Some("wrong-profile-session".to_string()),
            profile_snapshot: test_profile_snapshot("fingerprint-a"),
            current_source_fingerprint: Some("fingerprint-a".to_string()),
        })?;

        let err = store
            .commit_turn(CommitTurnRequest {
                session_id: opened.session_id.clone(),
                turn_id: opened.turn_id,
                base_commit_id: opened.base_commit_id,
                profile_hash: "not-the-locked-hash".to_string(),
                status: TurnOutcomeStatus::Completed,
                model: "test-model".to_string(),
                provider: "test-provider".to_string(),
                messages: test_messages("hello", "hi"),
                manifests: vec![test_manifest(&opened.session_id, "req-wrong-profile")],
            })
            .expect_err("wrong profile hash must fail");
        assert!(matches!(err, SessionError::Conflict(_)));
        assert!(store.load(&opened.session_id)?.is_none());

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn commit_turn_writes_completed_error_and_cancelled_statuses() -> Result<()> {
        let dir = temp_dir("commit_turn_writes_statuses");
        let store = SqliteCasSessionStore::new(dir.clone());
        let statuses = [
            (TurnOutcomeStatus::Completed, "completed"),
            (TurnOutcomeStatus::Error, "error"),
            (TurnOutcomeStatus::Cancelled, "cancelled"),
        ];

        for (index, (status, expected)) in statuses.into_iter().enumerate() {
            let session_id = format!("status-session-{index}");
            let opened = store.open_turn(OpenTurnRequest {
                session_id: Some(session_id.clone()),
                profile_snapshot: test_profile_snapshot("fingerprint-a"),
                current_source_fingerprint: Some("fingerprint-a".to_string()),
            })?;
            let committed = store.commit_turn(CommitTurnRequest {
                session_id: session_id.clone(),
                turn_id: opened.turn_id,
                base_commit_id: opened.base_commit_id,
                profile_hash: opened.profile_snapshot.hash.clone(),
                status,
                model: "test-model".to_string(),
                provider: "test-provider".to_string(),
                messages: test_messages("hello", "hi"),
                manifests: vec![test_manifest(&session_id, &format!("req-status-{index}"))],
            })?;
            let loaded = store
                .get_turn_manifest(&committed.turn_manifest_ids[0])?
                .expect("manifest should exist");
            assert_eq!(loaded.metadata["turn_status"], expected);
            assert_eq!(
                loaded.metadata["profile_snapshot"]["hash"],
                opened.profile_snapshot.hash
            );
        }

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn commit_turn_manifest_metadata_excludes_secret_fields() -> Result<()> {
        let dir = temp_dir("commit_turn_metadata_excludes_secrets");
        let store = SqliteCasSessionStore::new(dir.clone());
        let mut snapshot = test_profile_snapshot("fingerprint-a");
        snapshot.summary.context = serde_json::json!({
            "project_instructions": true,
            "api_key": "secret",
            "credentials": {"OPENAI_API_KEY": "secret"},
            "env": {"ANTHROPIC_API_KEY": "secret"}
        });
        snapshot.canonical_spec = serde_json::json!({
            "profile_name": "default",
            "model": "test-model",
            "authorization": "Bearer secret",
            "tools": {"bundles": ["filesystem"]}
        });
        snapshot = ProfileSnapshot::from_draft(ProfileSnapshotDraft {
            source: snapshot.source,
            canonical_spec: snapshot.canonical_spec,
            summary: snapshot.summary,
            captured_at: Some(snapshot.captured_at),
        })?;
        let opened = store.open_turn(OpenTurnRequest {
            session_id: Some("secret-metadata-session".to_string()),
            profile_snapshot: snapshot,
            current_source_fingerprint: Some("fingerprint-a".to_string()),
        })?;
        let mut manifest = test_manifest(&opened.session_id, "req-secret-metadata");
        manifest.metadata = serde_json::json!({
            "safe": true,
            "apiKey": "secret",
            "authorization": "Bearer secret",
            "nested": {
                "credentials": {"api_key": "secret"},
                "safe": "ok"
            }
        });

        let committed = store.commit_turn(CommitTurnRequest {
            session_id: opened.session_id.clone(),
            turn_id: opened.turn_id,
            base_commit_id: opened.base_commit_id,
            profile_hash: opened.profile_snapshot.hash,
            status: TurnOutcomeStatus::Completed,
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            messages: test_messages("hello", "hi"),
            manifests: vec![manifest],
        })?;
        let loaded = store
            .get_turn_manifest(&committed.turn_manifest_ids[0])?
            .expect("manifest should exist");
        assert!(!turn_tx::contains_secret_key(&loaded.metadata));
        assert_eq!(loaded.metadata["safe"], true);
        assert_eq!(loaded.metadata["nested"]["safe"], "ok");

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn sqlite_save_with_manifests_round_trips() -> Result<()> {
        let dir = temp_dir("sqlite_save_with_manifests_round_trips");
        let store = SqliteCasSessionStore::new(dir.clone());
        let session = test_session("manifest-session");
        let manifest = test_manifest(&session.id, "req-1");

        store.save_with_manifests(&session, vec![manifest])?;

        // list_turn_manifests returns the saved manifest
        let summaries = store.list_turn_manifests(&session.id)?;
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].session_id, session.id);
        assert_eq!(summaries[0].request_id, "req-1");

        // commit references the manifest id
        let commit_id = store.commit_id_for_session(&session.id)?.unwrap();
        let commit = store.read_commit(&commit_id)?.unwrap();
        assert_eq!(commit.turn_manifest_ids.len(), 1);
        assert_eq!(commit.turn_manifest_ids[0], summaries[0].id);

        // get_turn_manifest reads back full object plus reverse-index metadata
        let loaded = store.get_turn_manifest(&summaries[0].id)?.unwrap();
        assert_eq!(loaded.session_id, session.id);
        assert_eq!(loaded.request_id, "req-1");
        assert_eq!(loaded.prompt.total_tokens, 100);
        assert_eq!(loaded.commit_id.as_deref(), Some(commit_id.as_str()));

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn sqlite_save_with_manifests_uses_final_commit_for_reverse_index() -> Result<()> {
        let dir = temp_dir("sqlite_save_with_manifests_uses_final_commit");
        let store = SqliteCasSessionStore::new(dir.clone());
        let session = test_session("final-commit-manifest-session");
        let mut manifest = test_manifest(&session.id, "req-final-commit");
        manifest.commit_id = Some("stale-caller-commit".to_string());

        store.save_with_manifests(&session, vec![manifest])?;

        let commit_id = store.commit_id_for_session(&session.id)?.unwrap();
        let commit = store.read_commit(&commit_id)?.unwrap();
        let loaded = store
            .get_turn_manifest(&commit.turn_manifest_ids[0])?
            .unwrap();
        assert_eq!(loaded.commit_id.as_deref(), Some(commit_id.as_str()));

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn sqlite_save_with_manifests_commit_references_valid_manifests() -> Result<()> {
        let dir = temp_dir("sqlite_manifest_commit_integrity");
        let store = SqliteCasSessionStore::new(dir.clone());
        let session = test_session("integrity-session");
        let manifest = test_manifest(&session.id, "req-integrity");

        store.save_with_manifests(&session, vec![manifest])?;

        // Every manifest ID in the commit must resolve to an actual manifest
        let commit_id = store.commit_id_for_session(&session.id)?.unwrap();
        let commit = store.read_commit(&commit_id)?.unwrap();
        for id in &commit.turn_manifest_ids {
            assert!(
                store.get_turn_manifest(id)?.is_some(),
                "commit references a missing turn manifest"
            );
        }

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn sqlite_put_turn_manifest_is_idempotent() -> Result<()> {
        let dir = temp_dir("sqlite_put_turn_manifest_is_idempotent");
        let store = SqliteCasSessionStore::new(dir.clone());
        let manifest = test_manifest("idempotent-session", "req-idempotent");

        let first = store.put_turn_manifest(manifest.clone())?;
        let second = store.put_turn_manifest(manifest)?;

        assert_eq!(first.id, second.id);
        assert_eq!(first.created_at, second.created_at);

        let summaries = store.list_turn_manifests("idempotent-session")?;
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, first.id);

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn turn_manifest_id_ignores_commit_id() -> Result<()> {
        let mut first = test_manifest("same-session", "same-request");
        let mut second = first.clone();
        first.commit_id = Some("commit-a".to_string());
        second.commit_id = Some("commit-b".to_string());

        assert_eq!(turn_manifest_id(&first)?, turn_manifest_id(&second)?);
        Ok(())
    }

    #[test]
    fn sqlite_turn_manifest_cas_object_excludes_index_metadata() -> Result<()> {
        let dir = temp_dir("sqlite_turn_manifest_cas_object_excludes_index_metadata");
        let store = SqliteCasSessionStore::new(dir.clone());
        let mut manifest = test_manifest("cas-payload-session", "req-cas-payload");
        manifest.commit_id = Some("commit-a".to_string());

        let object = store.put_turn_manifest(manifest)?;
        let conn = store.conn()?;
        let object_path: String = conn
            .query_row(
                "SELECT path FROM objects WHERE kind = 'turn_manifests' AND id = ?1",
                params![&object.id],
                |row| row.get(0),
            )
            .map_err(|source| SessionError::Database {
                path: store.db_path().to_path_buf(),
                source,
            })?;
        let content = read_to_string(object_path)?;
        let value: serde_json::Value =
            serde_json::from_str(&content).expect("stored manifest should be valid JSON");

        assert!(value.get("commit_id").is_none());
        assert!(value.get("created_at").is_none());

        let loaded = store.get_turn_manifest(&object.id)?.unwrap();
        assert_eq!(loaded.commit_id.as_deref(), Some("commit-a"));
        assert_eq!(loaded.created_at, object.created_at);

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn sqlite_reused_manifest_keeps_first_reverse_index_binding() -> Result<()> {
        let dir = temp_dir("sqlite_reused_manifest_keeps_first_reverse_index_binding");
        let store = SqliteCasSessionStore::new(dir.clone());

        let first_session = test_session("first-manifest-session");
        let second_session = test_session("second-manifest-session");
        let manifest = test_manifest("shared-manifest-session", "req-shared");

        store.save_with_manifests(&first_session, vec![manifest.clone()])?;
        let first_commit_id = store.commit_id_for_session(&first_session.id)?.unwrap();
        store.save_with_manifests(&second_session, vec![manifest])?;
        let second_commit_id = store.commit_id_for_session(&second_session.id)?.unwrap();

        let first_commit = store.read_commit(&first_commit_id)?.unwrap();
        let second_commit = store.read_commit(&second_commit_id)?.unwrap();
        assert_eq!(
            first_commit.turn_manifest_ids,
            second_commit.turn_manifest_ids
        );

        let loaded = store
            .get_turn_manifest(&first_commit.turn_manifest_ids[0])?
            .unwrap();
        assert_eq!(loaded.commit_id.as_deref(), Some(first_commit_id.as_str()));

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn sqlite_gc_prunes_unreachable_manifests() -> Result<()> {
        let dir = temp_dir("sqlite_gc_prunes_manifests");
        let store = SqliteCasSessionStore::new(dir.clone());

        // Save a session with a manifest
        let session = test_session("gc-manifest-session");
        let manifest = test_manifest(&session.id, "req-gc");
        store.save_with_manifests(&session, vec![manifest])?;

        let summaries = store.list_turn_manifests(&session.id)?;
        assert_eq!(summaries.len(), 1);
        let manifest_id = summaries[0].id.clone();

        // Create a new session without manifests (orphaning the old one)
        let mut new_session = test_session("gc-new-session");
        new_session.parent_id = None;
        store.save(&new_session)?;

        // GC with prune=true should remove the orphaned manifest
        let gc = store.gc(true)?;
        assert!(gc.pruned_objects > 0);

        // The orphaned manifest should be gone
        assert!(store.get_turn_manifest(&manifest_id)?.is_none());
        assert!(store.list_turn_manifests(&session.id)?.is_empty());

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn sqlite_migrates_existing_commits_table_for_manifest_ids() {
        let dir = temp_dir("sqlite_migrates_commits");
        // Create a database with the old schema (no turn_manifest_ids_json)
        {
            let db_path = dir.join("session.db");
            fs::create_dir_all(&dir).unwrap();
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS commits (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    parents_json TEXT NOT NULL,
                    parent_session_ids_json TEXT NOT NULL,
                    message_ids_json TEXT NOT NULL,
                    model TEXT NOT NULL,
                    provider TEXT NOT NULL,
                    title TEXT,
                    branch TEXT,
                    created_at TEXT NOT NULL,
                    committed_at TEXT NOT NULL,
                    object_path TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS objects (
                    id TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    path TEXT NOT NULL,
                    size INTEGER NOT NULL,
                    created_at TEXT NOT NULL,
                    PRIMARY KEY (kind, id)
                );
                CREATE TABLE IF NOT EXISTS meta (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS sessions (
                    id TEXT PRIMARY KEY,
                    model TEXT NOT NULL,
                    provider TEXT NOT NULL,
                    title TEXT,
                    parent_id TEXT,
                    branch TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    message_count INTEGER NOT NULL,
                    commit_id TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS refs (
                    name TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    PRIMARY KEY (kind, name)
                );
                ",
            )
            .unwrap();
        }

        // Opening with SqliteCasSessionStore should trigger migration
        let store = SqliteCasSessionStore::new(dir.clone());
        let session = test_session("migrate-session");
        store.save(&session).unwrap();

        // Verify the column was added
        let conn = store.conn().unwrap();
        let has_column: bool = conn
            .prepare("PRAGMA table_info(commits)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .any(|name| name == "turn_manifest_ids_json");
        assert!(
            has_column,
            "turn_manifest_ids_json column should exist after migration"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sqlite_manifest_metadata_round_trips() -> Result<()> {
        let dir = temp_dir("sqlite_manifest_metadata_round_trips");
        let store = SqliteCasSessionStore::new(dir.clone());
        let session = test_session("metadata-roundtrip-session");

        let task_meta = serde_json::json!({
            "todos": [
                {"id": 1, "content": "Fix auth bug", "status": "in_progress"},
                {"id": 2, "content": "Add tests", "status": "pending"},
                {"id": 3, "content": "Review", "status": "pending"}
            ],
            "changed_files": [
                {"path": "src/auth.rs", "operation": "update", "diagnostic": null},
                {"path": "src/lib.rs", "operation": "add", "diagnostic": "new pub fn"}
            ],
            "tool_failures": [
                {"call_id": "tc-1", "tool_name": "bash", "message": "command failed", "timestamp": "2026-05-12T00:00:00Z"}
            ],
            "verifications": [
                {"command": "cargo test", "output_trimmed": "2 failures", "timestamp": "2026-05-12T00:00:01Z"}
            ],
            "unverified_work": ["src/auth.rs", "cargo test"]
        });

        let manifest = NewTurnManifest {
            session_id: session.id.clone(),
            commit_id: None,
            turn_index: 0,
            request_id: "req-metadata-1".to_string(),
            parent_manifest_ids: Vec::new(),
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            prompt: PromptSnapshot {
                total_tokens: 100,
                effective_context_budget: 4096,
                rendered_fragments: Vec::new(),
                withheld_fragments: Vec::new(),
            },
            hooks: Vec::new(),
            tools: Vec::new(),
            metadata: task_meta.clone(),
        };

        store.save_with_manifests(&session, vec![manifest])?;

        let summaries = store.list_turn_manifests(&session.id)?;
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].session_id, session.id);
        assert_eq!(summaries[0].request_id, "req-metadata-1");

        let loaded = store.get_turn_manifest(&summaries[0].id)?.unwrap();
        assert!(!loaded.metadata.is_null());

        let meta = loaded.metadata.as_object().unwrap();
        assert_eq!(meta["todos"].as_array().unwrap().len(), 3);
        assert_eq!(meta["todos"][0]["content"], "Fix auth bug");
        assert_eq!(meta["todos"][0]["status"], "in_progress");
        assert_eq!(meta["changed_files"].as_array().unwrap().len(), 2);
        assert_eq!(meta["changed_files"][0]["path"], "src/auth.rs");
        assert_eq!(meta["tool_failures"].as_array().unwrap().len(), 1);
        assert_eq!(meta["tool_failures"][0]["tool_name"], "bash");
        assert_eq!(meta["verifications"].as_array().unwrap().len(), 1);
        assert_eq!(meta["verifications"][0]["command"], "cargo test");
        assert_eq!(meta["unverified_work"].as_array().unwrap().len(), 2);

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn sqlite_null_metadata_round_trips() -> Result<()> {
        let dir = temp_dir("sqlite_null_metadata_round_trips");
        let store = SqliteCasSessionStore::new(dir.clone());
        let session = test_session("null-metadata-session");

        let manifest = NewTurnManifest {
            session_id: session.id.clone(),
            commit_id: None,
            turn_index: 0,
            request_id: "req-null-meta".to_string(),
            parent_manifest_ids: Vec::new(),
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            prompt: PromptSnapshot {
                total_tokens: 100,
                effective_context_budget: 4096,
                rendered_fragments: Vec::new(),
                withheld_fragments: Vec::new(),
            },
            hooks: Vec::new(),
            tools: Vec::new(),
            metadata: serde_json::Value::Null,
        };

        store.save_with_manifests(&session, vec![manifest])?;

        let summaries = store.list_turn_manifests(&session.id)?;
        let loaded = store.get_turn_manifest(&summaries[0].id)?.unwrap();
        assert!(loaded.metadata.is_null());

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }
}
