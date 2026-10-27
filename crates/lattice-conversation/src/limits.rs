// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Protocol stream-assembly hard limits (ADR 0005 §10), counted as UTF-8
//! bytes.
//!
//! Exceeding a limit stops consumption and records a typed
//! [`AttemptInterruption::OutputLimitExceeded`]; no partial conversation or
//! `InvocationId` is produced from an interrupted attempt.

use crate::attempt::AttemptInterruption;
use std::collections::BTreeMap;

/// Maximum number of ToolCalls in one complete provider batch.
///
/// This code-defined bound keeps one model step's durable binding/closure
/// journal finite independently of argument byte sizes.
pub const MAX_TOOL_CALLS_PER_BATCH: usize = 32;

/// Maximum byte length of one ToolCall's textual arguments (1 MiB).
///
/// Aligned with the LANCET single-file workspace write limit
/// (`MAX_WORKSPACE_WRITE_BYTES`, 1 MiB): write content travels inside the
/// arguments, so a larger call could never execute successfully.
pub const MAX_TOOL_CALL_ARGUMENTS_BYTES: usize = 1024 * 1024;

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
    /// One ToolCall's arguments (1 MiB).
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
#[derive(Debug, Clone, Default)]
pub struct AttemptAssembly {
    assistant_bytes: usize,
    reasoning_bytes: usize,
    total_bytes: usize,
    tool_arguments: BTreeMap<usize, usize>,
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

    /// 按线上调用 index 计量；交错片段共享尝试总量，并分别保留单调用预算。
    pub fn push_tool_arguments(
        &mut self,
        index: usize,
        delta: &str,
    ) -> Result<(), AttemptInterruption> {
        let next = self
            .tool_arguments
            .get(&index)
            .copied()
            .unwrap_or(0)
            .saturating_add(delta.len());
        self.check(LimitKind::ToolCallArguments, next)?;
        self.check_total(delta.len())?;
        self.tool_arguments.insert(index, next);
        self.total_bytes += delta.len();
        Ok(())
    }

    /// 参数只解析一次；规范形式变长时补计正差额，保留已经消费的原文字节。
    pub fn finish_tool_arguments(
        &mut self,
        index: usize,
        canonical_bytes: usize,
    ) -> Result<(), AttemptInterruption> {
        let raw = self.tool_arguments.get(&index).copied().unwrap_or(0);
        let next = raw.max(canonical_bytes);
        self.check(LimitKind::ToolCallArguments, next)?;
        self.check_total(next - raw)?;
        self.tool_arguments.insert(index, next);
        self.total_bytes += next - raw;
        Ok(())
    }

    /// 替换已计入的元数据长度；相同快照幂等，缩短时释放其净占用。
    pub fn replace_metadata(
        &mut self,
        previous: usize,
        next: usize,
    ) -> Result<(), AttemptInterruption> {
        let total = self
            .total_bytes
            .checked_sub(previous)
            .expect("previous metadata was accounted")
            .saturating_add(next);
        self.check(LimitKind::ModelAttemptTotal, total)?;
        self.total_bytes = total;
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

/// 持久边界独立核对完整消息；参数使用规范形式，复用流组装的常量与计量算法。
pub fn validate_assistant_bounds<S: crate::ToolCallState>(
    message: &crate::NativeMessage<S>,
) -> Result<(), AttemptInterruption> {
    let mut assembly = AttemptAssembly::new();
    for (index, part) in message.parts.iter().enumerate() {
        match &part.semantic {
            crate::MessageSemantic::Text { text } => assembly.push_assistant(text)?,
            crate::MessageSemantic::Reasoning { text } => assembly.push_reasoning(text)?,
            crate::MessageSemantic::ToolCall { call } => {
                assembly.push_tool_arguments(index, &crate::canonical_json(call.arguments()))?;
                assembly.replace_metadata(
                    0,
                    call.name()
                        .len()
                        .saturating_add(call.provider_call_id().map_or(0, str::len)),
                )?;
            }
        }
        if let Some(replay) = &part.replay_state {
            assembly.replace_metadata(0, replay.as_str().len())?;
        }
    }
    Ok(())
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
        assembly
            .push_tool_arguments(0, &chunk)
            .expect("first call chunk");
        // A second call gets its own per-call budget.
        assembly
            .push_tool_arguments(1, &chunk)
            .expect("second call chunk");

        // Per-call plus-one fails even with total headroom remaining.
        let mut per_call = AttemptAssembly::new();
        let exact = "t".repeat(MAX_TOOL_CALL_ARGUMENTS_BYTES);
        per_call.push_tool_arguments(9, &exact).expect("fill");
        let (kind, _, _) = exceeded(per_call.push_tool_arguments(9, "b"));
        assert_eq!(kind, LimitKind::ToolCallArguments);
    }

    #[test]
    fn interleaved_arguments_keep_each_index_budget() {
        let mut assembly = AttemptAssembly::new();
        assembly
            .push_tool_arguments(9, &"x".repeat(MAX_TOOL_CALL_ARGUMENTS_BYTES))
            .unwrap();
        assembly.push_tool_arguments(2, "{}").unwrap();
        assert_eq!(
            exceeded(assembly.push_tool_arguments(9, "!")),
            (
                LimitKind::ToolCallArguments,
                MAX_TOOL_CALL_ARGUMENTS_BYTES,
                MAX_TOOL_CALL_ARGUMENTS_BYTES + 1
            ),
        );
        assembly.push_tool_arguments(usize::MAX, "{}").unwrap();
        assert_eq!(assembly.total_bytes(), MAX_TOOL_CALL_ARGUMENTS_BYTES + 4);
    }

    #[test]
    fn attempt_total_limit_aggregates_all_payloads() {
        let mut assembly = AttemptAssembly::new();
        // 2 MiB assistant + 2 MiB reasoning + 1 MiB arguments = 5 MiB.
        assembly
            .push_assistant(&"a".repeat(MAX_ASSISTANT_CONTENT_BYTES))
            .expect("assistant");
        assembly
            .push_reasoning(&"r".repeat(MAX_REASONING_CONTENT_BYTES))
            .expect("reasoning");
        assembly
            .push_tool_arguments(0, &"t".repeat(MAX_TOOL_CALL_ARGUMENTS_BYTES))
            .expect("arguments");
        // 用按 index 独立计量的调用参数填满剩余总量；
        // 各调用都在单调用限额内，确保只触发总量限额。
        let mut headroom = MAX_MODEL_ATTEMPT_BYTES - assembly.total_bytes();
        let mut index = 1;
        while headroom > 0 {
            let chunk = headroom.min(MAX_TOOL_CALL_ARGUMENTS_BYTES);
            assembly
                .push_tool_arguments(index, &"u".repeat(chunk))
                .expect("fill remaining headroom");
            headroom -= chunk;
            index += 1;
        }
        assert_eq!(assembly.total_bytes(), MAX_MODEL_ATTEMPT_BYTES);
        let (kind, limit, _) = exceeded(assembly.push_tool_arguments(index, "!"));
        assert_eq!(kind, LimitKind::ModelAttemptTotal);
        assert_eq!(limit, MAX_MODEL_ATTEMPT_BYTES);
    }
}
