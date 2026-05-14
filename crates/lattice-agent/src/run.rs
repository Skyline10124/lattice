use lattice_hooks::PromptHookContext;

use super::turn_engine::TurnEngine;
use super::{Agent, LoopEvent};

impl Agent {
    pub async fn send_message(&mut self, content: &str) -> Vec<LoopEvent> {
        self.state.push_user_message(content);
        self.run_chat().await
    }

    pub async fn submit_tools(
        &mut self,
        results: Vec<(String, String)>,
        max_size: Option<usize>,
    ) -> Vec<LoopEvent> {
        for (call_id, result) in &results {
            self.state.push_tool_result(call_id, result, max_size);
        }
        self.run_chat().await
    }

    pub async fn run(&mut self, content: &str, max_turns: u32) -> Vec<LoopEvent> {
        let mut ignore_event = |_| {};
        self.run_with_observer(content, max_turns, &mut ignore_event, true)
            .await
    }

    pub async fn run_streaming<F>(
        &mut self,
        content: &str,
        max_turns: u32,
        mut emit: F,
    ) -> Vec<LoopEvent>
    where
        F: FnMut(LoopEvent),
    {
        self.run_with_observer(content, max_turns, &mut emit, false)
            .await
    }

    async fn run_with_observer<F>(
        &mut self,
        content: &str,
        max_turns: u32,
        emit: &mut F,
        retry_stream_errors: bool,
    ) -> Vec<LoopEvent>
    where
        F: FnMut(LoopEvent),
    {
        TurnEngine::new(self, content, emit, retry_stream_errors)
            .run(max_turns)
            .await
    }

    pub(super) async fn dispatch_prompt_lifecycle(
        &self,
        context: PromptHookContext,
    ) -> Result<lattice_hooks::PromptHookOutcome, LoopEvent> {
        self.prompt_hooks
            .dispatch(context)
            .await
            .map_err(|err| LoopEvent::Error {
                message: err.to_string(),
                retryable: false,
            })
    }
}
