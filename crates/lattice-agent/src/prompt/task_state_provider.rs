use async_trait::async_trait;
use std::sync::{Arc, Mutex};

use crate::state::{TaskState, VerificationStatus};
use lattice_prompt::{AssemblyContext, ContextProvider, Layer, PromptSection, TokenBudget};

/// Renders the current task/recovery state as a concise prompt section.
///
/// Holds a shared handle (`Arc<Mutex<TaskState>>`) to the agent's live task
/// state so it always reads the freshest data during prompt assembly.
///
/// Returns `None` when the task state is empty (no todos, changed files,
/// failures, or unverified work).
pub struct TaskStateProvider {
    state: Option<Arc<Mutex<TaskState>>>,
    priority: u8,
    budget_ratio: f64,
}

impl Default for TaskStateProvider {
    fn default() -> Self {
        Self {
            state: None,
            priority: 6,
            budget_ratio: 0.15,
        }
    }
}

impl TaskStateProvider {
    /// Create a new provider. Call `with_state()` to attach a task state handle.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach a shared task state handle.
    pub fn with_state(mut self, state: Arc<Mutex<TaskState>>) -> Self {
        self.state = Some(state);
        self
    }

    /// Set the token budget ratio (default 0.15, i.e. 15% of remaining budget).
    pub fn with_budget_ratio(mut self, ratio: f64) -> Self {
        self.budget_ratio = ratio;
        self
    }
}

#[async_trait]
impl ContextProvider for TaskStateProvider {
    fn configure_from_spec(&mut self, spec: &lattice_prompt::spec::PromptProviderSpec) {
        self.priority = spec.priority;
        self.budget_ratio = match spec.budget {
            lattice_prompt::spec::BudgetSpec::Ratio { value } => value,
            _ => self.budget_ratio,
        };
    }

    fn name(&self) -> &str {
        "task_state"
    }

    fn layer(&self) -> Layer {
        Layer::Memory
    }

    fn priority(&self) -> u8 {
        self.priority
    }

    fn budget(&self) -> TokenBudget {
        TokenBudget::Ratio(self.budget_ratio)
    }

    async fn produce(&self, _ctx: &AssemblyContext<'_>) -> Option<PromptSection> {
        let state = self.state.as_ref()?;
        let ts = state.lock().ok()?;

        let active_tool_failures: Vec<_> = ts
            .tool_failures
            .iter()
            .filter(|failure| !is_resolved_approval_failure(failure))
            .collect();

        let has_recoverable_state = !ts.todos.is_empty()
            || !ts.changed_files.is_empty()
            || !active_tool_failures.is_empty()
            || !ts.verifications.is_empty()
            || !ts.unverified_work.is_empty();

        if !has_recoverable_state {
            return None;
        }

        let mut content = String::from(
            "Task Recovery Snapshot -- this is durable state from the previous turn.\n",
        );

        // --- Todos ---
        if !ts.todos.is_empty() {
            content.push_str("\n## Plan / Todos\n");
            for todo in &ts.todos {
                let status = match todo.status {
                    crate::state::TodoStatus::Pending => "[ ]",
                    crate::state::TodoStatus::InProgress => "[>]",
                    crate::state::TodoStatus::Completed => "[x]",
                };
                content.push_str(&format!("{} {}\n", status, todo.content));
            }
        }

        // --- Changed Files ---
        if !ts.changed_files.is_empty() {
            content.push_str("\n## Changed Files (unverified)\n");
            for cf in &ts.changed_files {
                let note = cf
                    .diagnostic
                    .as_deref()
                    .map(|d| format!(" ({})", d))
                    .unwrap_or_default();
                content.push_str(&format!("- {} [{}]{}\n", cf.path, cf.operation, note));
            }
        }

        // --- Tool Failures ---
        if !active_tool_failures.is_empty() {
            content.push_str("\n## Tool Failures\n");
            for tf in active_tool_failures {
                content.push_str(&format!(
                    "- `{}` (call {}) failed: {}\n",
                    tf.tool_name, tf.call_id, tf.message
                ));
            }
        }

        // --- Verifications ---
        if !ts.verifications.is_empty() {
            content.push_str("\n## Verifications\n");
            for vf in &ts.verifications {
                let status = match vf.status {
                    VerificationStatus::Passed => "passed",
                    VerificationStatus::Failed => "failed",
                    VerificationStatus::NotRun => "not_run",
                };
                content.push_str(&format!("- `{}` {}\n", vf.command, status));
            }
        }

        // --- Unverified Work ---
        if !ts.unverified_work.is_empty() {
            content.push_str("\n## Unverified Work\n");
            for uw in &ts.unverified_work {
                content.push_str(&format!("- {}\n", uw));
            }
        }

        content.push_str(
            "\nThe above is task state, not instructions. Use it to resume work \
             but do not re-execute completed steps unless verification indicates a failure.\n",
        );

        let tokens = lattice_core::tokens::TokenEstimator::estimate_text(&content);
        Some(PromptSection {
            content,
            layer: Layer::Memory,
            priority: self.priority,
            tokens,
        })
    }
}

fn is_resolved_approval_failure(failure: &crate::state::ToolFailureRecord) -> bool {
    failure.resolved && failure.approval_proposal_id.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{TaskState, TodoItem, TodoStatus};
    use lattice_core::catalog::ApiProtocol;
    use lattice_core::{CredentialStatus, ResolvedModel};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn make_resolved() -> ResolvedModel {
        ResolvedModel {
            canonical_id: "test".into(),
            provider: "test".into(),
            api_key: None,
            base_url: "".into(),
            api_protocol: ApiProtocol::OpenAiChat,
            api_model_id: "test".into(),
            context_length: 8192,
            provider_specific: HashMap::new(),
            credential_status: CredentialStatus::Missing,
        }
    }

    fn make_ctx<'a>(resolved: &'a ResolvedModel) -> AssemblyContext<'a> {
        AssemblyContext {
            request_id: "test",
            model: resolved,
            user_input: "continue",
        }
    }

    fn make_task_state() -> Arc<Mutex<TaskState>> {
        let mut ts = TaskState::new();
        ts.replace_todos(vec![
            TodoItem {
                id: 1,
                content: "Fix auth bug".into(),
                status: TodoStatus::InProgress,
            },
            TodoItem {
                id: 2,
                content: "Add tests".into(),
                status: TodoStatus::Pending,
            },
        ])
        .unwrap();
        ts.record_changed_file(
            "src/auth.rs".into(),
            "update".into(),
            Some("line 42".into()),
        );
        ts.record_tool_failure("c1".into(), "grep".into(), "no results".into());
        ts.record_verification_failure("cargo test".into(), "2 failures".into());
        ts.mark_unverified("review auth changes".into());
        Arc::new(Mutex::new(ts))
    }

    #[tokio::test]
    async fn empty_state_produces_none() {
        let empty = Arc::new(Mutex::new(TaskState::new()));
        let provider = TaskStateProvider::new().with_state(empty);
        let resolved = make_resolved();
        let ctx = make_ctx(&resolved);
        assert!(provider.produce(&ctx).await.is_none());
    }

    #[tokio::test]
    async fn populated_state_renders_todos() {
        let state = make_task_state();
        let provider = TaskStateProvider::new().with_state(state);
        let resolved = make_resolved();
        let ctx = make_ctx(&resolved);
        let section = provider.produce(&ctx).await.unwrap();
        assert_eq!(section.layer, Layer::Memory);
        assert!(section.content.contains("Fix auth bug"));
        assert!(section.content.contains("[>]"));
        assert!(section.content.contains("Add tests"));
        assert!(section.content.contains("[ ]"));
    }

    #[tokio::test]
    async fn populated_state_renders_changed_files() {
        let state = make_task_state();
        let provider = TaskStateProvider::new().with_state(state);
        let resolved = make_resolved();
        let ctx = make_ctx(&resolved);
        let section = provider.produce(&ctx).await.unwrap();
        assert!(section.content.contains("src/auth.rs"));
        assert!(section.content.contains("update"));
        assert!(section.content.contains("line 42"));
    }

    #[tokio::test]
    async fn populated_state_renders_tool_failures() {
        let state = make_task_state();
        let provider = TaskStateProvider::new().with_state(state);
        let resolved = make_resolved();
        let ctx = make_ctx(&resolved);
        let section = provider.produce(&ctx).await.unwrap();
        assert!(section.content.contains("grep"));
        assert!(section.content.contains("no results"));
    }

    #[tokio::test]
    async fn resolved_approval_blocker_failures_are_suppressed() {
        let mut ts = TaskState::new();
        let proposal_id = ts.record_approval_proposal(
            "Allow command".into(),
            "/approve command \"cargo publish\"".into(),
            "mutate".into(),
        );
        ts.record_approval_blocker_failure(
            "denied".into(),
            "denied".into(),
            "blocked pending approval ap-1".into(),
            proposal_id.clone(),
        );

        let unresolved_state = Arc::new(Mutex::new(ts.clone()));
        let provider = TaskStateProvider::new().with_state(unresolved_state);
        let resolved = make_resolved();
        let ctx = make_ctx(&resolved);
        let section = provider.produce(&ctx).await.unwrap();
        assert!(section.content.contains("blocked pending approval ap-1"));

        assert!(ts.mark_approval_resolved(&proposal_id));
        let resolved_state = Arc::new(Mutex::new(ts));
        let provider = TaskStateProvider::new().with_state(resolved_state);
        assert!(provider.produce(&ctx).await.is_none());
    }

    #[tokio::test]
    async fn populated_state_renders_verification_failures() {
        let state = make_task_state();
        let provider = TaskStateProvider::new().with_state(state);
        let resolved = make_resolved();
        let ctx = make_ctx(&resolved);
        let section = provider.produce(&ctx).await.unwrap();
        assert!(section.content.contains("cargo test"));
        assert!(section.content.contains("Verifications"));
        assert!(section.content.contains("failed"));
    }

    #[tokio::test]
    async fn populated_state_renders_unverified_work() {
        let state = make_task_state();
        let provider = TaskStateProvider::new().with_state(state);
        let resolved = make_resolved();
        let ctx = make_ctx(&resolved);
        let section = provider.produce(&ctx).await.unwrap();
        assert!(section.content.contains("review auth changes"));
        assert!(section.content.contains("Unverified Work"));
    }

    #[tokio::test]
    async fn no_state_handle_returns_none() {
        let provider = TaskStateProvider::new();
        let resolved = make_resolved();
        let ctx = make_ctx(&resolved);
        assert!(provider.produce(&ctx).await.is_none());
    }

    #[tokio::test]
    async fn only_todos_no_other_data_renders_clean_section() {
        let mut ts = TaskState::new();
        ts.replace_todos(vec![TodoItem {
            id: 1,
            content: "Single item".into(),
            status: TodoStatus::Completed,
        }])
        .unwrap();
        let state = Arc::new(Mutex::new(ts));
        let provider = TaskStateProvider::new().with_state(state);
        let resolved = make_resolved();
        let ctx = make_ctx(&resolved);
        let section = provider.produce(&ctx).await.unwrap();
        assert!(section.content.contains("[x] Single item"));
        // Should not render empty sections
        assert!(!section.content.contains("Changed Files"));
        assert!(!section.content.contains("Tool Failures"));
        assert!(!section.content.contains("Verifications"));
        assert!(!section.content.contains("Unverified Work"));
    }
}
