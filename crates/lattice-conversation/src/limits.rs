// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Protocol stream-assembly hard limits (ADR 0005 §10), counted as UTF-8
//! bytes.
//!
//! Exceeding a limit stops consumption and records a typed
//! [`AttemptInterruption::OutputLimitExceeded`]; no partial conversation or
//! `InvocationId` is produced from an interrupted attempt.

use crate::attempt::AttemptInterruption;

/// Maximum byte length of one ToolCall's textual arguments (8 MiB).
pub const MAX_TOOL_CALL_ARGUMENTS_BYTES: usize = 8 * 1024 * 1024;

/// Maximum byte length of the assistant content of one attempt (2 MiB).
pub const MAX_ASSISTANT_CONTENT_BYTES: usize = 2 * 1024 * 1024;

/// Maximum byte length of the reasoning content of one attempt (2 MiB).
pub const MAX_REASONING_CONTENT_BYTES: usize = 2 * 1024 * 1024;

/// Maximum total byte length assembled by one model attempt (16 MiB).
pub const MAX_MODEL_ATTEMPT_BYTES: usize = 16 * 1024 * 1024;

/// Which protocol limit was exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitKind {
    /// One ToolCall's arguments (8 MiB).
    ToolCallArguments,
    /// Assistant content of one attempt (2 MiB).
    AssistantContent,
    /// Reasoning content of one attempt (2 MiB).
    ReasoningContent,
    /// Total assembled output of one attempt (16 MiB).
    ModelAttemptTotal,
}

impl LimitKind {
    /// The byte limit for this kind.
    pub fn limit_bytes(&self) -> usize {
        match self {
            Self::ToolCallArguments => MAX_TOOL_CALL_ARGUMENTS_BYTES,
            Self::AssistantContent => MAX_ASSISTANT_CONTENT_BYTES,
            Self::ReasoningContent => MAX_REASONING_CONTENT_BYTES,
            Self::ModelAttemptTotal => MAX_MODEL_ATTEMPT_BYTES,
        }
    }
}

/// Streaming byte accountant for one model attempt.
///
/// Feed provider stream deltas through the `push_*` methods. The first push
/// that would cross a per-kind or total limit returns a typed
/// [`AttemptInterruption::OutputLimitExceeded`] and leaves the accountant
/// unchanged; the caller stops consumption and records the interruption. A
/// value that lands exactly on a limit is accepted.
#[derive(Debug, Clone, Copy, Default)]
pub struct AttemptAssembly {
    assistant_bytes: usize,
    reasoning_bytes: usize,
    current_tool_arguments_bytes: usize,
    total_bytes: usize,
}

impl AttemptAssembly {
    /// A fresh accountant for one attempt.
    pub fn new() -> Self {
        Self::default()
    }

    /// Accumulated assistant content bytes so far.
    pub fn assistant_bytes(&self) -> usize {
        self.assistant_bytes
    }

    /// Accumulated reasoning bytes so far.
    pub fn reasoning_bytes(&self) -> usize {
        self.reasoning_bytes
    }

    /// Accumulated bytes across every payload of this attempt so far.
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    /// Bytes accumulated for the ToolCall arguments currently streaming.
    pub fn current_tool_arguments_bytes(&self) -> usize {
        self.current_tool_arguments_bytes
    }

    /// Start a new ToolCall's arguments stream (per-call limit resets).
    pub fn begin_tool_call(&mut self) {
        self.current_tool_arguments_bytes = 0;
    }

    /// Account one assistant content delta.
    pub fn push_assistant(&mut self, delta: &str) -> Result<(), AttemptInterruption> {
        let next = self.assistant_bytes.saturating_add(delta.len());
        self.check(LimitKind::AssistantContent, next)?;
        self.check_total(delta.len())?;
        self.assistant_bytes = next;
        self.total_bytes += delta.len();
        Ok(())
    }

    /// Account one reasoning content delta.
    pub fn push_reasoning(&mut self, delta: &str) -> Result<(), AttemptInterruption> {
        let next = self.reasoning_bytes.saturating_add(delta.len());
        self.check(LimitKind::ReasoningContent, next)?;
        self.check_total(delta.len())?;
        self.reasoning_bytes = next;
        self.total_bytes += delta.len();
        Ok(())
    }

    /// Account one ToolCall arguments delta for the current call.
    pub fn push_tool_arguments(&mut self, delta: &str) -> Result<(), AttemptInterruption> {
        let next = self
            .current_tool_arguments_bytes
            .saturating_add(delta.len());
        self.check(LimitKind::ToolCallArguments, next)?;
        self.check_total(delta.len())?;
        self.current_tool_arguments_bytes = next;
        self.total_bytes += delta.len();
        Ok(())
    }

    fn check(&self, kind: LimitKind, next: usize) -> Result<(), AttemptInterruption> {
        let limit = kind.limit_bytes();
        if next > limit {
            return Err(AttemptInterruption::OutputLimitExceeded {
                kind,
                limit,
                actual: next,
            });
        }
        Ok(())
    }

    fn check_total(&self, added: usize) -> Result<(), AttemptInterruption> {
        let limit = LimitKind::ModelAttemptTotal.limit_bytes();
        let next = self.total_bytes.saturating_add(added);
        if next > limit {
            return Err(AttemptInterruption::OutputLimitExceeded {
                kind: LimitKind::ModelAttemptTotal,
                limit,
                actual: next,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exceeded(result: Result<(), AttemptInterruption>) -> (LimitKind, usize, usize) {
        match result {
            Err(AttemptInterruption::OutputLimitExceeded {
                kind,
                limit,
                actual,
            }) => (kind, limit, actual),
            other => panic!("expected OutputLimitExceeded, got {other:?}"),
        }
    }

    #[test]
    fn assistant_content_exact_boundary_passes_plus_one_fails() {
        let mut assembly = AttemptAssembly::new();
        let exact = "x".repeat(MAX_ASSISTANT_CONTENT_BYTES);
        assembly.push_assistant(&exact).expect("exact boundary");
        assert_eq!(assembly.assistant_bytes(), MAX_ASSISTANT_CONTENT_BYTES);

        let mut over = AttemptAssembly::new();
        over.push_assistant(&exact).expect("fill to boundary");
        let (kind, limit, actual) = exceeded(over.push_assistant("y"));
        assert_eq!(kind, LimitKind::AssistantContent);
        assert_eq!(limit, MAX_ASSISTANT_CONTENT_BYTES);
        assert_eq!(actual, MAX_ASSISTANT_CONTENT_BYTES + 1);
        // The failed push left the accountant unchanged.
        assert_eq!(over.assistant_bytes(), MAX_ASSISTANT_CONTENT_BYTES);
    }

    #[test]
    fn reasoning_content_limit_is_independent() {
        let mut assembly = AttemptAssembly::new();
        let exact = "r".repeat(MAX_REASONING_CONTENT_BYTES);
        assembly.push_reasoning(&exact).expect("exact boundary");
        let (kind, _, _) = exceeded(assembly.push_reasoning("!"));
        assert_eq!(kind, LimitKind::ReasoningContent);
    }

    #[test]
    fn tool_arguments_limit_is_per_call() {
        let chunk = "a".repeat(MAX_TOOL_CALL_ARGUMENTS_BYTES / 2);
        let mut assembly = AttemptAssembly::new();
        assembly.begin_tool_call();
        assembly
            .push_tool_arguments(&chunk)
            .expect("first call chunk");
        // A second call gets its own per-call budget.
        assembly.begin_tool_call();
        assembly
            .push_tool_arguments(&chunk)
            .expect("second call chunk");

        // Per-call plus-one fails even with total headroom remaining.
        let mut per_call = AttemptAssembly::new();
        per_call.begin_tool_call();
        let exact = "t".repeat(MAX_TOOL_CALL_ARGUMENTS_BYTES);
        per_call.push_tool_arguments(&exact).expect("fill");
        let (kind, _, _) = exceeded(per_call.push_tool_arguments("b"));
        assert_eq!(kind, LimitKind::ToolCallArguments);
    }

    #[test]
    fn attempt_total_limit_aggregates_all_payloads() {
        let mut assembly = AttemptAssembly::new();
        // 2 MiB assistant + 2 MiB reasoning + 8 MiB arguments = 12 MiB.
        assembly
            .push_assistant(&"a".repeat(MAX_ASSISTANT_CONTENT_BYTES))
            .expect("assistant");
        assembly
            .push_reasoning(&"r".repeat(MAX_REASONING_CONTENT_BYTES))
            .expect("reasoning");
        assembly.begin_tool_call();
        assembly
            .push_tool_arguments(&"t".repeat(MAX_TOOL_CALL_ARGUMENTS_BYTES))
            .expect("arguments");
        // 4 MiB of headroom remains; fill it with a second ToolCall's
        // arguments (per-call limit resets, so only the total can trip).
        let headroom = MAX_MODEL_ATTEMPT_BYTES - assembly.total_bytes();
        assembly.begin_tool_call();
        assembly
            .push_tool_arguments(&"u".repeat(headroom))
            .expect("fill remaining headroom");
        assert_eq!(assembly.total_bytes(), MAX_MODEL_ATTEMPT_BYTES);
        let (kind, limit, _) = exceeded({
            assembly.begin_tool_call();
            assembly.push_tool_arguments("!")
        });
        assert_eq!(kind, LimitKind::ModelAttemptTotal);
        assert_eq!(limit, MAX_MODEL_ATTEMPT_BYTES);
    }
}
