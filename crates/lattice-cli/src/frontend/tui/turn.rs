use std::collections::VecDeque;
use std::time::{Duration, Instant};

use lattice_core::types::Role;

use super::state::AppStatus;
use super::state::ChatMessage;

/// Tracks a single active agent turn plus prompts queued behind it.
#[derive(Debug, Clone)]
pub(super) struct TurnState {
    pub(super) active_turn_id: Option<u64>,
    pub(super) next_turn_id: u64,
    pub(super) active_assistant_index: Option<usize>,
    pub(super) pending_user: Option<String>,
    pub(super) queued_inputs: VecDeque<String>,
    pub(super) stream_started: Option<Instant>,
    pub(super) reasoning_started: Option<Instant>,
    pub(super) reasoning_duration: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum StreamOutcome {
    Ignored,
    Updated,
    Finished { pending_user: Option<String> },
    Errored,
}

#[derive(Debug, Clone)]
pub(super) struct StreamTokenUpdate {
    pub(super) turn_id: u64,
    pub(super) content: String,
    pub(super) reasoning: Option<String>,
    pub(super) done: bool,
    pub(super) error: Option<String>,
}

impl Default for TurnState {
    fn default() -> Self {
        Self {
            active_turn_id: None,
            next_turn_id: 1,
            active_assistant_index: None,
            pending_user: None,
            queued_inputs: VecDeque::new(),
            stream_started: None,
            reasoning_started: None,
            reasoning_duration: None,
        }
    }
}

impl TurnState {
    pub(super) fn begin(&mut self, user: String, assistant_index: usize) -> u64 {
        let turn_id = self.next_turn_id;
        self.next_turn_id = self.next_turn_id.wrapping_add(1).max(1);
        self.active_turn_id = Some(turn_id);
        self.active_assistant_index = Some(assistant_index);
        self.pending_user = Some(user);
        turn_id
    }

    pub(super) fn cancel(&mut self, status: &mut AppStatus) {
        self.active_turn_id = None;
        self.active_assistant_index = None;
        self.pending_user = None;
        if *status == AppStatus::Streaming {
            *status = AppStatus::Ready;
        }
        self.stream_started = None;
        self.reasoning_started = None;
        self.reasoning_duration = None;
    }

    pub(super) fn accepts(&self, turn_id: u64) -> bool {
        self.active_turn_id == Some(turn_id)
    }

    pub(super) fn is_active(&self, status: &AppStatus) -> bool {
        self.active_turn_id.is_some() || *status == AppStatus::Streaming
    }

    pub(super) fn reset_runtime(&mut self) {
        self.active_turn_id = None;
        self.active_assistant_index = None;
        self.pending_user = None;
        self.stream_started = None;
        self.reasoning_started = None;
        self.reasoning_duration = None;
        self.queued_inputs.clear();
    }

    pub(super) fn finish(&mut self) {
        self.active_turn_id = None;
        self.active_assistant_index = None;
        self.pending_user = None;
        self.stream_started = None;
        self.reasoning_started = None;
        self.reasoning_duration = None;
    }

    pub(super) fn queue_input(&mut self, text: String) -> usize {
        self.queued_inputs.push_back(text);
        self.queued_inputs.len()
    }

    /// Apply a streaming delta to the active assistant message.
    ///
    /// Successful completion returns the pending user prompt before the turn
    /// runtime is cleared so the coordinator can persist the session.
    pub(super) fn apply_stream_token(
        &mut self,
        messages: &mut Vec<ChatMessage>,
        status: &mut AppStatus,
        token_count: &mut usize,
        update: StreamTokenUpdate,
    ) -> StreamOutcome {
        if !self.accepts(update.turn_id) {
            return StreamOutcome::Ignored;
        }

        let has_payload =
            !update.content.is_empty() || update.reasoning.is_some() || update.error.is_some();
        if update.done && !has_payload {
            *status = AppStatus::Ready;
            let pending_user = self.pending_user.clone();
            self.finish();
            return StreamOutcome::Finished { pending_user };
        }

        let assistant_index = match self.active_assistant_index {
            Some(idx) if idx < messages.len() => idx,
            _ => {
                if has_payload {
                    messages.push(ChatMessage::new(Role::Assistant, String::new()));
                    let idx = messages.len().saturating_sub(1);
                    self.active_assistant_index = Some(idx);
                    idx
                } else {
                    return StreamOutcome::Ignored;
                }
            }
        };

        if messages
            .get(assistant_index)
            .is_none_or(|msg| msg.role != Role::Assistant)
        {
            return StreamOutcome::Ignored;
        }

        if !update.content.is_empty() {
            if let Some(last) = messages.get_mut(assistant_index) {
                last.content.push_str(&update.content);
            }
            *token_count +=
                lattice_core::tokens::TokenEstimator::estimate_text(&update.content) as usize;
        }
        if let Some(r) = update.reasoning {
            *token_count += lattice_core::tokens::TokenEstimator::estimate_text(&r) as usize;
            if self.reasoning_started.is_none() {
                self.reasoning_started = Some(std::time::Instant::now());
            }
            if let Some(last) = messages.get_mut(assistant_index) {
                match last.reasoning {
                    Some(ref mut existing) => existing.push_str(&r),
                    None => last.reasoning = Some(r),
                }
            }
        } else if self.reasoning_started.is_some() && self.reasoning_duration.is_none() {
            if let Some(start) = self.reasoning_started {
                self.reasoning_duration = Some(start.elapsed());
            }
        }
        if let Some(ref msg) = update.error {
            let error_text = format!("Error: {}", msg);
            if let Some(last) = messages.get_mut(assistant_index) {
                if last.content.is_empty() {
                    last.content = error_text.clone();
                } else if !last.content.contains(&error_text) {
                    if !last.content.ends_with('\n') && !last.content.is_empty() {
                        last.content.push('\n');
                    }
                    last.content.push_str(&error_text);
                }
            }
        }

        if let Some(msg) = update.error {
            *status = AppStatus::Error(msg);
            self.finish();
            StreamOutcome::Errored
        } else if update.done {
            *status = AppStatus::Ready;
            let pending_user = self.pending_user.clone();
            self.finish();
            StreamOutcome::Finished { pending_user }
        } else {
            StreamOutcome::Updated
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_token_appends_and_finishes_with_pending_user() {
        let mut turn = TurnState {
            active_turn_id: Some(1),
            active_assistant_index: Some(0),
            pending_user: Some("hello".into()),
            ..TurnState::default()
        };
        let mut messages = vec![ChatMessage::new(Role::Assistant, String::new())];
        let mut status = AppStatus::Streaming;
        let mut token_count = 0;

        let first = turn.apply_stream_token(
            &mut messages,
            &mut status,
            &mut token_count,
            StreamTokenUpdate {
                turn_id: 1,
                content: "answer".into(),
                reasoning: None,
                done: false,
                error: None,
            },
        );

        assert_eq!(first, StreamOutcome::Updated);
        assert_eq!(messages[0].content, "answer");
        assert!(token_count > 0);

        let done = turn.apply_stream_token(
            &mut messages,
            &mut status,
            &mut token_count,
            StreamTokenUpdate {
                turn_id: 1,
                content: String::new(),
                reasoning: None,
                done: true,
                error: None,
            },
        );

        assert_eq!(
            done,
            StreamOutcome::Finished {
                pending_user: Some("hello".into())
            }
        );
        assert_eq!(status, AppStatus::Ready);
        assert!(turn.active_turn_id.is_none());
    }

    #[test]
    fn stale_stream_token_is_ignored() {
        let mut turn = TurnState {
            active_turn_id: Some(2),
            active_assistant_index: Some(0),
            ..TurnState::default()
        };
        let mut messages = vec![ChatMessage::new(Role::Assistant, String::new())];
        let mut status = AppStatus::Streaming;
        let mut token_count = 0;

        let outcome = turn.apply_stream_token(
            &mut messages,
            &mut status,
            &mut token_count,
            StreamTokenUpdate {
                turn_id: 1,
                content: "ignored".into(),
                reasoning: None,
                done: false,
                error: None,
            },
        );

        assert_eq!(outcome, StreamOutcome::Ignored);
        assert!(messages[0].content.is_empty());
        assert_eq!(token_count, 0);
    }

    #[test]
    fn done_event_finishes_after_tool_only_segment() {
        let mut turn = TurnState {
            active_turn_id: Some(1),
            active_assistant_index: None,
            pending_user: Some("hello".into()),
            ..TurnState::default()
        };
        let mut messages = Vec::new();
        let mut status = AppStatus::Streaming;
        let mut token_count = 0;

        let outcome = turn.apply_stream_token(
            &mut messages,
            &mut status,
            &mut token_count,
            StreamTokenUpdate {
                turn_id: 1,
                content: String::new(),
                reasoning: None,
                done: true,
                error: None,
            },
        );

        assert_eq!(
            outcome,
            StreamOutcome::Finished {
                pending_user: Some("hello".into())
            }
        );
        assert_eq!(status, AppStatus::Ready);
        assert!(turn.active_turn_id.is_none());
    }
}
