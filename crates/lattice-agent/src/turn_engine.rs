use lattice_core::types::{Role, ToolCall};
use lattice_hooks::{PromptHookContext, PromptHookPoint};

use super::manifest::{emit_turn_manifest, ManifestCollector};
use super::{prompt, Agent, LoopEvent, SessionContext, MAX_STREAM_RETRIES};

fn collect_tool_calls(events: &[LoopEvent]) -> Vec<ToolCall> {
    let mut tool_calls = Vec::new();
    for event in events {
        if let LoopEvent::ToolCallRequired { calls } = event {
            tool_calls.extend(calls.clone());
        }
    }
    tool_calls
}

fn capture_non_empty_task_metadata(agent: &Agent, collector: &mut ManifestCollector) {
    let task_state = agent.state.task_state();
    if task_state.is_empty() {
        return;
    }
    if let Ok(meta) = serde_json::to_value(&*task_state) {
        collector.set_task_metadata(meta);
    }
}

pub(super) struct TurnEngine<'agent, 'emit, F>
where
    F: FnMut(LoopEvent),
{
    agent: &'agent mut Agent,
    emit: &'emit mut F,
    retry_stream_errors: bool,
    user_input: String,
    user_message_start: usize,
    all_events: Vec<LoopEvent>,
}

impl<'agent, 'emit, F> TurnEngine<'agent, 'emit, F>
where
    F: FnMut(LoopEvent),
{
    pub(super) fn new(
        agent: &'agent mut Agent,
        content: &str,
        emit: &'emit mut F,
        retry_stream_errors: bool,
    ) -> Self {
        let user_message_start = agent.state.messages.len();
        Self {
            agent,
            emit,
            retry_stream_errors,
            user_input: content.to_string(),
            user_message_start,
            all_events: Vec::new(),
        }
    }

    fn emit_and_record(&mut self, event: LoopEvent) {
        (self.emit)(event.clone());
        self.all_events.push(event);
    }

    async fn run_chat_with_retries(&mut self) -> Vec<LoopEvent> {
        let chat_snapshot = self.agent.state.snapshot();
        let mut events = self.agent.run_chat_with_observer(self.emit).await;

        let mut retry_count = 0u32;
        while self.retry_stream_errors && retry_count < MAX_STREAM_RETRIES {
            let has_retryable_error = events.iter().any(|e| {
                matches!(
                    e,
                    LoopEvent::Error {
                        retryable: true,
                        ..
                    }
                )
            });
            if !has_retryable_error {
                break;
            }
            self.agent.state.restore(chat_snapshot.clone());
            retry_count += 1;
            events = self.agent.run_chat_with_observer(self.emit).await;
        }

        events
    }

    pub(super) async fn run(mut self, max_turns: u32) -> Vec<LoopEvent> {
        for turn_counter in 0_u32..max_turns {
            // Per-turn manifest collector (D-01, D-02, D-03)
            let mut collector = ManifestCollector::new(
                self.agent
                    .session_context
                    .clone()
                    .unwrap_or(SessionContext {
                        session_id: String::new(),
                        branch: String::new(),
                        parent_commit_id: String::new(),
                    }),
                "run".to_string(), // request_id from AssemblyContext (D-16)
                turn_counter,
            );

            match self
                .agent
                .dispatch_prompt_lifecycle(PromptHookContext::new(
                    PromptHookPoint::TurnStart,
                    "run",
                    &self.user_input,
                ))
                .await
            {
                Ok(outcome) => {
                    collector.push_hook_records(&outcome.executions);
                }
                Err(event) => {
                    self.emit_and_record(event);
                    break;
                }
            }

            let rendered = match self.compile_turn_prompt(&mut collector).await {
                Ok(rendered) => rendered,
                Err(event) => {
                    self.emit_and_record(event);
                    break;
                }
            };
            for msg in &rendered.messages {
                match msg.role {
                    Role::System => self.agent.state.push_system_message(&msg.content),
                    Role::User => self
                        .agent
                        .state
                        .upsert_user_message_from(self.user_message_start, &msg.content),
                    _ => {}
                }
            }

            let context_len = if self.agent.state.resolved.context_length > 0 {
                self.agent.state.resolved.context_length
            } else {
                131072
            };
            let _ = self.agent.state.maybe_auto_compact();
            let _ = self.agent.state.manage_context(context_len, 15);

            let events = self.run_chat_with_retries().await;

            let tool_calls = collect_tool_calls(&events);

            if tool_calls.is_empty() {
                // Capture task/recovery state into manifest metadata.
                capture_non_empty_task_metadata(self.agent, &mut collector);
                // Build and emit manifest before Done (D-10, D-11)
                emit_turn_manifest(
                    collector,
                    &rendered,
                    &self.agent.state.resolved.canonical_id,
                    &mut self.all_events,
                    self.emit,
                );
                self.all_events.extend(events);
                break;
            }
            if self.agent.tool_executor.is_none() {
                self.all_events.extend(events);
                break;
            }
            self.all_events.extend(
                events
                    .into_iter()
                    .filter(|event| !matches!(event, LoopEvent::Done { .. })),
            );
            let stop_after_tool_hook_error =
                self.execute_tool_calls(&tool_calls, &mut collector).await;
            // Capture task/recovery state into manifest metadata
            // after all tool results have been processed.
            capture_non_empty_task_metadata(self.agent, &mut collector);
            // Emit manifest for this turn (including intermediate tool-calling turns)
            emit_turn_manifest(
                collector,
                &rendered,
                &self.agent.state.resolved.canonical_id,
                &mut self.all_events,
                self.emit,
            );
            if stop_after_tool_hook_error {
                break;
            }
        }

        // Ensure Done is always emitted (CR-01: max_turns exhaustion)
        if !self
            .all_events
            .iter()
            .any(|e| matches!(e, LoopEvent::Done { .. }))
        {
            self.emit_and_record(LoopEvent::Done { usage: None });
        }

        self.all_events
    }

    async fn compile_turn_prompt(
        &self,
        collector: &mut ManifestCollector,
    ) -> Result<prompt::RenderedPrompt, LoopEvent> {
        // Compile prompt fresh each iteration: events and memory accumulated
        // from previous tool turns are re-evaluated by providers, and budget
        // pressure from growing tool results naturally compresses lower-priority
        // sections.
        let ctx = prompt::AssemblyContext {
            request_id: "run",
            model: &self.agent.state.resolved,
            user_input: &self.user_input,
        };
        let mut contributions = self.agent.registry.collect_contributions(&ctx).await;
        let outcome = self
            .agent
            .dispatch_prompt_lifecycle(
                PromptHookContext::new(
                    PromptHookPoint::PostContextCollect,
                    "run",
                    &self.user_input,
                )
                .with_contributions(contributions),
            )
            .await?;
        collector.push_hook_records(&outcome.executions);
        contributions = outcome.contributions;

        let outcome = self
            .agent
            .dispatch_prompt_lifecycle(
                PromptHookContext::new(PromptHookPoint::PrePromptCompile, "run", &self.user_input)
                    .with_contributions(contributions),
            )
            .await?;
        collector.push_hook_records(&outcome.executions);
        contributions = outcome.contributions;

        let rendered = prompt::compiler::compile_contributions(
            &contributions,
            &self.user_input,
            &self.agent.state.resolved,
        )
        .map_err(|err| LoopEvent::Error {
            message: err.to_string(),
            retryable: false,
        })?;

        let outcome = self
            .agent
            .dispatch_prompt_lifecycle(
                PromptHookContext::new(PromptHookPoint::PostPromptCompile, "run", &self.user_input)
                    .with_rendered(rendered.clone()),
            )
            .await?;
        collector.push_hook_records(&outcome.executions);

        Ok(rendered)
    }

    async fn execute_tool_calls(
        &mut self,
        tool_calls: &[ToolCall],
        collector: &mut ManifestCollector,
    ) -> bool {
        if self.agent.tool_executor.is_none() {
            return false;
        }

        for call in tool_calls {
            match self
                .agent
                .dispatch_prompt_lifecycle(
                    PromptHookContext::new(PromptHookPoint::PreToolCall, "run", &self.user_input)
                        .with_tool(call.clone(), None),
                )
                .await
            {
                Ok(outcome) => {
                    collector.push_hook_records(&outcome.executions);
                }
                Err(event) => {
                    self.emit_and_record(event);
                    return true;
                }
            }

            let outcome = {
                let Some(executor) = self.agent.tool_executor.as_ref() else {
                    return false;
                };
                executor.execute_outcome(call).await
            };
            let result = Agent::render_tool_outcome(&outcome);

            match self
                .agent
                .dispatch_prompt_lifecycle(
                    PromptHookContext::new(
                        PromptHookPoint::PostToolResult,
                        "run",
                        &self.user_input,
                    )
                    .with_tool(call.clone(), Some(result.clone())),
                )
                .await
            {
                Ok(outcome) => {
                    collector.push_hook_records(&outcome.executions);
                }
                Err(event) => {
                    self.emit_and_record(event);
                    return true;
                }
            }

            let event = LoopEvent::ToolResult {
                call: call.clone(),
                result: result.clone(),
            };
            collector.push_tool_record(call, &result); // D-07, D-08, D-09
            self.emit_and_record(event);
            self.agent.state.push_tool_result(&call.id, &result, None);

            // Record todo_write updates into live task state (Plan 13-02 Task 1).
            // A successful todo_write result starts with "Todo list updated:"; errors
            // and denials produce different prefixes, so we only update on success.
            self.agent.record_todo_result(call, &result);
            self.agent.classify_tool_outcome(call, &outcome, &result);
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{TodoItem, TodoStatus};
    use lattice_core::catalog::ApiProtocol;
    use lattice_core::types::{Message, Role};
    use lattice_core::{CredentialStatus, ResolvedModel};
    use std::collections::HashMap;

    fn make_resolved() -> ResolvedModel {
        ResolvedModel {
            canonical_id: "test".into(),
            api_model_id: "test".into(),
            provider: "test".into(),
            base_url: "http://localhost".to_string(),
            api_key: Some("sk-test".into()),
            api_protocol: ApiProtocol::OpenAiChat,
            context_length: 4096,
            credential_status: CredentialStatus::Present,
            provider_specific: HashMap::new(),
        }
    }

    fn make_collector() -> ManifestCollector {
        ManifestCollector::new(
            SessionContext {
                session_id: "session".into(),
                branch: "main".into(),
                parent_commit_id: "parent".into(),
            },
            "request".into(),
            0,
        )
    }

    fn rendered_prompt() -> prompt::RenderedPrompt {
        prompt::RenderedPrompt {
            messages: vec![Message::new(Role::User, "hello".into(), None, None, None)],
            sections: Vec::new(),
            contributions: Vec::new(),
            fragments: Vec::new(),
            context_manifest: Vec::new(),
            total_tokens: 1,
        }
    }

    #[test]
    fn capture_non_empty_task_metadata_skips_empty_state() {
        let agent = Agent::new(make_resolved());
        let mut collector = make_collector();

        capture_non_empty_task_metadata(&agent, &mut collector);

        let manifest = collector.build_manifest(&rendered_prompt(), "test-model");
        assert!(manifest.metadata.is_none());
    }

    #[test]
    fn capture_non_empty_task_metadata_records_populated_state() {
        let agent = Agent::new(make_resolved());
        agent
            .state
            .task_state_mut()
            .replace_todos(vec![TodoItem {
                id: 0,
                content: "Resume current plan".into(),
                status: TodoStatus::InProgress,
            }])
            .unwrap();
        let mut collector = make_collector();

        capture_non_empty_task_metadata(&agent, &mut collector);

        let manifest = collector.build_manifest(&rendered_prompt(), "test-model");
        let metadata = manifest.metadata.expect("non-empty task state metadata");
        let task_state: crate::state::TaskState = serde_json::from_value(metadata).unwrap();
        assert_eq!(task_state.todos.len(), 1);
        assert_eq!(task_state.todos[0].content, "Resume current plan");
    }
}
