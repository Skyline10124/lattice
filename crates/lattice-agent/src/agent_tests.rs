use super::*;
use async_trait::async_trait;
use lattice_core::catalog::ApiProtocol;
use lattice_core::types::Role;
use lattice_core::ResolvedModel;
use lattice_hooks::{
    PromptHook, PromptHookContext, PromptHookDecision, PromptHookOutcome, PromptHookPoint,
};
use std::collections::HashMap;

fn make_resolved(context_length: u32) -> ResolvedModel {
    ResolvedModel {
        canonical_id: "test".into(),
        api_model_id: "test".into(),
        provider: "test".into(),
        base_url: "http://localhost".to_string(),
        api_key: Some("sk-test".into()),
        api_protocol: ApiProtocol::OpenAiChat,
        context_length,
        credential_status: lattice_core::CredentialStatus::Present,
        provider_specific: HashMap::new(),
    }
}

#[test]
fn test_set_system_prompt_does_not_panic() {
    let resolved = make_resolved(4096);
    let mut agent = Agent::new(resolved);
    agent.set_system_prompt("first");
    agent.set_system_prompt("second");
    // If we get here without panic, the inherent method resolved correctly
}

/// A test provider that produces a fixed section at a given layer/priority.
struct TestProvider {
    layer: prompt::Layer,
    priority: u8,
    content: &'static str,
    tokens: u32,
    budget: prompt::TokenBudget,
}

#[async_trait]
impl prompt::ContextProvider for TestProvider {
    fn layer(&self) -> prompt::Layer {
        self.layer
    }
    fn priority(&self) -> u8 {
        self.priority
    }
    fn budget(&self) -> prompt::TokenBudget {
        self.budget
    }
    async fn produce(&self, _ctx: &prompt::AssemblyContext<'_>) -> Option<prompt::PromptSection> {
        Some(prompt::PromptSection {
            content: self.content.to_string(),
            layer: self.layer,
            priority: self.priority,
            tokens: self.tokens,
        })
    }
}

struct AddSectionHook;

#[async_trait]
impl PromptHook for AddSectionHook {
    fn name(&self) -> &str {
        "add-section"
    }

    fn points(&self) -> Vec<PromptHookPoint> {
        vec![PromptHookPoint::PostContextCollect]
    }

    async fn execute(
        &self,
        _context: &PromptHookContext,
    ) -> Result<PromptHookDecision, lattice_hooks::HookError> {
        Ok(PromptHookDecision::AddSection {
            section: prompt::PromptSection::new(
                "hook-injected context",
                prompt::Layer::Events,
                1,
                3,
            ),
            budget: prompt::TokenBudget::Fixed(3),
        })
    }
}

async fn dispatch_test_prompt_hook(agent: &Agent, context: PromptHookContext) -> PromptHookOutcome {
    agent.prompt_hooks.dispatch(context).await.unwrap()
}

#[tokio::test]
async fn test_agent_prompt_compilation_integration() {
    // Verify that set_system_prompt + registered provider produces
    // correctly ordered messages through the engine.
    let resolved = make_resolved(16384);
    let mut agent = Agent::new(resolved);

    agent.set_system_prompt("You are a code review assistant.");

    agent = agent.with_provider(TestProvider {
        layer: prompt::Layer::Tools,
        priority: 5,
        content: "read_file, grep, patch",
        tokens: 8,
        budget: prompt::TokenBudget::Dynamic,
    });

    // Simulate what Agent::run() does internally (no network call)
    let ctx = prompt::AssemblyContext {
        request_id: "test",
        model: &agent.state.resolved,
        user_input: "test",
    };
    let contributions = agent.registry.collect_contributions(&ctx).await;
    let rendered = prompt::compiler::compile_contributions(
        &contributions,
        "review this file",
        &agent.state.resolved,
    )
    .unwrap();

    // System message with plain content
    let sys = rendered
        .messages
        .iter()
        .find(|m| m.role == Role::System)
        .unwrap();
    assert!(sys.content.contains("You are a code review assistant."));

    // User message with Tools + Input markers
    let user = rendered
        .messages
        .iter()
        .find(|m| m.role == Role::User)
        .unwrap();
    assert!(user.content.contains("layer=\"tools\""));
    assert!(user.content.contains("layer=\"input\""));
    assert!(user.content.contains("read_file, grep, patch"));
    assert!(user.content.contains("review this file"));

    // Verify what gets pushed to state
    for msg in &rendered.messages {
        match msg.role {
            Role::System => agent.state.push_system_message(&msg.content),
            Role::User => agent.state.push_user_message(&msg.content),
            _ => {}
        }
    }
    // System message at the front
    assert_eq!(agent.state.messages[0].role, Role::System);
    assert!(agent.state.messages[0]
        .content
        .contains("You are a code review assistant."));
    // User message after
    assert_eq!(agent.state.messages[1].role, Role::User);
}

#[tokio::test]
async fn test_prompt_hook_can_add_context_before_compile() {
    let resolved = make_resolved(16384);
    let agent = Agent::new(resolved).with_prompt_hook(AddSectionHook);

    let ctx = prompt::AssemblyContext {
        request_id: "test",
        model: &agent.state.resolved,
        user_input: "test",
    };
    let contributions = agent.registry.collect_contributions(&ctx).await;
    let outcome = dispatch_test_prompt_hook(
        &agent,
        PromptHookContext::new(PromptHookPoint::PostContextCollect, "test", "hello")
            .with_contributions(contributions),
    )
    .await;

    let rendered = prompt::compiler::compile_contributions(
        &outcome.contributions,
        "hello",
        &agent.state.resolved,
    )
    .unwrap();
    let user = rendered
        .messages
        .iter()
        .find(|m| m.role == Role::User)
        .unwrap();

    assert!(user.content.contains("hook-injected context"));
    assert!(user.content.contains("layer=\"events\""));
}

#[tokio::test]
async fn test_empty_registry_falls_through_to_raw_input() -> Result<(), Box<dyn std::error::Error>>
{
    // No agent prompt and no providers should still include the base system
    // prompt, while keeping the user input unwrapped.
    let resolved = make_resolved(16384);
    let agent = Agent::new(resolved);

    let ctx = prompt::AssemblyContext {
        request_id: "test",
        model: &agent.state.resolved,
        user_input: "test",
    };
    let contributions = agent.registry.collect_contributions(&ctx).await;
    let rendered =
        prompt::compiler::compile_contributions(&contributions, "hello", &agent.state.resolved)?;

    assert_eq!(rendered.messages.len(), 2);
    assert_eq!(rendered.messages[0].role, Role::System);
    assert!(rendered.messages[0]
        .content
        .contains("You are a LATTICE agent."));
    assert_eq!(rendered.messages[1].role, Role::User);
    assert_eq!(rendered.messages[1].content, "hello");
    Ok(())
}

#[tokio::test]
async fn test_budget_overflow_drops_low_priority_provider() {
    // Tight budget should drop non-essential sections.
    let resolved = make_resolved(5000); // effective: 5000 - 4096 = 904
    let mut agent = Agent::new(resolved);

    agent.set_system_prompt("S");
    // Use a large-token provider at Events layer
    struct BigProvider;
    #[async_trait]
    impl prompt::ContextProvider for BigProvider {
        fn layer(&self) -> prompt::Layer {
            prompt::Layer::Events
        }
        fn priority(&self) -> u8 {
            5
        }
        fn budget(&self) -> prompt::TokenBudget {
            prompt::TokenBudget::Dynamic
        }
        async fn produce(
            &self,
            _ctx: &prompt::AssemblyContext<'_>,
        ) -> Option<prompt::PromptSection> {
            Some(prompt::PromptSection {
                content: "x".repeat(2000),
                layer: prompt::Layer::Events,
                priority: 5,
                tokens: 2000,
            })
        }
    }
    agent = agent.with_provider(BigProvider);

    let ctx = prompt::AssemblyContext {
        request_id: "test",
        model: &agent.state.resolved,
        user_input: "test",
    };
    let contributions = agent.registry.collect_contributions(&ctx).await;
    let rendered =
        prompt::compiler::compile_contributions(&contributions, "hi", &agent.state.resolved)
            .unwrap();

    // Events (2000 tokens > 904 effective) should be dropped
    let user = rendered
        .messages
        .iter()
        .find(|m| m.role == Role::User)
        .unwrap();
    assert!(!user.content.contains("layer=\"events\""));
    assert!(user.content.contains("<lattice-context-manifest>"));
    // System message survives
    let sys = rendered
        .messages
        .iter()
        .find(|m| m.role == Role::System)
        .unwrap();
    assert!(sys.content.contains("You are a LATTICE agent."));
    assert!(sys.content.contains("S"));
}

// -----------------------------------------------------------------------
// Integration tests: context management layer with blob-store
// -----------------------------------------------------------------------

#[cfg(feature = "blob-store")]
#[tokio::test]
async fn test_agent_with_blob_store_and_bus_events() {
    let resolved = make_resolved(16384);
    let store = std::sync::Arc::new(
        crate::blob::BlobStore::connect("sqlite::memory:")
            .await
            .unwrap(),
    );

    let small_event = ContextEvent::new(
        "audit-pass",
        "auditor",
        serde_json::json!({"status": "clean"}),
    );
    let large_event = ContextEvent::new(
        "inspection",
        "auditor",
        serde_json::json!({"lines": vec![String::from("x").repeat(5000)]}),
    );

    let agent = Agent::new(resolved)
        .with_blob_store(std::sync::Arc::clone(&store))
        .with_provider(
            crate::events_provider::EventsProvider::new("test-source")
                .with_blob_store(store)
                .with_events(vec![small_event, large_event]),
        );

    // Simulate prompt compilation (no network call)
    let ctx = prompt::AssemblyContext {
        request_id: "test",
        model: &agent.state.resolved,
        user_input: "test",
    };
    let contributions = agent.registry.collect_contributions(&ctx).await;
    let rendered = prompt::compiler::compile_contributions(
        &contributions,
        "review these events",
        &agent.state.resolved,
    )
    .unwrap();

    let user = rendered
        .messages
        .iter()
        .find(|m| m.role == Role::User)
        .unwrap();

    // Small event inlined
    assert!(user.content.contains("[topic: audit-pass]"));
    // Large event as blob reference
    assert!(user.content.contains("blob://test-source/inspection/"));
}

#[cfg(feature = "blob-store")]
#[tokio::test]
async fn test_bus_fetch_tool_execution() {
    let store = crate::blob::BlobStore::connect("sqlite::memory:")
        .await
        .unwrap();
    let blob = crate::blob::StoredBlob {
        key: "blob://test/data/abc123".to_string(),
        source: "test".to_string(),
        topic: "data".to_string(),
        mime: "application/json".to_string(),
        size: 100,
        payload: "{\"result\": \"ok\"}".to_string(),
        summary: "".to_string(),
    };
    store.insert(&blob).await.unwrap();

    let executor = lattice_tools::DefaultToolExecutor::new_with_blob_store(
        "/tmp",
        Some(std::sync::Arc::new(store)),
    )
    .unwrap();

    let call = lattice_core::types::ToolCall {
        id: "call-1".to_string(),
        function: lattice_core::types::FunctionCall {
            name: "bus:fetch".to_string(),
            arguments: "{\"key\": \"blob://test/data/abc123\"}".to_string(),
        },
    };
    let result = executor.execute(&call).await;
    assert_eq!(result, "{\"result\": \"ok\"}");
}

#[cfg(feature = "blob-store")]
#[tokio::test]
async fn test_bus_fetch_not_found() {
    let store = crate::blob::BlobStore::connect("sqlite::memory:")
        .await
        .unwrap();
    let executor = lattice_tools::DefaultToolExecutor::new_with_blob_store(
        "/tmp",
        Some(std::sync::Arc::new(store)),
    )
    .unwrap();

    let call = lattice_core::types::ToolCall {
        id: "call-2".to_string(),
        function: lattice_core::types::FunctionCall {
            name: "bus:fetch".to_string(),
            arguments: "{\"key\": \"blob://test/nonexistent/abc\"}".to_string(),
        },
    };
    let result = executor.execute(&call).await;
    assert!(result.contains("not found"));
}

#[cfg(feature = "blob-store")]
#[tokio::test]
async fn test_bus_fetch_without_blob_store() {
    let executor = lattice_tools::DefaultToolExecutor::new_with_blob_store("/tmp", None).unwrap();
    let call = lattice_core::types::ToolCall {
        id: "call-3".to_string(),
        function: lattice_core::types::FunctionCall {
            name: "bus:fetch".to_string(),
            arguments: "{\"key\": \"blob://any/key/here\"}".to_string(),
        },
    };
    let result = executor.execute(&call).await;
    assert!(result.contains("not configured"));
}

#[cfg(feature = "blob-store")]
#[tokio::test]
async fn test_degradation_without_blob_store() -> Result<(), Box<dyn std::error::Error>> {
    // Agent without blob_store: large events silently skipped
    let resolved = make_resolved(16384);
    // All events are large — will be skipped with no blob_store
    let large_event = ContextEvent::new(
        "big-data",
        "source",
        serde_json::json!({"data": String::from("x").repeat(5000)}),
    );
    let agent = Agent::new(resolved).with_provider(
        crate::events_provider::EventsProvider::new("test-source").with_events(vec![large_event]),
    );

    let ctx = prompt::AssemblyContext {
        request_id: "test",
        model: &agent.state.resolved,
        user_input: "test",
    };
    let contributions = agent.registry.collect_contributions(&ctx).await;
    let rendered =
        prompt::compiler::compile_contributions(&contributions, "check", &agent.state.resolved)?;

    // No Events section should appear (all events skipped)
    let user = rendered.messages.iter().find(|m| m.role == Role::User);
    if let Some(user_msg) = user {
        assert!(!user_msg.content.contains("layer=\"events\""));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Task / recovery state integration tests
// ---------------------------------------------------------------------------

use super::{TaskState, TaskStateError, TodoItem, TodoStatus, TurnManifestObject};

#[test]
fn task_state_serde_preserves_todo_ordering() {
    let mut state = TaskState::new();
    state
        .replace_todos(vec![
            TodoItem {
                id: 0,
                content: "Step 1: Inspect".into(),
                status: TodoStatus::Completed,
            },
            TodoItem {
                id: 0,
                content: "Step 2: Fix bug".into(),
                status: TodoStatus::InProgress,
            },
            TodoItem {
                id: 0,
                content: "Step 3: Run tests".into(),
                status: TodoStatus::Pending,
            },
            TodoItem {
                id: 0,
                content: "Step 4: Review".into(),
                status: TodoStatus::Pending,
            },
        ])
        .unwrap();
    state.record_changed_file("src/auth.rs".into(), "update".into(), None);
    state.record_changed_file("src/lib.rs".into(), "add".into(), Some("new pub fn".into()));
    state.record_tool_failure(
        "tc-1".into(),
        "bash".into(),
        "command failed: bad_arg".into(),
    );
    state.record_verification_failure("cargo test".into(), "2 failures".into());
    state.mark_unverified("src/auth.rs".into());
    state.mark_unverified("cargo test".into());

    let json = serde_json::to_string_pretty(&state).unwrap();
    let round: TaskState = serde_json::from_str(&json).unwrap();

    assert_eq!(round.todos.len(), 4);
    assert_eq!(round.todos[0].id, 1);
    assert_eq!(round.todos[2].id, 3);
    assert_eq!(round.todos[1].status, TodoStatus::InProgress);
    assert_eq!(round.todos[1].content, "Step 2: Fix bug");

    assert_eq!(round.changed_files.len(), 2);
    assert_eq!(round.changed_files[0].path, "src/auth.rs");

    assert_eq!(round.tool_failures.len(), 1);
    assert!(round.tool_failures[0].message.contains("bad_arg"));

    assert_eq!(round.verifications.len(), 1);
    assert_eq!(round.verifications[0].command, "cargo test");
    assert!(round.verifications[0].output_trimmed.contains("2 failures"));

    assert_eq!(round.unverified_work.len(), 2);
    assert_eq!(round.unverified_work[0], "src/auth.rs");

    assert_eq!(round, state);
}

#[test]
fn turn_manifest_metadata_round_trips_task_state() {
    let mut state = TaskState::new();
    state
        .replace_todos(vec![TodoItem {
            id: 0,
            content: "Fix auth bug".into(),
            status: TodoStatus::InProgress,
        }])
        .unwrap();
    state.record_changed_file("src/auth.rs".into(), "update".into(), None);
    state.record_verification_failure("cargo test auth".into(), "FAILED".into());
    state.mark_unverified("cargo test auth".into());

    let metadata = serde_json::to_value(&state).unwrap();

    let manifest = TurnManifestObject {
        hook_decisions: Vec::new(),
        tool_decisions: Vec::new(),
        prompt_snapshot: super::PromptSnapshot {
            content_hash: "abc".into(),
            token_count: 0,
            model: "test".into(),
            turn_index: 0,
            rendered_fragments: Vec::new(),
            withheld_fragments: Vec::new(),
        },
        session_id: "s1".into(),
        branch: "main".into(),
        parent_commit_id: String::new(),
        request_id: "r1".into(),
        metadata: Some(metadata),
    };

    let meta = manifest.metadata.as_ref().unwrap();
    assert!(!meta.is_null());

    let round: TaskState = serde_json::from_value(meta.clone()).unwrap();
    assert_eq!(round.todos.len(), 1);
    assert_eq!(round.todos[0].content, "Fix auth bug");
    assert_eq!(round.todos[0].status, TodoStatus::InProgress);
    assert_eq!(round.changed_files.len(), 1);
    assert_eq!(round.changed_files[0].path, "src/auth.rs");
    assert_eq!(round.verifications.len(), 1);
    assert_eq!(round.verifications[0].command, "cargo test auth");
    assert_eq!(round.unverified_work.len(), 1);
}

#[test]
fn empty_task_state_produces_null_manifest_metadata() {
    let state = TaskState::new();
    assert!(state.is_empty());

    let manifest = TurnManifestObject {
        hook_decisions: Vec::new(),
        tool_decisions: Vec::new(),
        prompt_snapshot: super::PromptSnapshot {
            content_hash: "abc".into(),
            token_count: 0,
            model: "test".into(),
            turn_index: 0,
            rendered_fragments: Vec::new(),
            withheld_fragments: Vec::new(),
        },
        session_id: "s1".into(),
        branch: "main".into(),
        parent_commit_id: String::new(),
        request_id: "r1".into(),
        metadata: None,
    };

    assert!(manifest.metadata.is_none());
}

#[test]
fn task_state_with_failed_verification_captures_command() {
    let mut state = TaskState::new();
    state.record_verification_failure("cargo test --lib".into(), "FAILED".into());

    let json = serde_json::to_value(&state).unwrap();
    let round: TaskState = serde_json::from_value(json).unwrap();

    assert_eq!(round.verifications.len(), 1);
    assert_eq!(round.verifications[0].command, "cargo test --lib");
    assert!(!round.verifications[0].timestamp.is_empty());
    assert!(round.verifications[0].output_trimmed.contains("FAILED"));
}

#[test]
fn task_state_with_only_todos_is_not_empty() {
    let mut state = TaskState::new();
    state
        .replace_todos(vec![TodoItem {
            id: 0,
            content: "Single task".into(),
            status: TodoStatus::Pending,
        }])
        .unwrap();

    assert!(!state.is_empty());
}

#[test]
fn task_state_with_only_changed_files_is_not_empty() {
    let mut state = TaskState::new();
    state.record_changed_file("src/main.rs".into(), "add".into(), None);

    assert!(!state.is_empty());
}

#[test]
fn task_state_with_only_tool_failure_is_not_empty() {
    let mut state = TaskState::new();
    state.record_tool_failure("tc-1".into(), "grep".into(), "no match".into());

    assert!(!state.is_empty());
}

#[test]
fn task_state_with_only_unverified_is_not_empty() {
    let mut state = TaskState::new();
    state.mark_unverified("needs review".into());

    assert!(!state.is_empty());
}

#[test]
fn task_state_rejects_duplicate_inprogress_on_replace() {
    let mut state = TaskState::new();
    state
        .replace_todos(vec![
            TodoItem {
                id: 0,
                content: "A".into(),
                status: TodoStatus::InProgress,
            },
            TodoItem {
                id: 0,
                content: "B".into(),
                status: TodoStatus::Pending,
            },
        ])
        .unwrap();

    let err = state
        .replace_todos(vec![
            TodoItem {
                id: 0,
                content: "A".into(),
                status: TodoStatus::InProgress,
            },
            TodoItem {
                id: 0,
                content: "B".into(),
                status: TodoStatus::InProgress,
            },
        ])
        .unwrap_err();

    assert_eq!(err, TaskStateError::MultipleInProgress);
    assert_eq!(state.todos.len(), 2);
    assert_eq!(state.todos[0].status, TodoStatus::InProgress);
}
