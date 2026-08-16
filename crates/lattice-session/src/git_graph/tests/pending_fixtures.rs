// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Shared builders for PendingTurn journal tests (GitHub #144).
//! Deterministic identities only; no production values.

use lancet_protocol::{
    InvocationId, ModelAttemptKey, ModelMessageAddress, PlanKey, SessionId, ToolCallAddress, TurnId,
};
use lattice_conversation::{
    MessagePart, MessageRole, MessageSemantic, NativeMessage, ProviderOrigin, UnboundToolCall,
    arguments_digest,
};

use crate::graph::GraphMessage;
use crate::pending::config::TurnStartedFacts;
use crate::pending::outcome::{ClosureOrigin, TurnFinishReason};
use crate::pending::{TurnCommitCandidate, TurnClosingFacts, TurnOutcome};
use crate::pending::{FrozenTurnConfig, PlannedBinding, ToolCatalogEntry, ToolCatalogSnapshot, TurnExecutionLimits, TurnUsageAggregate};
use crate::test_helpers::user_msg;

pub(crate) const SESSION: &str = "3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c";
pub(crate) const TURN: &str = "3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c";
pub(crate) const CREATED_AT: &str = "2026-08-16T12:00:00Z";

pub(crate) fn turn_id() -> TurnId {
    TurnId::parse(TURN).expect("turn id")
}

pub(crate) fn session_id() -> SessionId {
    SessionId::parse(SESSION).expect("session id")
}

pub(crate) fn invocation_id(n: u32) -> InvocationId {
    InvocationId::parse(&format!("00000000-0000-4000-8000-{n:012x}")).expect("invocation id")
}

pub(crate) fn attempt_key(step: u32, attempt: u32) -> ModelAttemptKey {
    ModelAttemptKey {
        turn_id: turn_id(),
        model_step_ordinal: step,
        attempt_ordinal: attempt,
    }
}

pub(crate) fn frozen_config() -> FrozenTurnConfig {
    FrozenTurnConfig {
        agent_name: "coder".to_string(),
        model: "test-model".to_string(),
        provider: "test-provider".to_string(),
        system_prompt: None,
        profile_snapshot_json: "{\"model\":\"test-model\"}".to_string(),
        policy_snapshot_json: "{}".to_string(),
        catalog: ToolCatalogSnapshot::new(
            "test-build".to_string(),
            vec![ToolCatalogEntry {
                name: "observe.workspace".to_string(),
                descriptor_identity: "d".repeat(64),
                description: "observe".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            }],
        )
        .expect("catalog"),
        limits: TurnExecutionLimits {
            timeout_secs: 60,
            max_model_steps: crate::pending::MAX_MODEL_STEPS,
            max_model_attempts: 2,
        },
    }
}

pub(crate) fn user_message(text: &str) -> NativeMessage<UnboundToolCall> {
    NativeMessage::new(
        MessageRole::User,
        None,
        vec![MessagePart::semantic_only(MessageSemantic::Text {
            text: text.to_string(),
        })],
    )
}

pub(crate) fn assistant_text(text: &str) -> NativeMessage<UnboundToolCall> {
    NativeMessage::new(
        MessageRole::Assistant,
        Some(ProviderOrigin::Anthropic),
        vec![MessagePart::semantic_only(MessageSemantic::Text {
            text: text.to_string(),
        })],
    )
}

pub(crate) fn tool_call_part(name: &str, arguments: serde_json::Value) -> MessagePart<UnboundToolCall> {
    MessagePart::semantic_only(MessageSemantic::ToolCall {
        call: UnboundToolCall {
            provider_call_id: Some(format!("call-{name}")),
            name: name.to_string(),
            arguments,
        },
    })
}

pub(crate) fn turn_started_facts(parent_tip: Option<String>) -> TurnStartedFacts {
    TurnStartedFacts {
        turn_id: turn_id(),
        session_id: session_id(),
        parent_conversation_tip: parent_tip,
        user_message: user_message("please continue"),
        config: frozen_config(),
        accepted_at: CREATED_AT.to_string(),
    }
}

/// A one-binding plan for `observe.workspace` at part ordinal 0.
pub(crate) fn one_binding_plan() -> crate::pending::BindingPlan {
    let arguments = serde_json::json!({"path": "."});
    let address = ToolCallAddress {
        message: ModelMessageAddress {
            session_id: session_id(),
            turn_id: turn_id(),
            model_step_ordinal: 0,
        },
        part_ordinal: 0,
    };
    let args_digest = arguments_digest(&arguments);
    let binding = PlannedBinding {
        invocation_id: invocation_id(1),
        binding_digest: crate::pending::binding_digest(
            &invocation_id(1),
            &address,
            "observe.workspace",
            &"d".repeat(64),
            &args_digest,
        ),
        address,
        method: "observe.workspace".to_string(),
        descriptor_identity: "d".repeat(64),
        arguments_digest: args_digest,
    };
    crate::pending::BindingPlan {
        plan_key: PlanKey {
            turn_id: turn_id(),
            model_step_ordinal: 0,
        },
        message: NativeMessage::new(
            MessageRole::Assistant,
            Some(ProviderOrigin::Anthropic),
            vec![tool_call_part("observe.workspace", arguments)],
        ),
        bindings: vec![binding],
    }
}

/// A pinned TurnCommit candidate closing the Turn as completed.
pub(crate) fn completed_candidate(parent: Option<String>) -> TurnCommitCandidate {
    TurnCommitCandidate {
        session_id: SESSION.to_string(),
        turn_id: turn_id(),
        parent_ids: parent.into_iter().collect(),
        messages: vec![
            user_msg("please continue"),
            crate::test_helpers::assistant_msg("done"),
        ],
        agent_name: "coder".to_string(),
        profile_snapshot_hash: "test-profile".to_string(),
        profile_snapshot_json: "{}".to_string(),
        model: "test-model".to_string(),
        provider: "test-provider".to_string(),
        outcome: TurnOutcome::Completed {
            finish_reason: TurnFinishReason::Stop,
        },
        origin: ClosureOrigin::Live,
        commit_message: "please continue".to_string(),
        usage_aggregate: Some(TurnUsageAggregate::Complete {
            usage: lattice_conversation::TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
        }),
        workspace_history_oid: None,
        created_at: CREATED_AT.to_string(),
    }
}

pub(crate) fn turn_closing(candidate: TurnCommitCandidate) -> TurnClosingFacts {
    TurnClosingFacts {
        turn_id: turn_id(),
        candidate,
    }
}
