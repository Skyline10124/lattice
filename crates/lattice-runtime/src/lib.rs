//! Deep runtime interface for LATTICE.
//!
//! Runtime owns model routing, runtime security, generic profile assembly,
//! submitted turns, and durable session persistence. Extension systems such as
//! pipelines, plugins, and live agent handles intentionally live outside this
//! crate's public boundary.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use futures::Stream;
use lattice_agent::audit::AuditLog;
use lattice_agent::hook::HookChain;
use lattice_agent::ContextManager;
use lattice_core::catalog::{ModelCatalogEntry, ResolvedModel};
use lattice_core::router::ModelRouter;
use lattice_core::streaming::StreamEvent;
use lattice_core::types::{FunctionCall, Message, Role, ToolCall, ToolDefinition};
use lattice_prompt as prompt;
use lattice_sandbox::{
    ApprovalKind, ApprovedRule, LandlockConfig, PolicyAuthority, SandboxConfig, ValidationError,
};
use lattice_session::{CompactBoundary, Session, SessionMessage, SessionStore};
use lattice_tools::{DefaultToolExecutor, ToolBundle};

mod assembly;
mod session;
pub mod turn;

pub use assembly::{GenericAgentAssembly, RuntimeInvocationContext};
pub use preset::coding_system_prompt;
pub use session::{AgentSessionRequest, ProfileSourceFingerprint, RuntimeAgentSession};
pub use turn::{
    RuntimeTurnEvent, SubmittedTurn, TurnCompletion, TurnId, TurnRequest, TurnStatus, TurnStream,
};

mod preset;

#[derive(Clone, Debug)]
pub struct RuntimeSecurityConfig {
    pub sandbox_mode: String,
    pub read_allowlist: Vec<String>,
    pub write_allowlist: Vec<String>,
    pub command_allowlist: Vec<String>,
    pub hook_chain: bool,
    pub landlock: bool,
    pub audit: bool,
    pub audit_dir: Option<PathBuf>,
    pub max_command_timeout: Option<u32>,
    pub max_read_size: Option<usize>,
    pub max_write_size: Option<usize>,
    pub max_http_response_size: Option<usize>,
}

impl Default for RuntimeSecurityConfig {
    fn default() -> Self {
        Self {
            sandbox_mode: "project".into(),
            read_allowlist: vec![],
            write_allowlist: vec![],
            command_allowlist: vec![],
            hook_chain: false,
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

#[derive(Clone)]
pub struct RuntimeSecurity {
    pub mode_label: String,
    pub sandbox: SandboxConfig,
    pub approved_rules: Arc<Mutex<Vec<ApprovedRule>>>,
    pub policy_authority: PolicyAuthority,
    audit_log: Option<Arc<AuditLog>>,
}

impl RuntimeSecurity {
    pub fn approve_rule(&self, rule: ApprovedRule) -> Result<(), ValidationError> {
        if let ApprovalKind::Bundle { bundle_name } = &rule.kind {
            ToolBundle::from_str(bundle_name)
                .map_err(|_| ValidationError::UnknownBundle(bundle_name.clone()))?;
        }
        let shared_with_legacy = Arc::ptr_eq(&self.approved_rules, self.policy_authority.inner());
        self.policy_authority.approve(rule.clone(), None)?;
        if !shared_with_legacy {
            self.approved_rules
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(rule);
        }
        Ok(())
    }

    pub async fn reap_audit(&self) {
        if let Some(log) = &self.audit_log {
            log.reap_background_tasks().await;
        }
    }

    pub async fn execute_bash(&self, workdir: &Path, cmd: &str) -> Result<String, RuntimeError> {
        let project_root = workdir.to_string_lossy().to_string();
        let output = DefaultToolExecutor::execute_bash(&project_root, &self.sandbox, cmd)
            .await
            .map_err(RuntimeError::Config)?;
        let mut rendered = String::from_utf8_lossy(&output.stdout).to_string();
        if !output.stderr.is_empty() {
            if !rendered.is_empty() {
                rendered.push('\n');
            }
            rendered.push_str(&String::from_utf8_lossy(&output.stderr));
        }
        Ok(rendered)
    }
}

pub fn build_runtime_security(
    config: &RuntimeSecurityConfig,
    workdir: &Path,
) -> Result<RuntimeSecurity, RuntimeError> {
    let canonical_workdir = std::fs::canonicalize(workdir)
        .unwrap_or_else(|_| workdir.to_path_buf())
        .to_string_lossy()
        .to_string();

    let mode_label = config.sandbox_mode.clone();
    let audit_log = if config.audit {
        let dir = config
            .audit_dir
            .clone()
            .unwrap_or_else(|| workdir.join(".lattice-audit"));
        Some(Arc::new(AuditLog::new(dir)))
    } else {
        None
    };

    let hook_chain = if config.hook_chain {
        Some(Arc::new(HookChain::new(vec![Box::new(
            lattice_agent::hook::VetHook::new(PathBuf::from(&canonical_workdir)),
        )])))
    } else {
        None
    };

    let landlock = config.landlock.then(LandlockConfig::default);
    let approved_rules = Arc::new(Mutex::new(Vec::new()));
    let policy_authority = PolicyAuthority::from_shared_rules(Arc::clone(&approved_rules));

    let mut sandbox = match config.sandbox_mode.as_str() {
        "permissive" => SandboxConfig::permissive(),
        "project" | "strict" => SandboxConfig {
            read_allowlist: if config.read_allowlist.is_empty() {
                vec![canonical_workdir.clone()]
            } else {
                config.read_allowlist.clone()
            },
            write_allowlist: if config.write_allowlist.is_empty() {
                vec![canonical_workdir.clone()]
            } else {
                config.write_allowlist.clone()
            },
            command_allowlist: if config.command_allowlist.is_empty() {
                vec![
                    "cargo test".into(),
                    "cargo build".into(),
                    "cargo check".into(),
                    "cargo fmt".into(),
                    "cargo clippy".into(),
                    "git status".into(),
                    "git diff".into(),
                    "git log".into(),
                    "grep".into(),
                    "find".into(),
                    "ls".into(),
                ]
            } else {
                config.command_allowlist.clone()
            },
            hook: hook_chain.map(|hc| hc as Arc<dyn lattice_sandbox::hooks::CommandHook>),
            landlock,
            audit: audit_log.clone().map(|log| {
                Arc::new(lattice_agent::sandbox::AuditLogSink::new(log))
                    as Arc<dyn lattice_sandbox::audit::AuditSink>
            }),
            max_command_timeout: config.max_command_timeout.unwrap_or(30),
            max_read_size: config.max_read_size.unwrap_or(10 * 1024 * 1024),
            max_write_size: config.max_write_size.unwrap_or(1024 * 1024),
            max_http_response_size: config.max_http_response_size.unwrap_or(10 * 1024 * 1024),
            ..SandboxConfig::default()
        },
        mode => {
            return Err(RuntimeError::Config(format!(
                "unknown security.sandbox_mode '{}': expected 'project', 'strict', or 'permissive'",
                mode
            )));
        }
    };

    sandbox.approved_rules = Some(Arc::clone(&approved_rules));
    sandbox.policy_authority = Some(policy_authority.clone());

    Ok(RuntimeSecurity {
        mode_label,
        sandbox,
        approved_rules,
        policy_authority,
        audit_log,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Core(#[from] lattice_core::LatticeError),
    #[error("runtime configuration error: {0}")]
    Config(String),
    #[error("agent assembly failed: {0}")]
    Assembly(String),
    #[error("turn not found: {0}")]
    TurnNotFound(String),
    #[error("turn busy: {0}")]
    TurnBusy(String),
}

#[derive(Clone, Default)]
pub struct RuntimeConfig {
    pub credentials: HashMap<String, String>,
}

#[derive(Clone, Default)]
pub struct RuntimeParts {
    pub session_store: Option<Arc<dyn SessionStore>>,
}

#[derive(Clone)]
pub struct Runtime {
    router: Arc<ModelRouter>,
    session_store: Option<Arc<dyn SessionStore>>,
    session_turns: Arc<Mutex<HashMap<(String, String), tokio::task::AbortHandle>>>,
}

impl Runtime {
    pub fn new(config: RuntimeConfig, parts: RuntimeParts) -> Self {
        let router = if config.credentials.is_empty() {
            ModelRouter::new()
        } else {
            ModelRouter::with_credentials(config.credentials)
        };
        Self {
            router: Arc::new(router),
            session_store: parts.session_store,
            session_turns: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn builder() -> RuntimeBuilder {
        RuntimeBuilder::default()
    }

    pub fn resolve(&self, model: &str) -> Result<ResolvedModel, RuntimeError> {
        self.resolve_with_provider(model, None)
    }

    pub fn resolve_with_provider(
        &self,
        model: &str,
        provider_override: Option<&str>,
    ) -> Result<ResolvedModel, RuntimeError> {
        Ok(self.router.resolve(model, provider_override)?)
    }

    pub fn inspect_model(&self, model: &str) -> Result<ResolvedModel, RuntimeError> {
        Ok(self.router.inspect_model(model)?)
    }

    pub fn list_models(&self) -> Vec<String> {
        self.router.list_models()
    }

    pub fn list_authenticated_models(&self) -> Vec<String> {
        self.router.list_authenticated_models()
    }

    pub fn register_model(&mut self, entry: ModelCatalogEntry) -> Result<(), RuntimeError> {
        match Arc::get_mut(&mut self.router) {
            Some(router) => {
                router.register_model(entry);
                Ok(())
            }
            None => Err(RuntimeError::Config(
                "custom models must be registered before cloning runtime handles".into(),
            )),
        }
    }

    pub async fn chat_complete(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<lattice_core::provider::ChatResponse, RuntimeError> {
        let resolved = self.resolve(model)?;
        Ok(lattice_core::chat_complete(&resolved, messages, tools).await?)
    }

    pub async fn stream_chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<Pin<Box<dyn Stream<Item = StreamEvent> + Send>>, RuntimeError> {
        let resolved = self.resolve(model)?;
        Ok(lattice_core::chat(&resolved, messages, tools).await?)
    }

    pub fn router(&self) -> Arc<ModelRouter> {
        Arc::clone(&self.router)
    }

    pub fn session_store(&self) -> Option<Arc<dyn SessionStore>> {
        self.session_store.as_ref().map(Arc::clone)
    }

    pub fn compact_session(
        &self,
        mut session: Session,
        model: &str,
        provider_override: Option<&str>,
        recent_non_system: usize,
    ) -> Result<Session, RuntimeError> {
        let resolved = self.resolve_with_provider(model, provider_override)?;
        let parent_session_id = session.id.clone();
        let original_messages = session.messages.clone();
        let mut messages = messages_for_session(&session);
        let manager = ContextManager::new(&resolved);
        let report = manager.compact(&mut messages, recent_non_system, false);
        let retained_suffix = retained_session_suffix(&original_messages, recent_non_system);

        let boundary = CompactBoundary {
            mode: "manual".to_string(),
            parent_session_id: Some(parent_session_id),
            original_message_count: report.original_message_count,
            summarized_message_count: report.summarized_message_count,
            retained_message_count: report.retained_message_count,
            active_tokens_before: report.active_tokens_before,
            active_tokens_after: report.active_tokens_after,
            compacted_at: chrono::Local::now().to_rfc3339(),
        };

        session.parent_id = Some(session.id.clone());
        session.id = uuid::Uuid::new_v4().simple().to_string();
        session.model = resolved.canonical_id.clone();
        session.provider = resolved.provider.clone();
        session.compact_with_summary(report.summary_message.content, retained_suffix, boundary);
        session.normalize();
        Ok(session)
    }

    pub async fn dry_run_prompt(
        &self,
        model_name: &str,
        system_prompt: Option<&str>,
        prompt_text: &str,
        history: Vec<Message>,
    ) -> Result<DryRunReport, RuntimeError> {
        let resolved = self.resolve(model_name)?;

        let mut messages = Vec::new();
        if let Some(sp) = system_prompt {
            messages.push(Message {
                role: Role::System,
                content: sp.to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }
        messages.extend(history);
        messages.push(Message {
            role: Role::User,
            content: prompt_text.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });

        let total_tokens = messages.iter().map(|m| m.content.len()).sum();
        Ok(DryRunReport {
            sections: vec![],
            budgets: vec![],
            messages,
            rendered: DryRunRendered { total_tokens },
            resolved,
        })
    }
}

pub fn messages_for_session(session: &Session) -> Vec<Message> {
    session
        .messages
        .iter()
        .map(|msg| {
            let role = match msg.role.to_ascii_lowercase().as_str() {
                "assistant" => Role::Assistant,
                "system" => Role::System,
                "tool" => Role::Tool,
                _ => Role::User,
            };
            Message {
                role,
                content: msg.content.clone(),
                reasoning_content: msg.reasoning_content.clone(),
                tool_calls: msg.tool_calls.as_ref().map(|calls| {
                    calls
                        .iter()
                        .map(|call| ToolCall {
                            id: call.id.clone(),
                            function: FunctionCall {
                                name: call.name.clone(),
                                arguments: call.arguments.clone(),
                            },
                        })
                        .collect()
                }),
                tool_call_id: msg.tool_call_id.clone(),
                name: msg.name.clone(),
            }
        })
        .collect()
}

fn retained_session_suffix(
    messages: &[SessionMessage],
    recent_non_system: usize,
) -> Vec<SessionMessage> {
    let non_system_indexes = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| !message.role.eq_ignore_ascii_case("system"))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let keep_count = recent_non_system.min(non_system_indexes.len());
    let keep_start = non_system_indexes.len().saturating_sub(keep_count);
    let first_keep_index = non_system_indexes
        .get(keep_start)
        .copied()
        .unwrap_or(messages.len());

    messages
        .iter()
        .enumerate()
        .filter(|(index, message)| {
            *index >= first_keep_index
                && !message.role.eq_ignore_ascii_case("system")
                && message.compact.is_none()
        })
        .map(|(_, message)| {
            let mut message = message.clone();
            message.compact = None;
            message
        })
        .collect()
}

#[derive(Clone)]
pub struct DryRunReport {
    pub sections: Vec<DryRunSection>,
    pub budgets: Vec<prompt::TokenBudget>,
    pub messages: Vec<Message>,
    pub rendered: DryRunRendered,
    pub resolved: ResolvedModel,
}

#[derive(Clone)]
pub struct DryRunSection {
    pub layer: prompt::Layer,
    pub tokens: usize,
    pub priority: u32,
}

#[derive(Clone)]
pub struct DryRunRendered {
    pub total_tokens: usize,
}

#[derive(Default)]
pub struct RuntimeBuilder {
    config: RuntimeConfig,
    parts: RuntimeParts,
}

impl RuntimeBuilder {
    pub fn credentials(mut self, credentials: HashMap<String, String>) -> Self {
        self.config.credentials = credentials;
        self
    }

    pub fn session_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.parts.session_store = Some(store);
        self
    }

    pub fn build(self) -> Runtime {
        Runtime::new(self.config, self.parts)
    }
}

pub struct RuntimeHandle {
    runtime: Arc<Runtime>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl RuntimeHandle {
    pub fn new(runtime: Runtime) -> Self {
        Self {
            runtime: Arc::new(runtime),
            tasks: Mutex::new(Vec::new()),
        }
    }

    pub fn runtime(&self) -> Arc<Runtime> {
        Arc::clone(&self.runtime)
    }

    pub fn track(&self, task: tokio::task::JoinHandle<()>) {
        self.tasks
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .push(task);
    }

    pub fn shutdown(&self) {
        let mut tasks = self.tasks.lock().unwrap_or_else(|err| err.into_inner());
        for task in tasks.drain(..) {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_core::catalog::{ApiProtocol, CatalogProviderEntry};

    fn register_test_model(runtime: &mut Runtime, name: &str) {
        runtime
            .register_model(ModelCatalogEntry {
                canonical_id: name.to_string(),
                context_length: 8192,
                providers: vec![CatalogProviderEntry {
                    provider_id: "ollama".to_string(),
                    api_model_id: name.to_string(),
                    priority: 1,
                    credential_keys: HashMap::new(),
                    base_url: Some("http://localhost:11434".to_string()),
                    api_protocol: ApiProtocol::OpenAiChat,
                    provider_specific: HashMap::new(),
                }],
                aliases: vec![],
            })
            .unwrap();
    }

    #[test]
    fn runtime_exposes_session_store_facade() {
        let dir = std::env::temp_dir().join(format!(
            "lattice-runtime-session-store-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Arc::new(lattice_session::FileSessionStore::new(&dir));
        let runtime = Runtime::builder().session_store(store).build();

        let sessions = runtime.session_store().expect("session store should exist");
        let mut session = lattice_session::Session::new("model".into(), "provider".into());
        session.id = "runtime-session".into();
        session.push_message("user", "hello runtime".into());
        sessions.save(&session).unwrap();

        assert_eq!(
            sessions.current_session_id().unwrap().as_deref(),
            Some("runtime-session")
        );
        assert_eq!(
            sessions.current().unwrap().unwrap().title.as_deref(),
            Some("hello runtime")
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compact_session_creates_child_with_boundary_metadata() {
        let mut runtime = Runtime::builder().build();
        register_test_model(&mut runtime, "compact-test-model");
        let mut session =
            lattice_session::Session::new("compact-test-model".into(), "ollama".into());
        session.id = "compact-parent".into();
        for index in 0..6 {
            session.push_message("user", format!("request {index}"));
            session.push_message("assistant", format!("answer {index}"));
        }

        let compacted = runtime
            .compact_session(session, "compact-test-model", None, 4)
            .expect("compact should succeed");

        assert_ne!(compacted.id, "compact-parent");
        assert_eq!(compacted.parent_id.as_deref(), Some("compact-parent"));
        assert!(compacted.messages[0]
            .content
            .contains("<lattice-compact-summary"));
        let boundary = compacted.messages[0].compact.as_ref().unwrap();
        assert_eq!(boundary.mode, "manual");
        assert_eq!(
            boundary.parent_session_id.as_deref(),
            Some("compact-parent")
        );
        assert!(boundary.summarized_message_count > 0);
    }

    #[test]
    fn messages_for_session_preserves_tool_fields() {
        let mut session = lattice_session::Session::new("model".into(), "provider".into());
        session.messages.push(lattice_session::SessionMessage {
            role: "assistant".into(),
            content: String::new(),
            reasoning_content: Some("thinking".into()),
            tool_calls: Some(vec![lattice_session::SessionToolCall {
                id: "call_1".into(),
                name: "grep".into(),
                arguments: r#"{"pattern":"x"}"#.into(),
            }]),
            tool_call_id: None,
            name: None,
            artifact_ids: Vec::new(),
            compact: None,
        });
        session.messages.push(lattice_session::SessionMessage {
            role: "tool".into(),
            content: "result".into(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some("call_1".into()),
            name: Some("grep".into()),
            artifact_ids: Vec::new(),
            compact: None,
        });

        let messages = messages_for_session(&session);

        assert_eq!(messages[0].role, Role::Assistant);
        assert_eq!(messages[0].reasoning_content.as_deref(), Some("thinking"));
        assert_eq!(
            messages[0].tool_calls.as_ref().unwrap()[0].function.name,
            "grep"
        );
        assert_eq!(messages[1].role, Role::Tool);
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn test_approve_valid_command_rule() {
        let shared = Arc::new(Mutex::new(Vec::new()));
        let security = RuntimeSecurity {
            mode_label: "strict".into(),
            sandbox: SandboxConfig::default(),
            approved_rules: Arc::clone(&shared),
            policy_authority: PolicyAuthority::from_shared_rules(Arc::clone(&shared)),
            audit_log: None,
        };
        let rule = ApprovedRule {
            kind: ApprovalKind::CommandPrefix {
                prefix: "cargo publish".to_string(),
            },
            risk_class: lattice_sandbox::RiskClass::Mutate,
        };
        assert!(security.approve_rule(rule).is_ok());
        assert_eq!(security.approved_rules.lock().unwrap().len(), 1);
    }

    #[test]
    fn test_approve_invalid_single_token_rejected() {
        let shared = Arc::new(Mutex::new(Vec::new()));
        let security = RuntimeSecurity {
            mode_label: "strict".into(),
            sandbox: SandboxConfig::default(),
            approved_rules: Arc::clone(&shared),
            policy_authority: PolicyAuthority::from_shared_rules(Arc::clone(&shared)),
            audit_log: None,
        };
        let rule = ApprovedRule {
            kind: ApprovalKind::CommandPrefix {
                prefix: "cargo".to_string(),
            },
            risk_class: lattice_sandbox::RiskClass::Mutate,
        };
        let result = security.approve_rule(rule);
        assert!(matches!(result, Err(ValidationError::TooFewTokens { .. })));
    }

    #[test]
    fn test_runtime_security_shared_state() {
        let workdir =
            std::env::temp_dir().join(format!("lattice-shared-state-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&workdir);
        let security = build_runtime_security(&RuntimeSecurityConfig::default(), &workdir).unwrap();

        security
            .approve_rule(ApprovedRule {
                kind: ApprovalKind::CommandPrefix {
                    prefix: "npm run build".into(),
                },
                risk_class: lattice_sandbox::RiskClass::Mutate,
            })
            .expect("approve_rule should succeed");

        assert_eq!(security.approved_rules.lock().unwrap().len(), 1);
        assert!(security.sandbox.approved_rules.is_some());
        assert_eq!(
            security
                .sandbox
                .approved_rules
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .len(),
            1
        );
        assert!(Arc::ptr_eq(
            &security.approved_rules,
            security.sandbox.approved_rules.as_ref().unwrap()
        ));

        let _ = std::fs::remove_dir_all(&workdir);
    }
}
