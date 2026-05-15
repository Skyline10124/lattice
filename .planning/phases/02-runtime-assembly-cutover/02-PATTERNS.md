# Phase 2: Runtime Assembly Cutover - Pattern Map

**Mapped:** 2026-05-15
**Files analyzed:** 17
**Analogs found:** 16 / 17

## File Classification

| New/Modified File | Role | Data Flow | Closest Analog | Match Quality |
|-------------------|------|-----------|----------------|---------------|
| `crates/lattice-session/src/turn_tx.rs` or `crates/lattice-session/src/lib.rs` | service/model | CRUD + batch transaction | `crates/lattice-session/src/lib.rs` manifest/CAS transaction code | role-match |
| `crates/lattice-runtime/src/assembly.rs` | service/utility | transform + request-response assembly | `crates/lattice-runtime/src/lib.rs` `build_agent`; `crates/lattice-profile/src/lowering.rs` | role-match |
| `crates/lattice-runtime/src/lib.rs` | facade/config | request-response + dependency wiring | same file: `RuntimeSecurity`, `RuntimeBuilder`, `build_agent` | exact |
| `crates/lattice-runtime/src/turn.rs` | service/executor | streaming + event-driven + session commit | existing `turn.rs` streaming/completion flow | exact, but replace handle state |
| `crates/lattice-runtime/src/handle.rs` | model/registry | event-driven live-state registry | existing `handle.rs` | delete/anti-pattern |
| `crates/lattice-runtime/src/turn_executor.rs` | adapter | request-response bridge | existing `turn_executor.rs` | delete/anti-pattern |
| `crates/lattice-runtime/Cargo.toml` | config | dependency graph | current runtime manifest + validation boundary grep | exact |
| `crates/lattice-cli/src/runtime.rs` | adapter/config | request-response runtime construction | `model_runtime` builder path | role-match |
| `crates/lattice-cli/src/coding_agent.rs` | controller/adapter | streaming request-response | `submit_coding_runtime_turn`, completion rendering | role-match |
| `crates/lattice-cli/src/session.rs` | utility/adapter | transform | `agent_manifest_to_new` conversion | role-match |
| `crates/lattice-cli/src/commands/run.rs` | controller | streaming request-response | `submit_run_runtime_turn`, `RuntimeEventRenderer` | role-match |
| `crates/lattice-cli/src/commands/debug.rs` | controller | streaming request-response | same pattern as `commands/run.rs` | role-match |
| `crates/lattice-cli/src/frontend/tui/agent_runner.rs` | adapter | event-driven streaming | `spawn_stream`, `dispatch_runtime_event` | role-match |
| `crates/lattice-cli/src/test_support.rs` | test utility | streaming request-response fixture | runtime turn fixtures in same file | exact |
| `crates/lattice-cli/tests/frontend_runtime_guards.rs` | test | boundary grep/source guard | existing frontend source guard | exact |
| runtime/session inline tests | test | CRUD + streaming + boundary | `lattice-session` manifest tests; runtime turn tests | exact |
| profile drift warning/audit record | utility/audit | event-driven | no exact implementation | no analog |

## Pattern Assignments

### `crates/lattice-session/src/turn_tx.rs` or `crates/lattice-session/src/lib.rs` (service/model, CRUD + batch transaction)

**Analog:** `crates/lattice-session/src/lib.rs`

**Use for:** `ProfileSnapshot`, `open_turn`, `commit_turn`, turn status persistence, profile identity in manifests.

**Durable object shape** (`crates/lattice-session/src/lib.rs` lines 309-368):
```rust
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
```

**Trait extension pattern** (`crates/lattice-session/src/lib.rs` lines 605-630):
```rust
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
```

**Transaction pattern** (`crates/lattice-session/src/lib.rs` lines 2650-2695):
```rust
fn save_with_manifests(
    &self,
    session: &Session,
    manifests: Vec<NewTurnManifest>,
) -> Result<()> {
    validate_session_id(&session.id)?;
    let mut manifest_ids = Vec::with_capacity(manifests.len());
    for manifest in &manifests {
        manifest_ids.push(turn_manifest_id(manifest)?);
    }

    let conn = self.conn()?;
    for mut manifest in manifests {
        manifest.commit_id = None;
        self.put_turn_manifest_with_conn(&conn, manifest)?;
    }

    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;

    let result = (|| {
        let commit =
            self.record_session_with_manifests_with_conn(&conn, session, manifest_ids.clone())?;
        self.backfill_turn_manifest_commit_ids_with_conn(&conn, &commit.id, &manifest_ids)?;
        self.advance_head_with_conn(&conn, &session.id, "commit")
    })();
```

**Stable logical hash pattern** (`crates/lattice-session/src/lib.rs` lines 3669-3688):
```rust
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
```

**Test pattern** (`crates/lattice-session/src/lib.rs` lines 4432-4480, 4550-4557):
```rust
fn test_manifest(session_id: &str, request_id: &str) -> NewTurnManifest {
    NewTurnManifest {
        session_id: session_id.to_string(),
        commit_id: None,
        turn_index: 0,
        request_id: request_id.to_string(),
        parent_manifest_ids: Vec::new(),
        model: "test-model".to_string(),
        provider: "test-provider".to_string(),
        prompt: PromptSnapshot { total_tokens: 100, effective_context_budget: 4096, rendered_fragments: Vec::new(), withheld_fragments: Vec::new() },
        hooks: Vec::new(),
        tools: Vec::new(),
        metadata: serde_json::Value::Null,
    }
}

#[test]
fn sqlite_save_with_manifests_round_trips() -> Result<()> {
    let dir = temp_dir("sqlite_save_with_manifests_round_trips");
    let store = SqliteCasSessionStore::new(dir.clone());
    let session = test_session("manifest-session");
    let manifest = test_manifest(&session.id, "req-1");

    store.save_with_manifests(&session, vec![manifest])?;
    let summaries = store.list_turn_manifests(&session.id)?;
    assert_eq!(summaries.len(), 1);
    let commit_id = store.commit_id_for_session(&session.id)?.unwrap();
    let loaded = store.get_turn_manifest(&summaries[0].id)?.unwrap();
    assert_eq!(loaded.commit_id.as_deref(), Some(commit_id.as_str()));
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
```

**Planner guidance:** prefer a new `turn_tx.rs` if snapshot/open/commit structs grow beyond a few definitions. Reuse CAS helpers and SQLite transaction style. `commit_turn` should reject stale `base_commit_id` and profile hash mismatch before writing messages/manifests. Profile snapshot hash must be computed from canonical profile/spec content, not capture timestamp.

---

### `crates/lattice-runtime/src/assembly.rs` (service/utility, transform + request-response assembly)

**Analogs:** `crates/lattice-profile/src/spec.rs`, `crates/lattice-profile/src/lowering.rs`, `crates/lattice-runtime/src/lib.rs`

**Pure profile boundary** (`crates/lattice-profile/src/spec.rs` lines 8-24):
```rust
#[derive(Debug, Clone)]
pub struct GenericAgentSpec {
    pub profile_name: String,
    pub model: String,
    pub description: Option<String>,
    pub system_prompt: String,
    pub tools: ToolSelectionSpec,
    pub security: SecuritySpec,
    pub context: ContextSpec,
    pub behavior: BehaviorSpec,
}
```

**Pure lowering pattern** (`crates/lattice-profile/src/lowering.rs` lines 16-31, 85-99):
```rust
pub fn lower_profile_to_spec(
    loaded: &LoadedAgentProfile,
) -> Result<GenericAgentSpec, ProfileError> {
    let profile = &loaded.profile;

    if profile.agent.name.is_empty() {
        return Err(ProfileError::InvalidField {
            path: loaded.path.clone(),
            section: "agent".into(),
            field: "name".into(),
            value: "(empty)".into(),
            reason: "agent name must not be empty".into(),
            expected: "a non-empty agent name string".into(),
        });
    }

    Ok(GenericAgentSpec {
        profile_name: profile.agent.name.clone(),
        model: profile.agent.model.clone(),
        description: profile.agent.description.clone(),
        system_prompt: loaded.system_prompt.clone(),
        tools: ToolSelectionSpec { bundles: profile.tools.bundles.clone(), enabled: profile.tools.enabled.clone() },
        security: SecuritySpec { sandbox_mode: sandbox_mode.to_string() },
        context,
        behavior,
    })
}
```

**Runtime assembly pattern to copy** (`crates/lattice-runtime/src/lib.rs` lines 1296-1345, 1348-1371):
```rust
pub fn build_agent(&self, spec: AgentSpec) -> Result<BuiltAgent, RuntimeError> {
    let resolved =
        self.resolve_with_provider(&spec.model, spec.provider_override.as_deref())?;
    let mut agent = Agent::new(resolved.clone());

    if spec.capabilities.bundles.is_empty() && spec.capabilities.enabled.is_empty() {
        return Err(RuntimeError::Config(
            "AgentSpec must declare capabilities".into(),
        ));
    }
    let capabilities = resolve_capabilities(&spec.capabilities);
    tool_definitions = capabilities.definitions.clone();
    agent = agent.with_tools(tool_definitions.clone());

    let project_root = spec.project_root.clone().unwrap_or_else(|| ".".to_string());
    agent = agent.with_provider(prompt::ProjectInstructionsProvider::from_root(project_root.clone()));

    if let Some(security) = spec.security {
        let executor = DefaultToolExecutor::new(&project_root)
            .map_err(RuntimeError::Config)?
            .with_sandbox(security.sandbox)
            .with_authorized_tools(capabilities.tool_names.clone());
        agent = agent.with_tool_executor(Box::new(executor));
    }

    if let Some(prompt) = spec.system_prompt {
        agent.set_system_prompt(&prompt);
    }
    if !spec.messages.is_empty() {
        agent.seed_messages(spec.messages);
    }
    if let Some(effort) = spec.thinking_effort {
        agent = agent.with_thinking_effort(Some(effort));
    }

    Ok(BuiltAgent { agent, model: resolved, context_length, tool_definitions })
}
```

**Agent primitives** (`crates/lattice-agent/src/config.rs` lines 11-29, 53-87, 157-197):
```rust
impl Agent {
    pub fn new(resolved: ResolvedModel) -> Self { ... }
    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self { self.tools = tools; self }
    pub fn with_tool_executor(mut self, executor: Box<dyn ToolExecutor>) -> Self { self.tool_executor = Some(executor); self }
    pub fn set_system_prompt(&mut self, prompt: &str) { self.registry.set_system_prompt(prompt); }
    pub fn seed_messages(&mut self, messages: Vec<lattice_core::types::Message>) { self.state.seed_messages(messages); }
}
```

**Planner guidance:** `assembly.rs` should accept `GenericAgentSpec` plus a separate invocation context (`cwd`, `project_root`, `RuntimeSecurity`, optional provider override/session selector) and open-turn history. Do not copy `lattice_orchestrator::assembly::assemble_agent`, `AgentTurnSpec`, plugin overlay, registry-backed live handles, or profile discovery.

---

### `crates/lattice-runtime/src/lib.rs` (facade/config, request-response + dependency wiring)

**Analog:** same file, but retain only model resolution, security, tool registry, session store, and generic assembly entry points.

**Security construction** (`crates/lattice-runtime/src/lib.rs` lines 151-252):
```rust
pub fn build_runtime_security(
    config: &RuntimeSecurityConfig,
    workdir: &Path,
) -> Result<RuntimeSecurity, RuntimeError> {
    let canonical_workdir = std::fs::canonicalize(workdir)
        .unwrap_or_else(|_| workdir.to_path_buf())
        .to_string_lossy()
        .to_string();

    let approved_rules = Arc::new(Mutex::new(Vec::new()));
    let policy_authority = PolicyAuthority::from_shared_rules(Arc::clone(&approved_rules));

    let mut sandbox = match config.sandbox_mode.as_str() {
        "permissive" => SandboxConfig::permissive(),
        "project" | "strict" => SandboxConfig { read_allowlist: vec![canonical_workdir.clone()], write_allowlist: vec![canonical_workdir.clone()], ..SandboxConfig::default() },
        mode => return Err(RuntimeError::Config(format!("unknown security.sandbox_mode '{}': expected 'project', 'strict', or 'permissive'", mode))),
    };

    sandbox.approved_rules = Some(Arc::clone(&approved_rules));
    sandbox.policy_authority = Some(policy_authority.clone());

    Ok(RuntimeSecurity { mode_label, sandbox, approved_rules, policy_authority, audit_log })
}
```

**Builder pattern to keep** (`crates/lattice-runtime/src/lib.rs` lines 1548-1604):
```rust
pub struct RuntimeBuilder {
    config: RuntimeConfig,
    parts: RuntimeParts,
    _plugin_dir: Option<PathBuf>,
    _plugin_watch_dir: Option<PathBuf>,
}

impl RuntimeBuilder {
    pub fn name(mut self, name: impl Into<String>) -> Self { self.config.name = name.into(); self }
    pub fn credentials(mut self, credentials: HashMap<String, String>) -> Self { self.config.credentials = credentials; self }
    pub fn tool_registry(mut self, registry: Arc<ToolRegistry>) -> Self { self.parts.tool_registry = registry; self }
    pub fn session_store(mut self, store: Arc<dyn SessionStore>) -> Self { self.parts.session_store = Some(store); self }
}
```

**Delete/replace extension APIs** (`crates/lattice-runtime/src/lib.rs` lines 1221-1294):
```rust
pub async fn run_pipeline(&self, start_agent: &str, input: &str) -> PipelineRun { ... }
pub fn micro_agent(&self, profile_name: &str, bus: Arc<dyn Orchestrator>) -> Result<MicroAgent, RuntimeError> { ... }
pub fn dry_run_pipeline(&self, start_agent: &str) -> lattice_orchestrator::DryRunReport { ... }
pub fn pipeline(&self, name: &str) -> Pipeline { ... }
pub fn agent_registry(&self) -> Arc<AgentRegistry> { ... }
pub fn plugin_registry(&self) -> Arc<PluginRegistry> { ... }
pub fn event_bus(&self) -> Arc<EventBus> { ... }
```

**Planner guidance:** remove runtime public API for pipeline, micro-agent, event bus, plugin registry, plugin overlay, runtime-backed orchestrator executor, `load_plugin_dir`, and `watch_plugin_dir`. Keep errors explicit and convert session errors through `RuntimeError::Config` or a new narrow variant.

---

### `crates/lattice-runtime/src/turn.rs` (service/executor, streaming + session commit)

**Analog:** existing `turn.rs`; keep streaming/completion semantics, replace handle-bound identity and persistence.

**Event/completion shape to adapt** (`crates/lattice-runtime/src/turn.rs` lines 17-112):
```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TurnId(String);

#[derive(Debug, Clone)]
pub struct TurnRequest {
    pub input: String,
    pub max_turns: u32,
    pub persist_session: bool,
    pub session_id: Option<String>,
    pub cancellation_id: Option<String>,
}

#[derive(Debug, Clone)]
pub enum RuntimeTurnEvent {
    Token { handle_id: AgentHandleId, turn_id: TurnId, text: String },
    Reasoning { handle_id: AgentHandleId, turn_id: TurnId, text: String },
    ToolStarted { handle_id: AgentHandleId, turn_id: TurnId, call_id: String, name: String, arguments: String, batch_index: usize, batch_count: usize },
    ToolFinished { handle_id: AgentHandleId, turn_id: TurnId, call_id: String, name: String, result: String },
    ManifestReady { handle_id: AgentHandleId, turn_id: TurnId, count: usize },
    Completed { handle_id: AgentHandleId, turn_id: TurnId },
    Cancelled { handle_id: AgentHandleId, turn_id: TurnId },
    Error { handle_id: AgentHandleId, turn_id: TurnId, message: String, retryable: bool },
}
```

**Streaming mapping pattern** (`crates/lattice-runtime/src/turn.rs` lines 369-449):
```rust
let events = agent
    .run_streaming(&input, max_turns, |event: LoopEvent| match event {
        LoopEvent::Token { text } => {
            assistant_text.push_str(&text);
            let _ = tx.send(RuntimeTurnEvent::Token {
                handle_id: hid.clone(),
                turn_id: tid.clone(),
                text,
            });
        }
        LoopEvent::ToolCallRequired { calls } => {
            let batch_count = calls.len();
            for (batch_index, call) in calls.iter().enumerate() {
                let _ = tx.send(RuntimeTurnEvent::ToolStarted {
                    handle_id: hid.clone(),
                    turn_id: tid.clone(),
                    call_id: call.id.clone(),
                    name: call.function.name.clone(),
                    arguments: call.function.arguments.clone(),
                    batch_index: batch_index + 1,
                    batch_count,
                });
            }
        }
        LoopEvent::Manifest { manifest } => {
            manifests.push(manifest);
            let _ = tx.send(RuntimeTurnEvent::ManifestReady { handle_id: hid.clone(), turn_id: tid.clone(), count: manifests.len() });
        }
        LoopEvent::Error { message, retryable } => {
            status = TurnStatus::Error;
            error_msg = Some(message.clone());
            let _ = tx.send(RuntimeTurnEvent::Error { handle_id: hid.clone(), turn_id: tid.clone(), message, retryable });
        }
    })
    .await;
```

**Terminal completion pattern** (`crates/lattice-runtime/src/turn.rs` lines 465-543):
```rust
let task_state_snapshot = agent.task_state().clone();
let final_report = (status != TurnStatus::Cancelled).then(|| {
    lattice_agent::FinalReport::from_task_state(&task_state_snapshot, None).render_markdown()
});

match status {
    TurnStatus::Completed => { let _ = tx.send(RuntimeTurnEvent::Completed { handle_id: hid.clone(), turn_id: tid.clone() }); }
    TurnStatus::Cancelled => { let _ = tx.send(RuntimeTurnEvent::Cancelled { handle_id: hid.clone(), turn_id: tid.clone() }); }
    TurnStatus::Error => { /* emit error if not already emitted */ }
}

let completion = TurnCompletion {
    handle_id: hid,
    turn_id: tid,
    status,
    assistant_text,
    token_usage,
    task_state_snapshot,
    final_report,
    turn_manifests: manifests,
    model,
    provider,
    saved_session_id,
    error: error_msg,
};
```

**Persistence path to replace** (`crates/lattice-runtime/src/turn.rs` lines 616-668):
```rust
fn persist_turn_session(
    store: &dyn SessionStore,
    requested_session_id: Option<&str>,
    model: &str,
    provider: &str,
    input: &str,
    assistant_text: &str,
    manifests: &[TurnManifestObject],
) -> Result<String, lattice_session::SessionError> {
    let mut session = if let Some(id) = requested_session_id {
        match store.load(id)? {
            Some(session) => session,
            None => { let mut session = Session::new(model.to_string(), provider.to_string()); session.id = id.to_string(); session }
        }
    } else {
        Session::new(model.to_string(), provider.to_string())
    };

    session.append_turn(model.to_string(), provider.to_string(), input.to_string(), assistant_text.to_string());
    let saved_session_id = session.id.clone();
    let new_manifests = manifests.iter().enumerate().map(|(index, manifest)| manifest_to_new(manifest, &saved_session_id, model, provider, index)).collect::<Vec<_>>();

    if new_manifests.is_empty() { store.save(&session)?; } else { store.save_with_manifests(&session, new_manifests)?; }
    Ok(saved_session_id)
}
```

**Planner guidance:** preserve streaming event order and `TurnStatus::{Completed, Cancelled, Error}`. Replace `handle_id` with `session_id + turn_id`. Replace `persist_turn_session` with session `open_turn` / `commit_turn`; commit completed/error/cancelled outcomes when a visible/model-visible boundary exists.

---

### `crates/lattice-runtime/src/handle.rs` (model/registry, delete/anti-pattern)

**Analog:** existing file, but use it only to identify concepts to remove.

**Anti-pattern** (`crates/lattice-runtime/src/handle.rs` lines 1-6, 56-69, 105-119):
```rust
//! Runtime-owned agent handle registry and lifecycle.
//! Handles are keyed by session/workspace/profile/model/security policy
//! and support reuse, rebuild, dispose, and idle eviction.

pub struct AgentHandleKey {
    pub session_id: Option<String>,
    pub workspace: PathBuf,
    pub profile_name: Option<String>,
    pub model: String,
    pub provider_override: Option<String>,
    pub security_policy: String,
}

pub struct AgentHandleSnapshot {
    pub id: AgentHandleId,
    pub revision: u64,
    pub model: String,
    pub provider: String,
    pub message_count: usize,
    pub task_state_empty: bool,
    pub active_turn: bool,
    pub recent_completion_count: usize,
    pub plugin_overlay_name: Option<String>,
    pub approved_rule_count: usize,
}
```

**Planner guidance:** remove handle key matching, rebuild, recent completions, plugin overlay, and live `Agent` registry from the generic runtime path. Session owns history; runtime may construct transient per-turn agents only.

---

### `crates/lattice-runtime/src/turn_executor.rs` (adapter, delete/anti-pattern)

**Analog:** existing file, but it bridges runtime back to orchestrator-owned contracts.

**Anti-pattern** (`crates/lattice-runtime/src/turn_executor.rs` lines 13-23, 54-78):
```rust
use lattice_orchestrator::{
    AgentTurnExecutionError, AgentTurnExecutor, AgentTurnOutput, AgentTurnRequest, AgentTurnSpec,
    AgentTurnStatus,
};

async fn execute_runtime_turn(
    &self,
    spec: AgentTurnSpec,
    request: &AgentTurnRequest,
    input: String,
    security: RuntimeSecurity,
) -> Result<crate::turn::TurnCompletion, AgentTurnExecutionError> {
    let handle_id = self
        .runtime
        .get_or_create_agent_handle_for_session(spec, security, request.session_id.clone())
        .map_err(map_runtime_error)?;

    let stream = self
        .runtime
        .submit_turn(&handle_id, turn_request)
        .await
        .map_err(map_runtime_error)?;
```

**Planner guidance:** delete this adapter from `lattice-runtime` with orchestrator dependency removal. Any temporary bridge should live above runtime and consume the generic runtime API, not `AgentTurnSpec`.

---

### `crates/lattice-runtime/Cargo.toml` (config, dependency graph)

**Analog:** current manifest and `02-VALIDATION.md` boundary grep.

**Current dependency edges to cut** (`crates/lattice-runtime/Cargo.toml` lines 9-18, 29-31):
```toml
[dependencies]
lattice-core = { workspace = true }
lattice-session = { workspace = true }
lattice-prompt = { workspace = true }
lattice-agent = { workspace = true }
lattice-plugin = { workspace = true }
lattice-sandbox = { workspace = true }
lattice-tools = { workspace = true }
lattice-orchestrator = { workspace = true }

[features]
default = []
blob-store = ["lattice-agent/blob-store", "lattice-orchestrator/blob-store"]
```

**Planner guidance:** remove `lattice-orchestrator`, `lattice-plugin`, and plugin feature edges after source imports are gone. Add `lattice-profile` only if runtime directly uses `GenericAgentSpec`; otherwise keep profile loading in CLI and pass a snapshot/data contract.

---

### `crates/lattice-cli/src/runtime.rs` (adapter/config, request-response runtime construction)

**Analog:** `model_runtime`; `pipeline_runtime` is cleanup/deferred extension surface.

**Keep** (`crates/lattice-cli/src/runtime.rs` lines 11-16):
```rust
pub(crate) fn model_runtime(credentials: HashMap<String, String>) -> Runtime {
    Runtime::builder()
        .credentials(credentials)
        .session_store(Arc::new(crate::session::session_manager()))
        .build()
}
```

**Remove or defer** (`crates/lattice-cli/src/runtime.rs` lines 18-46):
```rust
pub(crate) fn pipeline_runtime(
    agents_dir: Option<&str>,
    plugins_dir: Option<&str>,
    credentials: HashMap<String, String>,
) -> Result<(Runtime, PathBuf)> {
    let registry = AgentRegistry::load_dir(&dir)?;
    let plugin_registry = Arc::new(PluginRegistry::new());
    lattice_plugins::register_official_plugins(&plugin_registry)?;
    let mut builder = Runtime::builder()
        .name("swarm-pipeline")
        .credentials(credentials)
        .agent_registry(registry)
        .plugin_registry(plugin_registry.clone())
        .tool_registry(tool_registry);
```

**Planner guidance:** CLI should build the narrowed runtime with credentials and session store, then call `lattice-profile` discovery/loading/lowering before invoking runtime. Do not reimplement profile resolution in CLI.

---

### `crates/lattice-cli/src/coding_agent.rs` (controller/adapter, streaming request-response)

**Analog:** current coding adapter.

**Request/completion flow** (`crates/lattice-cli/src/coding_agent.rs` lines 79-120):
```rust
let request = TurnRequest {
    input: options.prompt.clone(),
    max_turns: options.max_turns,
    persist_session: options.save_session,
    session_id: options.previous_session.as_ref().map(|s| s.id.clone()),
    cancellation_id: None,
};

let completion = if options.json || !options.stream_output {
    submit_coding_runtime_turn(&built.runtime, &built.handle_id, request, None).await?
} else {
    let mut renderer = CodingEventRenderer::new(options.verbose, false);
    let completion = submit_coding_runtime_turn(
        &built.runtime,
        &built.handle_id,
        request,
        Some(&mut renderer),
    ).await?;
    renderer.finish(completion.token_usage.as_ref().map(|usage| u64::from(usage.total_tokens)), &final_report_markdown)?;
    completion
};
```

**Stream drain + status gate** (`crates/lattice-cli/src/coding_agent.rs` lines 194-224):
```rust
pub(crate) async fn submit_coding_runtime_turn(
    runtime: &Runtime,
    handle_id: &AgentHandleId,
    request: TurnRequest,
    mut renderer: Option<&mut CodingEventRenderer>,
) -> Result<TurnCompletion> {
    let stream = runtime.submit_turn(handle_id, request).await?;
    tokio::pin!(stream);
    while let Some(event) = stream.next().await {
        if let Some(renderer) = renderer.as_deref_mut() {
            renderer.render_runtime(&event)?;
        }
    }

    let completion = runtime
        .latest_turn_completion(handle_id)?
        .ok_or_else(|| anyhow::anyhow!("runtime turn completed without stored completion"))?;
    require_successful_runtime_completion(completion)
}

pub(crate) fn require_successful_runtime_completion(completion: TurnCompletion) -> Result<TurnCompletion> {
    match completion.status {
        TurnStatus::Completed => Ok(completion),
        TurnStatus::Cancelled => anyhow::bail!("runtime turn was cancelled"),
        TurnStatus::Error => anyhow::bail!("{}", completion.error.as_deref().unwrap_or("runtime turn failed")),
    }
}
```

**Render pattern** (`crates/lattice-cli/src/coding_agent.rs` lines 374-409):
```rust
pub(crate) fn render_completion_value(completion: &TurnCompletion) -> serde_json::Value {
    render_final_json(
        &completion.assistant_text,
        completion.token_usage.as_ref().map(|usage| usage.total_tokens).unwrap_or_default().into(),
        completion.final_report.as_deref().unwrap_or_default(),
    )
}
```

**Planner guidance:** keep rendering/status behavior. Replace `AgentTurnSpec + AgentHandleId` build path with profile-source lowering plus generic runtime setup. Completion should come back from the generic turn API or be keyed by `session_id + turn_id`.

---

### `crates/lattice-cli/src/session.rs` (utility/adapter, transform)

**Analog:** agent manifest to session manifest conversion.

**Conversion pattern** (`crates/lattice-cli/src/session.rs` lines 22-115):
```rust
pub fn agent_manifest_to_new(
    manifest: &lattice_agent::TurnManifestObject,
    session_id: &str,
    model: &str,
    provider: &str,
    turn_index: usize,
) -> NewTurnManifest {
    NewTurnManifest {
        session_id: session_id.to_string(),
        commit_id: None,
        turn_index,
        request_id: manifest.request_id.clone(),
        parent_manifest_ids: Vec::new(),
        model: model.to_string(),
        provider: provider.to_string(),
        prompt: lattice_session::PromptSnapshot {
            total_tokens: manifest.prompt_snapshot.token_count,
            effective_context_budget: 0,
            rendered_fragments: manifest.prompt_snapshot.rendered_fragments.iter().map(|f| lattice_session::PromptFragmentRecord { ... }).collect(),
            withheld_fragments: manifest.prompt_snapshot.withheld_fragments.iter().map(|f| lattice_session::PromptFragmentRecord { ... }).collect(),
        },
        hooks: manifest.hook_decisions.iter().enumerate().map(|(i, h)| { ... }).collect(),
        tools: manifest.tool_decisions.iter().map(|t| lattice_session::ToolDecisionRecord { ... }).collect(),
        metadata: manifest.metadata.clone().unwrap_or(serde_json::Value::Null),
    }
}
```

**Planner guidance:** runtime/session should own this conversion after cutover. CLI should not be the primary persistence converter once `commit_turn` exists.

---

### `crates/lattice-cli/src/commands/run.rs` and `commands/debug.rs` (controller, streaming request-response)

**Analog:** `commands/run.rs`.

**Current adapter path to replace** (`crates/lattice-cli/src/commands/run.rs` lines 36-101):
```rust
let runtime = crate::runtime::model_runtime(creds.to_hashmap());
let session_id = previous_session.as_ref().map(|session| session.id.clone());
let seed_messages = previous_session
    .as_ref()
    .map(crate::session::messages_for_agent)
    .unwrap_or_default();
let task_state_seed =
    crate::coding_agent::load_latest_manifest_metadata(&runtime, previous_session.as_ref());
let handle_id = runtime.get_or_create_agent_handle_for_session_seeded(
    turn_spec,
    security.clone(),
    session_id.clone(),
    seed_messages,
    task_state_seed,
)?;

let request = TurnRequest {
    input: prompt.clone(),
    max_turns,
    persist_session: save,
    session_id,
    cancellation_id: None,
};
```

**Stream drain pattern** (`crates/lattice-cli/src/commands/run.rs` lines 361-379):
```rust
pub(crate) async fn submit_run_runtime_turn(
    runtime: &Runtime,
    handle_id: &AgentHandleId,
    request: TurnRequest,
    mut renderer: Option<&mut RuntimeEventRenderer>,
) -> Result<TurnCompletion> {
    let stream = runtime.submit_turn(handle_id, request).await?;
    tokio::pin!(stream);
    while let Some(event) = stream.next().await {
        if let Some(renderer) = renderer.as_deref_mut() {
            renderer.render(&event)?;
        }
    }

    let completion = runtime
        .latest_turn_completion(handle_id)?
        .ok_or_else(|| anyhow::anyhow!("runtime turn completed without stored completion"))?;
    crate::coding_agent::require_successful_runtime_completion(completion)
}
```

**Planner guidance:** preserve dry-run, `/compact`, renderer, and status gate. Replace `run_turn_spec`/handle creation with profile discovery/lowering or a generic default profile. `debug.rs` should follow the same Error/Cancelled gating.

---

### `crates/lattice-cli/src/frontend/tui/agent_runner.rs` (adapter, event-driven streaming)

**Analog:** TUI runtime event bridge.

**Build/stream path to rewrite** (`crates/lattice-cli/src/frontend/tui/agent_runner.rs` lines 119-159, 186-230):
```rust
fn build_runtime_handle(
    request: &AgentBuildRequest,
    spec: AgentTurnSpec,
    session_id: Option<String>,
) -> anyhow::Result<AgentBuildOutcome> {
    let resolved = request
        .runtime
        .resolve_with_provider(&request.current_model, request.provider_override.as_deref())?;
    let seed_messages = request.previous_session.as_ref().map(crate::session::messages_for_agent).unwrap_or_default();
    let task_state_seed = crate::coding_agent::load_latest_manifest_metadata(&request.runtime, request.previous_session.as_ref());
    let handle_id = request.runtime.get_or_create_agent_handle_for_session_seeded(
        spec,
        request.security.clone(),
        session_id,
        seed_messages,
        task_state_seed,
    )?;
    Ok(AgentBuildOutcome { handle_id, rebuilt: Some(...) })
}

pub(super) fn spawn_stream(request: AgentStreamRequest) {
    tokio::spawn(async move {
        let turn_request = TurnRequest {
            input: request.prompt_text.clone(),
            max_turns: 10,
            persist_session: request.persist_session,
            session_id: request.session_id.clone(),
            cancellation_id: None,
        };
        let stream = match request.runtime.submit_turn(&request.handle_id, turn_request).await { ... };
```

**Event dispatch pattern** (`crates/lattice-cli/src/frontend/tui/agent_runner.rs` lines 267-374):
```rust
fn dispatch_runtime_event(
    context: RuntimeEventContext<'_>,
    event: RuntimeTurnEvent,
) -> Option<RuntimeTerminal> {
    let event_handle = runtime_event_handle_id(&event);
    let event_turn = runtime_event_turn_id(&event);
    if event_handle != context.handle_id { return None; }
    if context.runtime_turn_id.is_some_and(|expected| expected != event_turn) { return None; }

    match event {
        RuntimeTurnEvent::Token { text, .. } => {
            let _ = context.tx.send(Event::StreamToken { turn_id: context.local_turn_id, content: text, reasoning: None, done: false, error: None });
            None
        }
        RuntimeTurnEvent::Completed { turn_id, .. } => Some(RuntimeTerminal { status: TurnStatus::Completed, runtime_turn_id: turn_id, error: None }),
        RuntimeTurnEvent::Cancelled { turn_id, .. } => Some(RuntimeTerminal { status: TurnStatus::Cancelled, runtime_turn_id: turn_id, error: None }),
        RuntimeTurnEvent::Error { turn_id, message, .. } => Some(RuntimeTerminal { status: TurnStatus::Error, runtime_turn_id: turn_id, error: Some(message) }),
        _ => None,
    }
}
```

**Planner guidance:** keep local TUI event translation and terminal mapping. Replace handle filtering with `session_id + runtime_turn_id`. Broad UX cleanup remains Phase 3.

---

## Shared Patterns

### Tool and Sandbox Binding

**Sources:** `crates/lattice-tools/src/capabilities.rs`, `crates/lattice-tools/src/executor.rs`, `crates/lattice-runtime/src/lib.rs`

**Capability resolution** (`crates/lattice-tools/src/capabilities.rs` lines 48-61):
```rust
pub fn resolve_capabilities(selection: &CapabilitySelection) -> ResolvedCapabilities {
    let bundles: BTreeSet<ToolBundle> = selection.bundles.iter().copied().collect();
    let tool_names = resolve_tool_names(selection);
    let definitions = default_tool_definitions()
        .into_iter()
        .filter(|definition| tool_names.contains(&definition.name))
        .collect();

    ResolvedCapabilities { bundles, tool_names, definitions }
}
```

**Executor authorization** (`crates/lattice-tools/src/executor.rs` lines 292-304, 313-329):
```rust
pub fn with_sandbox(mut self, config: SandboxConfig) -> Self {
    self.sandbox = config;
    self
}

pub fn with_authorized_tools<I, S>(mut self, tool_names: I) -> Self
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    self.authorized_tools = Some(tool_names.into_iter().map(Into::into).collect());
    self
}

fn authorization_denial_report(&self, tool_name: &str) -> Option<DenialReport> {
    let authorized = self.authorized_tools.as_ref()?;
    if authorized.contains(tool_name) { return None; }
    Some(DenialReport::new(
        format!("tool '{}' is not enabled for this agent", tool_name),
        RiskClass::Mutate,
        DenialKind::Tool { tool_name: tool_name.to_string(), enabled_tools: authorized.iter().cloned().collect() },
    ))
}
```

### Boundary Guard Pattern

**Source:** `crates/lattice-cli/tests/frontend_runtime_guards.rs` lines 3-24, 66-83, 86-119.

```rust
const FRONTEND_FILES: &[&str] = &[
    "src/coding_agent.rs",
    "src/commands/run.rs",
    "src/commands/debug.rs",
    "src/frontend/tui/app.rs",
    "src/frontend/tui/agent_runner.rs",
    "src/frontend/tui/plugin_mode.rs",
];

fn assert_absent_in_active_source(needles: &[&str]) {
    let mut failures = Vec::new();
    for (path, source) in frontend_sources() {
        let active = active_source(&source);
        for needle in needles {
            if active.contains(needle) {
                failures.push(format!("{path} contains forbidden active-path string {needle:?}"));
            }
        }
    }
    assert!(failures.is_empty(), "frontend runtime guard failures:\n{}", failures.join("\n"));
}
```

**Apply to:** new runtime boundary guard for `lattice_orchestrator|lattice_plugin|Pipeline|MicroAgent|EventBus|PluginRegistry|RuntimePluginOverlay|RuntimeAgentTurnExecutor|load_plugin_dir|watch_plugin_dir` under `crates/lattice-runtime` and `crates/lattice-runtime/Cargo.toml`.

### Runtime/Session Test Fixtures

**Runtime streaming tests:** copy the scripted OpenAI SSE fixture style from `crates/lattice-runtime/src/turn.rs` lines 804-856 and 1279-1495.

**CLI adapter fixtures:** copy `crates/lattice-cli/src/test_support.rs` lines 69-134 for runtime turn fixture construction, registering a scripted model, building security, submitting a turn, and asserting JSON rendering.

**Sandbox denial tests:** copy `crates/lattice-tools/src/executor.rs` lines 1688-1734 for unenabled tool denial and no side effects.

## No Analog Found

| File/Behavior | Role | Data Flow | Reason |
|---------------|------|-----------|--------|
| exact `open_turn` / `commit_turn` API | service/model | turn transaction | No exact API exists yet. Use session CAS/manifest transaction analogs. |
| profile drift warning/audit record | utility/audit | event-driven | No exact implementation exists. Store locked snapshot hash and safe drift metadata; surface warning equivalent to Phase context. |

## Metadata

**Analog search scope:** `crates/lattice-session`, `crates/lattice-runtime`, `crates/lattice-profile`, `crates/lattice-cli`, `crates/lattice-agent`, `crates/lattice-tools`, `crates/lattice-sandbox`
**Files scanned:** 75+ Rust/TOML files via targeted source search from the prior mapping pass
**Pattern extraction date:** 2026-05-15

**Do not copy:** runtime live-agent handle registry, orchestrator `AgentTurnSpec`, `RuntimeAgentTurnExecutor`, plugin overlay, plugin registry/event bus/pipeline facade, or `save_with_manifests` fallback behavior as the new primary design.
