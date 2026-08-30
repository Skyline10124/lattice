// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::super::config::{
    FrozenTurnConfig, ToolCatalogEntry, ToolCatalogSnapshot, TurnExecutionLimits,
};
use super::*;
use lancet_protocol::{ModelMessageAddress, PlanKey, SessionId, ToolCallAddress};
use lattice_conversation::{
    MessagePart, MessageRole, MessageSemantic, ProviderOrigin, TokenUsage, ToolResultOutcome,
};

const TURN: &str = "3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c";
const SESSION: &str = "3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c";
const INVOCATION: &str = "00000000-0000-4000-8000-000000000001";

fn turn_id() -> TurnId {
    TurnId::parse(TURN).unwrap()
}

fn session_id() -> SessionId {
    SessionId::parse(SESSION).unwrap()
}

fn attempt_key(step: u32, attempt: u32) -> ModelAttemptKey {
    ModelAttemptKey {
        turn_id: turn_id(),
        model_step_ordinal: step,
        attempt_ordinal: attempt,
    }
}

fn turn_started_facts() -> TurnStartedFacts {
    TurnStartedFacts {
        turn_id: turn_id(),
        session_id: session_id(),
        project_store_key: crate::ProjectStoreKey::from_canonical_path_bytes(b"/test/project"),
        parent_conversation_tip: None,
        user_message: NativeMessage::new(
            MessageRole::User,
            None,
            vec![MessagePart::semantic_only(MessageSemantic::Text {
                text: "hello".to_string(),
            })],
        ),
        config: FrozenTurnConfig {
            agent_name: "coder".to_string(),
            model: "m".to_string(),
            provider: "p".to_string(),
            system_prompt: None,
            profile_snapshot_json: "{}".to_string(),
            policy_snapshot_json: "{}".to_string(),
            catalog: ToolCatalogSnapshot::new(
                "b".to_string(),
                vec![ToolCatalogEntry {
                    name: "t".to_string(),
                    descriptor_identity: "d".to_string(),
                    description: "t".to_string(),
                    input_schema: serde_json::json!({}),
                }],
            )
            .unwrap(),
            limits: TurnExecutionLimits {
                timeout_secs: 30,
                max_model_steps: MAX_MODEL_STEPS,
                max_model_attempts: 2,
            },
        },
        accepted_at: "2026-08-16T00:00:00Z".to_string(),
    }
}

fn turn_started() -> PendingStage {
    PendingStage::TurnStarted(turn_started_facts())
}

fn zero_tool_message() -> NativeMessage<UnboundToolCall> {
    NativeMessage::new(
        MessageRole::Assistant,
        Some(ProviderOrigin::Anthropic),
        vec![MessagePart::semantic_only(MessageSemantic::Text {
            text: "done".to_string(),
        })],
    )
}

fn one_call_plan() -> BindingPlan {
    let invocation = lancet_protocol::InvocationId::parse(INVOCATION).unwrap();
    let address = ToolCallAddress {
        message: ModelMessageAddress {
            session_id: session_id(),
            turn_id: turn_id(),
            model_step_ordinal: 0,
        },
        part_ordinal: 0,
    };
    let arguments = serde_json::json!({"a": 1});
    let args_digest = lattice_conversation::arguments_digest(&arguments);
    let binding = super::super::binding::PlannedBinding {
        binding_digest: super::super::binding::PlannedBinding::compute_binding_digest(
            &invocation,
            &address,
            "t",
            "d",
            &args_digest,
        ),
        invocation_id: invocation,
        address,
        method: "t".to_string(),
        descriptor_identity: "d".to_string(),
        arguments_digest: args_digest,
    };
    BindingPlan {
        plan_key: PlanKey {
            turn_id: turn_id(),
            model_step_ordinal: 0,
        },
        message: NativeMessage::new(
            MessageRole::Assistant,
            Some(ProviderOrigin::Anthropic),
            vec![MessagePart::semantic_only(MessageSemantic::ToolCall {
                call: UnboundToolCall {
                    provider_call_id: None,
                    name: "t".to_string(),
                    arguments,
                },
            })],
        ),
        bindings: vec![binding],
    }
}

fn step_closing_cursor_0(plan: &BindingPlan) -> super::super::step::StepClosingState {
    super::super::step::StepClosingState {
        plan_key: plan.plan_key,
        bindings: plan.bindings.clone(),
        closure_cursor: 0,
        closures: vec![],
        current_candidate: None,
    }
}

fn unavailable_closure(plan: &BindingPlan) -> super::super::step::ClosedBinding {
    super::super::step::ClosedBinding {
        binding: plan.bindings[0].clone(),
        outcome: ToolResultOutcome::NotExecuted,
        material: super::super::step::MaterialAvailability::Unavailable {
            reason: super::super::facts::UnavailableMaterialReason::NotExecuted,
            digest: None,
            byte_length: None,
        },
    }
}

fn post_user_transcript_for(stage: &PendingStage) -> Vec<NativeMessage<BoundToolCall>> {
    match stage {
        PendingStage::TurnStarted(_)
        | PendingStage::ModelAttemptStarted { .. }
        | PendingStage::ModelAttemptFailed { .. }
        | PendingStage::ModelAttemptInterrupted { .. }
        | PendingStage::ModelAttemptRejected { .. } => Vec::new(),
        PendingStage::MessageComplete { message, .. } => {
            vec![message.clone().bind_zero_tool().unwrap()]
        }
        PendingStage::BindingPlanned { plan, .. } | PendingStage::Bound { plan } => {
            vec![plan.bound_message().unwrap()]
        }
        PendingStage::StepClosing(state) => {
            let mut transcript = vec![one_call_plan().bound_message().unwrap()];
            transcript.extend(
                state
                    .closures
                    .iter()
                    .map(|closure| super::super::step::closure_transcript(closure, None)),
            );
            transcript
        }
        PendingStage::StepClosed {
            kind: StepClosedKind::FinishTurn { final_message },
        } => vec![final_message.clone().bind_zero_tool().unwrap()],
        PendingStage::StepClosed {
            kind:
                StepClosedKind::ContinueModel {
                    ordered_tool_results,
                    ..
                },
        } => {
            let mut transcript = vec![one_call_plan().bound_message().unwrap()];
            transcript.extend(
                ordered_tool_results
                    .iter()
                    .map(|closure| super::super::step::closure_transcript(closure, None)),
            );
            transcript
        }
        PendingStage::StepClosed { .. } => Vec::new(),
        PendingStage::TurnClosing(facts) => facts.candidate.messages.iter().skip(1).cloned().collect(),
        PendingStage::Committed(facts) => facts.candidate.messages.iter().skip(1).cloned().collect(),
    }
}

fn snapshot_of(stage: PendingStage) -> PendingSnapshot {
    let post_user_transcript = post_user_transcript_for(&stage);
    PendingSnapshot {
        schema_version: PENDING_SNAPSHOT_SCHEMA_VERSION,
        session_id: session_id(),
        turn_id: turn_id(),
        predecessor: Some("0".repeat(40)),
        post_user_transcript,
        stage,
        recorded_at: None,
        root: None,
        attempt_usages: Vec::new(),
    }
}

fn rooted_snapshot(
    stage: PendingStage,
    post_user_transcript: Vec<NativeMessage<BoundToolCall>>,
    recorded_at: i64,
) -> PendingSnapshot {
    PendingSnapshot {
        schema_version: PENDING_SNAPSHOT_SCHEMA_VERSION,
        session_id: session_id(),
        turn_id: turn_id(),
        predecessor: Some("0".repeat(40)),
        post_user_transcript,
        stage,
        recorded_at: Some(recorded_at),
        root: Some(turn_started_facts()),
        attempt_usages: Vec::new(),
    }
}

#[test]
fn unpinned_dispatch_started_closes_as_exact_indeterminate() {
    let plan = one_call_plan();
    let previous = step_closing_cursor_0(&plan);
    let mut next = previous.clone();
    next.closure_cursor = 1;
    next.closures.push(super::super::step::ClosedBinding {
        binding: plan.bindings[0].clone(),
        outcome: ToolResultOutcome::Indeterminate,
        material: super::super::step::MaterialAvailability::Unavailable {
            reason: super::super::facts::UnavailableMaterialReason::DraftPublicationFailed,
            digest: None,
            byte_length: None,
        },
    });
    validate_transition(
        &PendingStage::StepClosing(previous.clone()),
        &PendingStage::StepClosing(next.clone()),
    )
    .expect("DispatchStarted before candidate pin is conservatively indeterminate");

    next.closures[0].material = super::super::step::MaterialAvailability::Unavailable {
        reason: super::super::facts::UnavailableMaterialReason::DraftPublicationFailed,
        digest: Some([7; 32]),
        byte_length: None,
    };
    assert!(
        validate_transition(
            &PendingStage::StepClosing(previous),
            &PendingStage::StepClosing(next),
        )
        .is_err(),
        "unpinned indeterminate material must not invent a digest",
    );
}

#[test]
fn turn_started_must_be_first() {
    let started = turn_started();
    assert!(matches!(
        validate_transition(
            &started,
            &PendingStage::ModelAttemptStarted {
                key: attempt_key(0, 0)
            }
        ),
        Ok(())
    ));
    assert!(validate_transition(&started, &turn_started()).is_err());
    assert!(
        validate_transition(
            &started,
            &PendingStage::ModelAttemptStarted {
                key: attempt_key(1, 0)
            }
        )
        .is_err()
    );
}

#[test]
fn retry_only_after_retryable_pre_stream_failure_of_next_ordinal() {
    let started = PendingStage::ModelAttemptStarted {
        key: attempt_key(0, 0),
    };
    let retryable = PendingStage::ModelAttemptFailed {
        key: attempt_key(0, 0),
        retryable: true,
        usage: None,
    };
    let terminal = PendingStage::ModelAttemptFailed {
        key: attempt_key(0, 0),
        retryable: false,
        usage: None,
    };
    assert!(
        validate_transition(
            &retryable,
            &PendingStage::ModelAttemptStarted {
                key: attempt_key(0, 1)
            }
        )
        .is_ok()
    );
    assert!(
        validate_transition(
            &terminal,
            &PendingStage::ModelAttemptStarted {
                key: attempt_key(0, 1)
            }
        )
        .is_err()
    );
    assert!(
        validate_transition(
            &retryable,
            &PendingStage::ModelAttemptStarted {
                key: attempt_key(0, 2)
            }
        )
        .is_err()
    );
    assert!(
        validate_transition(
            &retryable,
            &PendingStage::ModelAttemptStarted {
                key: attempt_key(1, 1)
            }
        )
        .is_err()
    );
    assert!(
        validate_transition(
            &started,
            &PendingStage::ModelAttemptFailed {
                key: attempt_key(0, 1),
                retryable: true,
                usage: None
            }
        )
        .is_err()
    );
}

#[test]
fn attempt_outcomes_must_follow_the_same_started_attempt() {
    let started = PendingStage::ModelAttemptStarted {
        key: attempt_key(0, 0),
    };
    for stage in [
        PendingStage::ModelAttemptFailed {
            key: attempt_key(0, 0),
            retryable: false,
            usage: None,
        },
        PendingStage::ModelAttemptInterrupted {
            key: attempt_key(0, 0),
            reason: AttemptInterruption::StreamError,
            usage: None,
        },
        PendingStage::ModelAttemptRejected {
            key: attempt_key(0, 0),
            rejection: AttemptRejection::InvalidToolCall {
                validation: lattice_conversation::ToolCallValidation::MalformedArguments {
                    call_ordinal: 0,
                },
            },
            usage: None,
        },
        PendingStage::MessageComplete {
            key: attempt_key(0, 0),
            message: zero_tool_message(),
            usage: None,
        },
    ] {
        assert!(validate_transition(&started, &stage).is_ok(), "{stage:?}");
    }
    assert!(
        validate_transition(
            &started,
            &PendingStage::MessageComplete {
                key: attempt_key(0, 1),
                message: zero_tool_message(),
                usage: None
            }
        )
        .is_err()
    );
}

#[test]
fn bound_requires_the_byte_equal_plan_and_closing_starts_at_cursor_zero() {
    let planned = PendingStage::BindingPlanned {
        plan: one_call_plan(),
        usage: None,
    };
    let bound = PendingStage::Bound {
        plan: one_call_plan(),
    };
    assert!(validate_transition(&planned, &bound).is_ok());
    let mut drifted = one_call_plan();
    drifted.bindings[0].descriptor_identity = "other".to_string();
    assert!(validate_transition(&planned, &PendingStage::Bound { plan: drifted }).is_err());

    let closing = PendingStage::StepClosing(step_closing_cursor_0(&one_call_plan()));
    assert!(validate_transition(&bound, &closing).is_ok());
    let mut jumped = step_closing_cursor_0(&one_call_plan());
    jumped.closure_cursor = 1;
    assert!(validate_transition(&bound, &PendingStage::StepClosing(jumped)).is_err());

    let plan = one_call_plan();
    let baseline = PendingStage::StepClosing(step_closing_cursor_0(&plan));
    let mut oversize = step_closing_cursor_0(&plan);
    oversize.closure_cursor = 1;
    oversize.closures.push(super::super::step::ClosedBinding {
        binding: plan.bindings[0].clone(),
        outcome: ToolResultOutcome::Succeeded,
        material: super::super::step::MaterialAvailability::Unavailable {
            reason: super::super::facts::UnavailableMaterialReason::OutputTooLarge,
            digest: Some([7; 32]),
            byte_length: Some(65_537),
        },
    });
    assert!(validate_transition(
        &baseline,
        &PendingStage::StepClosing(oversize.clone()),
    )
    .is_ok());

    let super::super::step::MaterialAvailability::Unavailable { digest, .. } =
        &mut oversize.closures[0].material
    else {
        unreachable!();
    };
    *digest = None;
    assert!(validate_transition(&baseline, &PendingStage::StepClosing(oversize)).is_err());
}

#[test]
fn committed_is_never_publishable_through_active_turn() {
    let started = turn_started();
    let facts = CommittedFacts {
        turn_id: turn_id(),
        candidate: candidate(),
    };
    assert!(validate_transition(&started, &PendingStage::Committed(facts)).is_err());
}

#[test]
fn message_complete_must_be_zero_tool() {
    let valid_message = zero_tool_message();
    let mut snapshot = PendingSnapshot {
        schema_version: PENDING_SNAPSHOT_SCHEMA_VERSION,
        session_id: session_id(),
        turn_id: turn_id(),
        predecessor: None,
        post_user_transcript: vec![valid_message.clone().bind_zero_tool().unwrap()],
        stage: PendingStage::MessageComplete {
            key: attempt_key(0, 0),
            message: one_call_plan().message,
            usage: None,
        },
        recorded_at: None,
        root: None,
        attempt_usages: Vec::new(),
    };
    assert!(snapshot.validate().is_err());
    snapshot.stage = PendingStage::MessageComplete {
        key: attempt_key(0, 0),
        message: valid_message,
        usage: None,
    };
    snapshot.validate().expect("zero-tool message completes");
}

#[test]
fn step_limit_is_enforced_on_attempt_keys() {
    let mut snapshot = PendingSnapshot {
        schema_version: PENDING_SNAPSHOT_SCHEMA_VERSION,
        session_id: session_id(),
        turn_id: turn_id(),
        predecessor: None,
        post_user_transcript: Vec::new(),
        stage: PendingStage::ModelAttemptStarted {
            key: attempt_key(MAX_MODEL_STEPS, 0),
        },
        recorded_at: None,
        root: None,
        attempt_usages: Vec::new(),
    };
    assert!(
        snapshot.validate().is_err(),
        "a model step ordinal at the code-defined limit is corruption"
    );
    snapshot.stage = PendingStage::ModelAttemptStarted {
        key: attempt_key(MAX_MODEL_STEPS - 1, 0),
    };
    snapshot.validate().expect("the last legal step ordinal");
}

fn candidate() -> TurnCommitCandidate {
    TurnCommitCandidate {
        session_id: SESSION.to_string(),
        turn_id: turn_id(),
        side_parent_ids: vec![],
        messages: vec![crate::test_helpers::user_msg("hello")],
        agent_name: "coder".to_string(),
        profile_snapshot_hash: "h".to_string(),
        profile_snapshot_json: "{}".to_string(),
        model: "m".to_string(),
        provider: "p".to_string(),
        outcome: TurnOutcome::Interrupted {
            reason: super::super::outcome::TurnInterruption::ProcessInterruptedBeforeModelAttempt,
        },
        origin: ClosureOrigin::Reconciled,
        commit_message: "hello".to_string(),
        usage_aggregate: None,
        workspace_history_checkpoint: crate::pending::WorkspaceHistoryCheckpoint::Empty,
        created_at: "2026-08-16T00:00:00Z".to_string(),
    }
}

#[test]
fn turn_closing_and_committed_facts_validates() {
    let closing = PendingStage::TurnClosing(TurnClosingFacts {
        turn_id: turn_id(),
        candidate: candidate(),
    });
    closing.validate().expect("valid closing");
    let committed = PendingStage::Committed(CommittedFacts {
        turn_id: turn_id(),
        candidate: candidate(),
    });
    committed.validate().expect("valid committed");
    assert!(
        validate_transition(
            &PendingStage::StepClosed {
                kind: super::super::step::StepClosedKind::InterruptTurn {
                    reason: super::super::outcome::TurnInterruption::ProcessInterruptedBeforeModelAttempt,
                },
            },
            &closing,
        )
        .is_ok()
    );
}

#[test]
fn snapshot_serde_round_trip_preserves_stage() {
    let snapshot = PendingSnapshot {
        schema_version: PENDING_SNAPSHOT_SCHEMA_VERSION,
        session_id: session_id(),
        turn_id: turn_id(),
        predecessor: None,
        post_user_transcript: Vec::new(),
        stage: turn_started(),
        recorded_at: None,
        root: None,
        attempt_usages: Vec::new(),
    };
    let value = serde_json::to_value(&snapshot).unwrap();
    assert!(value.get("recorded_at").is_none(), "recorded_at stays off the wire");
    assert!(value.get("attempt_usages").is_none(), "derived usage stays off the wire");
    let back: PendingSnapshot = serde_json::from_value(value).unwrap();
    assert_eq!(back, snapshot);
    back.validate().expect("round-tripped");
}

#[test]
fn turn_started_project_binding_is_required_and_strictly_typed() {
    let snapshot = PendingSnapshot {
        schema_version: PENDING_SNAPSHOT_SCHEMA_VERSION,
        session_id: session_id(),
        turn_id: turn_id(),
        predecessor: None,
        post_user_transcript: Vec::new(),
        stage: turn_started(),
        recorded_at: None,
        root: None,
        attempt_usages: Vec::new(),
    };
    let mut missing = serde_json::to_value(&snapshot).unwrap();
    missing["stage"]
        .as_object_mut()
        .unwrap()
        .remove("project_store_key");
    assert!(serde_json::from_value::<PendingSnapshot>(missing).is_err());

    let mut corrupt = serde_json::to_value(snapshot).unwrap();
    corrupt["stage"]["project_store_key"] = serde_json::json!("not-a-project-key");
    assert!(serde_json::from_value::<PendingSnapshot>(corrupt).is_err());
}

fn continue_model(step: u32) -> StepClosedKind {
    let plan = one_call_plan();
    let closure = unavailable_closure(&plan);
    StepClosedKind::ContinueModel {
        model_step_ordinal: step,
        ordered_tool_results: vec![closure.clone()],
        conversation_digest: step_conversation_digest(&plan.bound_message().unwrap(), &[closure]),
    }
}

#[test]
fn continue_model_names_the_step_it_closed() {
    let ok = PendingStage::StepClosed {
        kind: continue_model(0),
    };
    ok.validate().expect("ordinal agrees with the closures");

    let StepClosedKind::ContinueModel {
        ordered_tool_results,
        conversation_digest,
        ..
    } = continue_model(0)
    else {
        unreachable!()
    };
    let lying = PendingStage::StepClosed {
        kind: StepClosedKind::ContinueModel {
            model_step_ordinal: 3,
            ordered_tool_results: ordered_tool_results.clone(),
            conversation_digest,
        },
    };
    assert!(
        matches!(lying.validate(), Err(SessionError::JournalCorruption { .. })),
        "a ContinueModel ordinal must not contradict its closures"
    );

    let empty = PendingStage::StepClosed {
        kind: StepClosedKind::ContinueModel {
            model_step_ordinal: 0,
            ordered_tool_results: vec![],
            conversation_digest,
        },
    };
    assert!(
        matches!(empty.validate(), Err(SessionError::JournalCorruption { .. })),
        "ContinueModel closes a plan, so it always carries closures"
    );
}

#[test]
fn the_step_after_continue_model_is_contiguous_and_starts_at_attempt_zero() {
    let closed = PendingStage::StepClosed {
        kind: continue_model(0),
    };
    validate_transition(
        &closed,
        &PendingStage::ModelAttemptStarted {
            key: attempt_key(1, 0),
        },
    )
    .expect("step 0 continues into step 1 attempt 0");

    for illegal in [attempt_key(0, 0), attempt_key(2, 0), attempt_key(1, 1)] {
        assert!(
            validate_transition(&closed, &PendingStage::ModelAttemptStarted { key: illegal })
                .is_err(),
            "non-contiguous step or non-zero first attempt must be rejected: {illegal:?}"
        );
    }
}

#[test]
fn continue_model_must_name_the_plan_step_it_closes() {
    let plan = one_call_plan();
    let mut closing = step_closing_cursor_0(&plan);
    closing.closure_cursor = 1;
    closing.closures = vec![unavailable_closure(&plan)];
    validate_transition(
        &PendingStage::StepClosing(closing.clone()),
        &PendingStage::StepClosed {
            kind: continue_model(0),
        },
    )
    .expect("the plan's own step");
    assert!(
        validate_transition(
            &PendingStage::StepClosing(closing),
            &PendingStage::StepClosed {
                kind: continue_model(1),
            },
        )
        .is_err(),
        "a ContinueModel must close the step it names"
    );
}

#[test]
fn snapshot_rejects_addresses_from_another_session() {
    let other = SessionId::parse("00000000000000000000000000000001").unwrap();
    let mut plan = one_call_plan();
    plan.bindings[0].address.message.session_id = other;
    plan.bindings[0].binding_digest =
        super::super::binding::PlannedBinding::compute_binding_digest(
            &plan.bindings[0].invocation_id,
            &plan.bindings[0].address,
            &plan.bindings[0].method,
            &plan.bindings[0].descriptor_identity,
            &plan.bindings[0].arguments_digest,
        );
    plan.validate().expect("the tuple is internally consistent");

    for stage in [
        PendingStage::BindingPlanned {
            plan: plan.clone(),
            usage: None,
        },
        PendingStage::Bound { plan: plan.clone() },
        PendingStage::StepClosing(step_closing_cursor_0(&plan)),
    ] {
        let snapshot = snapshot_of(stage);
        assert!(
            matches!(snapshot.validate(), Err(SessionError::JournalCorruption { .. })),
            "a cross-session binding address must fail closed"
        );
    }
}

#[test]
fn snapshot_rejects_a_candidate_naming_another_session() {
    let candidate = TurnCommitCandidate {
        session_id: "00000000000000000000000000000001".to_string(),
        turn_id: turn_id(),
        side_parent_ids: vec![],
        messages: vec![crate::test_helpers::user_msg("hi")],
        agent_name: "coder".to_string(),
        profile_snapshot_hash: "h".to_string(),
        profile_snapshot_json: "{}".to_string(),
        model: "m".to_string(),
        provider: "p".to_string(),
        outcome: TurnOutcome::Completed {
            finish_reason: super::super::outcome::TurnFinishReason::Stop,
        },
        origin: ClosureOrigin::Reconciled,
        commit_message: "hi".to_string(),
        usage_aggregate: None,
        workspace_history_checkpoint: crate::pending::WorkspaceHistoryCheckpoint::Empty,
        created_at: "2026-08-16T00:00:00Z".to_string(),
    };
    let snapshot = snapshot_of(PendingStage::TurnClosing(TurnClosingFacts {
        turn_id: turn_id(),
        candidate,
    }));
    assert!(
        matches!(snapshot.validate(), Err(SessionError::JournalCorruption { .. })),
        "a candidate from another session must fail closed"
    );
}

#[test]
fn snapshot_transition_rejects_candidate_revision_usage_change() {
    let original = one_call_plan();
    let original_stage = PendingStage::BindingPlanned {
        plan: original.clone(),
        usage: Some(TokenUsage {
            prompt_tokens: 1,
            completion_tokens: 2,
            total_tokens: 3,
        }),
    };
    let previous = rooted_snapshot(
        original_stage,
        vec![original.bound_message().unwrap()],
        100,
    );

    let mut revised = original;
    revised.bindings[0].invocation_id =
        lancet_protocol::InvocationId::parse("00000000-0000-4000-8000-000000000002").unwrap();
    revised.bindings[0].binding_digest = super::super::binding::PlannedBinding::compute_binding_digest(
        &revised.bindings[0].invocation_id,
        &revised.bindings[0].address,
        &revised.bindings[0].method,
        &revised.bindings[0].descriptor_identity,
        &revised.bindings[0].arguments_digest,
    );
    let next = rooted_snapshot(
        PendingStage::BindingPlanned {
            plan: revised.clone(),
            usage: None,
        },
        vec![revised.bound_message().unwrap()],
        101,
    );
    assert!(
        validate_snapshot_transition(&previous, &next).is_err(),
        "candidate revision must preserve completing-attempt usage"
    );
}

#[test]
fn snapshot_transition_authenticates_continue_model_closures_and_digest() {
    let plan = one_call_plan();
    let closure = unavailable_closure(&plan);
    let transcript = vec![
        plan.bound_message().unwrap(),
        super::super::step::closure_transcript(&closure, None),
    ];
    let previous = rooted_snapshot(
        PendingStage::StepClosing(super::super::step::StepClosingState {
            plan_key: plan.plan_key,
            bindings: plan.bindings.clone(),
            closure_cursor: 1,
            closures: vec![closure.clone()],
            current_candidate: None,
        }),
        transcript.clone(),
        100,
    );
    let exact = rooted_snapshot(
        PendingStage::StepClosed {
            kind: StepClosedKind::ContinueModel {
                model_step_ordinal: 0,
                ordered_tool_results: vec![closure.clone()],
                conversation_digest:
                    step_conversation_digest(&transcript[0], std::slice::from_ref(&closure)),
            },
        },
        transcript.clone(),
        101,
    );
    validate_snapshot_transition(&previous, &exact).expect("exact closure edge");

    let mut forged_closure = closure.clone();
    forged_closure.material = super::super::step::MaterialAvailability::Unavailable {
        reason: super::super::facts::UnavailableMaterialReason::DraftPublicationFailed,
        digest: None,
        byte_length: None,
    };
    let forged_closures = rooted_snapshot(
        PendingStage::StepClosed {
            kind: StepClosedKind::ContinueModel {
                model_step_ordinal: 0,
                ordered_tool_results: vec![forged_closure.clone()],
                conversation_digest: step_conversation_digest(&transcript[0], &[forged_closure]),
            },
        },
        transcript.clone(),
        101,
    );
    assert!(validate_snapshot_transition(&previous, &forged_closures).is_err());

    let mut forged_digest = exact;
    let PendingStage::StepClosed {
        kind: StepClosedKind::ContinueModel {
            conversation_digest, ..
        },
    } = &mut forged_digest.stage
    else {
        unreachable!()
    };
    *conversation_digest = [9; 32];
    assert!(validate_snapshot_transition(&previous, &forged_digest).is_err());
}

#[test]
fn snapshot_transition_authenticates_the_turn_closing_candidate_basis() {
    let final_message = zero_tool_message();
    let transcript = vec![final_message.clone().bind_zero_tool().unwrap()];
    let previous = rooted_snapshot(
        PendingStage::StepClosed {
            kind: StepClosedKind::FinishTurn {
                final_message: final_message.clone(),
            },
        },
        transcript.clone(),
        100,
    );
    let candidate = previous
        .turn_commit_candidate_basis(
            TurnOutcome::Completed {
                finish_reason: super::super::outcome::TurnFinishReason::Stop,
            },
            ClosureOrigin::Live,
        )
        .unwrap()
        .with_workspace_history_checkpoint(crate::pending::WorkspaceHistoryCheckpoint::Empty)
        .unwrap();
    let exact = rooted_snapshot(
        PendingStage::TurnClosing(TurnClosingFacts {
            turn_id: turn_id(),
            candidate: candidate.clone(),
        }),
        transcript.clone(),
        101,
    );
    validate_snapshot_transition(&previous, &exact).expect("derived candidate");

    let mut forged = candidate;
    forged.model.push_str("-forged");
    let forged = rooted_snapshot(
        PendingStage::TurnClosing(TurnClosingFacts {
            turn_id: turn_id(),
            candidate: forged,
        }),
        transcript,
        101,
    );
    assert!(validate_snapshot_transition(&previous, &forged).is_err());
}

#[test]
fn partial_usage_aggregate_saturates_each_counter() {
    let aggregate = TurnUsageAggregate::derive(&[
        Some(TokenUsage {
            prompt_tokens: u64::MAX,
            completion_tokens: u64::MAX - 1,
            total_tokens: u64::MAX - 2,
        }),
        None,
        Some(TokenUsage {
            prompt_tokens: 1,
            completion_tokens: 2,
            total_tokens: 3,
        }),
    ])
    .expect("non-empty");
    assert_eq!(
        aggregate,
        TurnUsageAggregate::Partial {
            usage: TokenUsage {
                prompt_tokens: u64::MAX,
                completion_tokens: u64::MAX,
                total_tokens: u64::MAX,
            },
        }
    );
}
