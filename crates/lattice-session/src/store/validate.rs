// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use lancet_protocol::{
    CellEvidenceTriple, CepErrorCode, CepErrorFamily, InvocationBinding, InvocationId,
    ModelMessageAddress,
};
use lattice_conversation::{
    BoundToolCall, MessageRole, MessageSemantic, NativeMessage, ProviderOrigin, RawToolCall,
    ToolCallState, validate_tool_call_batch,
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn provider(name: &str) -> Result<ProviderOrigin> {
    match name {
        "openai" | "openai-compatible" | "openai_compatible" => {
            Ok(ProviderOrigin::OpenAiCompatible)
        }
        "anthropic" => Ok(ProviderOrigin::Anthropic),
        "gemini" => Ok(ProviderOrigin::Gemini),
        _ => Err(invalid("provider", "unsupported provider identity")),
    }
}
pub(super) fn config(config: &FrozenTurnConfig) -> Result<()> {
    config
        .validate()
        .map_err(|_| invalid("config", "invalid frozen configuration"))?;
    provider(&config.provider)?;
    if config.agent_name.len() > 128
        || config.model.len() > 4096
        || config
            .system_prompt
            .as_ref()
            .is_some_and(|s| s.len() > 2 * 1024 * 1024)
        || !(1..=3600).contains(&config.limits.timeout_secs)
    {
        return Err(invalid("config", "configuration bound exceeded"));
    }
    for json in [&config.profile_snapshot_json, &config.policy_snapshot_json] {
        let v: serde_json::Value =
            serde_json::from_str(json).map_err(|_| invalid("config", "invalid snapshot JSON"))?;
        if lancet_protocol::canonical_json(&v) != *json {
            return Err(invalid("config", "snapshot JSON is not canonical"));
        }
    }
    let rebuilt = ToolCatalogSnapshot::new(
        config.catalog.runtime_build_id.clone(),
        config.catalog.entries.clone(),
    )
    .map_err(|_| invalid("catalog", "invalid catalog entries"))?;
    if rebuilt != config.catalog
        || config.catalog.entries.iter().any(|e| {
            e.descriptor_identity.len() != 64
                || !e
                    .descriptor_identity
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    {
        return Err(invalid(
            "catalog",
            "catalog ordering or descriptor identity invalid",
        ));
    }
    Ok(())
}
pub(super) fn assistant<S: ToolCallState>(message: &NativeMessage<S>) -> Result<()> {
    if message.role != MessageRole::Assistant {
        return Err(invalid("reply", "expected assistant role"));
    }
    let mut text = 0usize;
    let mut reasoning = 0usize;
    let mut total = 0usize;
    let mut calls = Vec::new();
    for part in &message.parts {
        match &part.semantic {
            MessageSemantic::Text { text: t } => {
                text = text.saturating_add(t.len());
                total = total.saturating_add(t.len());
            }
            MessageSemantic::Reasoning { text: t } => {
                reasoning = reasoning.saturating_add(t.len());
                total = total.saturating_add(t.len());
            }
            MessageSemantic::ToolCall { call } => {
                let arguments = lancet_protocol::canonical_json(call.arguments());
                total = total
                    .saturating_add(arguments.len())
                    .saturating_add(call.name().len())
                    .saturating_add(call.provider_call_id().map_or(0, str::len));
                calls.push(RawToolCall {
                    provider_call_id: call.provider_call_id().map(str::to_owned),
                    name: call.name().into(),
                    arguments,
                });
            }
            MessageSemantic::ToolResult { .. } => {
                return Err(invalid("reply", "assistant contains tool result"));
            }
        }
        if let Some(replay) = &part.replay_state {
            total = total.saturating_add(replay.as_str().len());
        }
    }
    if text > 2 * 1024 * 1024 || reasoning > 2 * 1024 * 1024 || total > 16 * 1024 * 1024 {
        return Err(invalid("reply", "response byte limit exceeded"));
    }
    validate_tool_call_batch(&calls).map_err(|_| invalid("reply", "invalid tool call batch"))?;
    Ok(())
}
pub(super) fn diagnostic(cause: &StopCause, d: &AttemptDiagnostic) -> Result<()> {
    if matches!(
        cause,
        StopCause::InvocationIndeterminate { .. } | StopCause::ModelStepLimit
    ) {
        return Err(invalid("attempt", "step-only stop cause"));
    }
    if matches!(cause, StopCause::ProviderError { .. }) && d.provider_error.is_none() {
        return Err(invalid("diagnostic", "missing provider error"));
    }
    if d.provider_error
        .as_ref()
        .is_some_and(|s| s.chars().count() > MAX_PROVIDER_ERROR_CHARS)
    {
        return Err(invalid("diagnostic", "provider error too long"));
    }
    let mut text = 0usize;
    let mut reasoning = 0usize;
    let mut total = 0usize;
    for part in &d.partial {
        let n = match part {
            DiagnosticPart::Text { text: t } => {
                text = text.saturating_add(t.len());
                t.len()
            }
            DiagnosticPart::Reasoning { text: t } => {
                reasoning = reasoning.saturating_add(t.len());
                t.len()
            }
            DiagnosticPart::ToolCall {
                provider_call_id,
                name,
                arguments,
            } => {
                if arguments.len() > 1024 * 1024 {
                    return Err(invalid("diagnostic", "arguments too large"));
                }
                name.len()
                    .saturating_add(arguments.len())
                    .saturating_add(provider_call_id.as_ref().map_or(0, String::len))
            }
        };
        total = total.saturating_add(n);
    }
    if text > 2 * 1024 * 1024 || reasoning > 2 * 1024 * 1024 || total > 16 * 1024 * 1024 {
        return Err(invalid("diagnostic", "partial response too large"));
    }
    Ok(())
}
pub(super) fn evidence(binding: &InvocationBinding, e: &CellEvidenceTriple) -> Result<()> {
    use lancet_protocol::evidence::{
        ADMISSION_EVIDENCE_VERSION, DENIED_OPERATIONS_RECORD_VERSION,
        OBSERVED_EFFECTS_RECORD_VERSION,
    };
    if e.admission.version != ADMISSION_EVIDENCE_VERSION
        || e.effects.version != OBSERVED_EFFECTS_RECORD_VERSION
        || e.denied.version != DENIED_OPERATIONS_RECORD_VERSION
        || e.admission.tool != binding.identity.method
    {
        return Err(invalid("evidence", "evidence version or method mismatch"));
    }
    let mut seq = BTreeSet::new();
    if e.effects.effects.windows(2).any(|w| w[0].seq >= w[1].seq)
        || e.denied.denied.windows(2).any(|w| w[0].seq >= w[1].seq)
        || e.effects
            .effects
            .iter()
            .map(|e| e.seq)
            .chain(e.denied.denied.iter().map(|e| e.seq))
            .any(|s| !seq.insert(s))
    {
        return Err(invalid("evidence", "invalid operation order"));
    }
    for names in [
        &e.admission.approved_capabilities,
        &e.admission.denied_capabilities,
        &e.admission.boundary_excluded,
    ] {
        if names.windows(2).any(|w| w[0] >= w[1]) {
            return Err(invalid("evidence", "capabilities not sorted and unique"));
        }
    }
    Ok(())
}
pub(super) fn terminal(binding: &InvocationBinding, t: &InvocationTerminal) -> Result<()> {
    if t.result.invocation_id != binding.invocation_id || t.result.name != binding.identity.method {
        return Err(invalid("terminal", "result identity mismatch"));
    }
    match (&t.result.payload, &t.evidence) {
        (
            ResultPayload::NotExecuted(_),
            InvocationEvidence::Unavailable {
                reason: EvidenceUnavailableReason::NotDispatched,
            },
        ) => {}
        (
            ResultPayload::Indeterminate(_),
            InvocationEvidence::Unavailable {
                reason: EvidenceUnavailableReason::ProcessExit,
            },
        ) => {}
        (ResultPayload::NotExecuted(_), _) => {
            return Err(invalid(
                "terminal",
                "not-executed receipt has execution evidence",
            ));
        }
        (_, InvocationEvidence::Recorded(e)) => evidence(binding, e)?,
        _ => return Err(invalid("terminal", "missing receipt evidence")),
    }
    match &t.result.payload {
        ResultPayload::Succeeded(SuccessContent::Json {
            value,
            digest,
            byte_length,
        }) => {
            let bytes = lancet_protocol::canonical_json(value);
            if bytes.len() > MAX_TOOL_CONTENT_BYTES
                || bytes.len() as u64 != *byte_length
                || lattice_conversation::tool_content_digest(&bytes) != *digest
            {
                return Err(invalid("terminal", "success content identity mismatch"));
            }
        }
        ResultPayload::Succeeded(SuccessContent::OutputTooLarge { byte_length, .. })
            if *byte_length <= MAX_TOOL_CONTENT_BYTES as u64 =>
        {
            return Err(invalid("terminal", "unnecessary unavailable content"));
        }
        ResultPayload::Failed(e)
            if matches!(
                e.code,
                CepErrorCode::CallCancelled | CepErrorCode::OutcomeIndeterminate
            ) || matches!(
                e.code.family(),
                CepErrorFamily::Validation | CepErrorFamily::Permission | CepErrorFamily::Guard
            ) =>
        {
            return Err(invalid("terminal", "error belongs to a different outcome"));
        }
        ResultPayload::Rejected(r)
            if !matches!(
                r.code.family(),
                CepErrorFamily::Validation | CepErrorFamily::Permission | CepErrorFamily::Guard
            ) =>
        {
            return Err(invalid("terminal", "invalid rejection code"));
        }
        _ => {}
    }
    Ok(())
}
/// 终态前缀，最多一个 Started，余下 Registered；停止之后只能补 NotExecuted。
pub(super) fn slot_order(slots: &[InvocationSlot]) -> Result<()> {
    let mut waiting = false;
    let mut stopping = false;
    let mut indeterminate = false;
    let mut tail_reason = None;
    let mut last = None;
    let mut ids = BTreeSet::new();
    let mut message = None;
    for slot in slots {
        let address = slot.binding.identity.address();
        if last.is_some_and(|p| p >= address.part_ordinal)
            || !ids.insert(slot.binding.invocation_id)
            || message.is_some_and(|m| m != address.message)
        {
            return Err(corrupt("steps", "slots", "invalid slot order or ownership"));
        }
        last = Some(address.part_ordinal);
        message = Some(address.message);
        match &slot.state {
            SlotState::Registered => waiting = true,
            SlotState::Started => {
                if waiting || stopping {
                    return Err(corrupt("steps", "slots", "out-of-order start"));
                }
                waiting = true;
            }
            SlotState::Terminal(t) => {
                terminal(&slot.binding, t)
                    .map_err(|_| corrupt("steps", "terminal", "invalid terminal"))?;
                if waiting || stopping && !matches!(t.result.payload, ResultPayload::NotExecuted(_))
                {
                    return Err(corrupt(
                        "steps",
                        "slots",
                        "execution after unfinished or stopping slot",
                    ));
                }
                if matches!(
                    t.result.payload,
                    ResultPayload::NotExecuted(NotExecutedReason::PrecedingInvocationIndeterminate)
                ) && !indeterminate
                {
                    return Err(corrupt("steps", "slots", "missing preceding indeterminate"));
                }
                if let ResultPayload::NotExecuted(reason) = &t.result.payload {
                    if indeterminate
                        && *reason != NotExecutedReason::PrecedingInvocationIndeterminate
                        || tail_reason.is_some_and(|previous| previous != reason)
                    {
                        return Err(corrupt(
                            "steps",
                            "slots",
                            "inconsistent closure tail reason",
                        ));
                    }
                    tail_reason = Some(reason);
                }
                indeterminate |= matches!(t.result.payload, ResultPayload::Indeterminate(_));
                stopping |= close::stopping_cause(&slot.binding, &t.result.payload).is_some();
            }
        }
    }
    Ok(())
}
fn bound_reply(
    turn: &TurnRecord,
    key: StepKey,
    message: &NativeMessage<BoundToolCall>,
    slots: &[InvocationSlot],
) -> Result<()> {
    assistant(message)?;
    if message
        .provider_origin
        .is_some_and(|origin| provider(&turn.config.provider).ok() != Some(origin))
    {
        return Err(invalid("reply", "stored reply provider mismatch"));
    }
    slot_order(slots)?;
    if message.tool_calls().count() != slots.len() {
        return Err(invalid("slots", "reply/slot count mismatch"));
    }
    for ((ordinal, call), slot) in message.tool_calls().zip(slots) {
        let entry = turn
            .config
            .catalog
            .entries
            .iter()
            .find(|e| e.name == call.name)
            .ok_or_else(|| invalid("slots", "unknown tool"))?;
        let b = &slot.binding;
        let a = b.identity.address();
        if a.message
            != (ModelMessageAddress {
                session_id: turn.session_id,
                turn_id: turn.turn_id,
                model_step_ordinal: key.ordinal,
            })
            || a.part_ordinal as usize != ordinal
            || call.address != a
            || call.invocation_id != b.invocation_id
            || b.identity.method != call.name
            || b.identity.descriptor_identity != entry.descriptor_identity
            || b.identity.arguments_digest != call.arguments_digest
            || call.arguments_digest != lancet_protocol::arguments_digest(&call.arguments)
        {
            return Err(invalid("slots", "binding/reply mismatch"));
        }
    }
    Ok(())
}
fn retryable(attempt: &AttemptRecord) -> bool {
    matches!(
        attempt.end,
        AttemptEnd::Stopped {
            cause: StopCause::ProviderError { retryable: true },
            ..
        }
    )
}
pub(super) fn durable(state: &DurableTurnState) -> Result<()> {
    let t = &state.turn;
    let s = &state.session;
    let err = || corrupt("turns", &t.turn_id.to_string(), "invalid turn/step shape");
    config(&t.config).map_err(|_| err())?;
    if t.config.digest().map_err(|_| err())? != t.config_digest
        || t.session_id != s.session_id
        || t.project_store_key != s.project_store_key
        || state.next_node.0 == 0
        || state.next_node <= s.current
        || state.steps.len() > MAX_MODEL_STEPS as usize + 1
    {
        return Err(err());
    }
    if t.user_node == NodeId(0) || t.user_node >= state.next_node {
        return Err(err());
    }
    let mut last_node = t.user_node;
    for step in &state.steps {
        if let StepState::Closed { end, invocations } = &step.state {
            if let Some(node) = end.node() {
                if node <= last_node || node >= state.next_node {
                    return Err(err());
                }
                last_node = node;
            }
            for invocation in invocations {
                let b = &invocation.binding.identity;
                if b.session_id != t.session_id
                    || b.turn_id != t.turn_id
                    || b.model_step_ordinal != step.key.ordinal
                    || !t.config.catalog.entries.iter().any(|e| {
                        e.name == b.method && e.descriptor_identity == b.descriptor_identity
                    })
                {
                    return Err(err());
                }
            }
        }
    }
    if matches!(t.state, TurnState::Running) && s.current != last_node {
        return Err(err());
    }
    let running = matches!(t.state, TurnState::Running);
    if running != (s.active_turn == Some(t.turn_id)) || (!running && state.steps.is_empty()) {
        return Err(err());
    }
    for (ordinal, step) in state.steps.iter().enumerate() {
        if step.key
            != (StepKey {
                turn_id: t.turn_id,
                ordinal: ordinal as u32,
            })
            || step.attempts.len() > t.config.limits.max_model_attempts as usize
            || ordinal >= MAX_MODEL_STEPS as usize && !step.attempts.is_empty()
        {
            return Err(err());
        }
        let mut complete = false;
        for (index, a) in step.attempts.iter().enumerate() {
            if a.attempt as usize != index
                || complete
                || a.origin != provider(&t.config.provider).map_err(|_| err())?
            {
                return Err(err());
            }
            match &a.end {
                AttemptEnd::Completed { step: address } => {
                    if *address
                        != (ModelMessageAddress {
                            session_id: t.session_id,
                            turn_id: t.turn_id,
                            model_step_ordinal: ordinal as u32,
                        })
                    {
                        return Err(err());
                    }
                    complete = true;
                }
                AttemptEnd::Stopped {
                    cause,
                    diagnostic: d,
                } => {
                    diagnostic(cause, d).map_err(|_| err())?;
                    if index + 1 < step.attempts.len() && !retryable(a) {
                        return Err(err());
                    }
                }
            }
        }
        let last = ordinal + 1 == state.steps.len();
        match &step.state {
            StepState::Active { active } => {
                if !running || !last || ordinal >= MAX_MODEL_STEPS as usize {
                    return Err(err());
                }
                match active {
                    ActiveStep::Requesting { attempt } => {
                        if *attempt as usize != step.attempts.len()
                            || *attempt >= t.config.limits.max_model_attempts
                            || complete
                            || step.attempts.last().is_some_and(|a| !retryable(a))
                        {
                            return Err(err());
                        }
                    }
                    ActiveStep::RetryReady { next_attempt } => {
                        if *next_attempt as usize != step.attempts.len()
                            || *next_attempt >= t.config.limits.max_model_attempts
                            || step.attempts.last().is_none_or(|a| !retryable(a))
                        {
                            return Err(err());
                        }
                    }
                    ActiveStep::Executing { reply, slots } => {
                        if !complete || slots.is_empty() || slots.iter().any(|slot| {
                            matches!(&slot.state, SlotState::Terminal(t) if !matches!(t.evidence, InvocationEvidence::Recorded(_)) || matches!(t.result.payload, ResultPayload::NotExecuted(_)))
                        }) {
                            // 无 receipt 的补写只能与 Step/Turn 关闭同事务发布。
                            return Err(err());
                        }
                        bound_reply(t, step.key, reply, slots).map_err(|_| err())?;
                    }
                }
            }
            StepState::Closed { end, invocations } => {
                let slots: Vec<_> = invocations
                    .iter()
                    .map(|s| InvocationSlot {
                        binding: s.binding.clone(),
                        state: SlotState::Terminal(s.terminal.clone()),
                    })
                    .collect();
                slot_order(&slots)?;
                if matches!(end, StepEnd::Continue { .. }) != (running || !last) {
                    return Err(err());
                }
                if complete != end.node().is_some() || !complete && !invocations.is_empty() {
                    return Err(err());
                }
                match end {
                    StepEnd::Continue { .. } => {
                        if invocations.is_empty() || first_stopping_cause(invocations)?.is_some() {
                            return Err(err());
                        }
                    }
                    StepEnd::Finish { .. } => {
                        if !invocations.is_empty() {
                            return Err(err());
                        }
                    }
                    StepEnd::Stop {
                        node: Some(_),
                        cause,
                    } => {
                        if invocations.is_empty()
                            || *cause
                                != first_stopping_cause(invocations)?
                                    .unwrap_or(StopCause::ProcessExit)
                        {
                            return Err(err());
                        }
                    }
                    StepEnd::Stop { node: None, cause } => {
                        if *cause == StopCause::ModelStepLimit
                            && ordinal != MAX_MODEL_STEPS as usize
                        {
                            return Err(err());
                        }
                        let between = matches!(
                            cause,
                            StopCause::Cancelled
                                | StopCause::Deadline
                                | StopCause::ProcessExit
                                | StopCause::ModelStepLimit
                        );
                        match step.attempts.last() {
                            None if !between => return Err(err()),
                            Some(a) => {
                                if let AttemptEnd::Stopped { cause: actual, .. } = &a.end {
                                    let exhausted = step.attempts.len()
                                        >= t.config.limits.max_model_attempts as usize;
                                    if if retryable(a) && !exhausted {
                                        !between
                                    } else {
                                        actual != cause
                                    } {
                                        return Err(err());
                                    }
                                } else {
                                    return Err(err());
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    if let TurnState::Closed {
        outcome,
        notice,
        usage,
        ..
    } = &t.state
    {
        let (expected, expected_notice) = close::outcome(state.steps.last().ok_or_else(err)?)?;
        if *outcome != expected
            || *notice != expected_notice
            || *usage != close::usage(&state.steps)
        {
            return Err(err());
        }
    }
    Ok(())
}

pub(super) fn slots(step: &StepRecord) -> Vec<InvocationSlot> {
    match &step.state {
        StepState::Active {
            active: ActiveStep::Executing { slots, .. },
        } => slots.clone(),
        StepState::Closed { invocations, .. } => invocations
            .iter()
            .map(|i| InvocationSlot {
                binding: i.binding.clone(),
                state: SlotState::Terminal(i.terminal.clone()),
            })
            .collect(),
        _ => vec![],
    }
}

fn node(r: &Records, s: SessionId, n: NodeId) -> Result<()> {
    let err = || {
        corrupt(
            "nodes",
            &format!("{s}/{}", n.0),
            "invalid node relation or content",
        )
    };
    if !r.sessions.contains_key(&s) {
        return Err(err());
    }
    let node = r.nodes.get(&(s, n)).ok_or_else(err)?;
    if n == NodeId(0) {
        return if *node == Node::Root {
            Ok(())
        } else {
            Err(err())
        };
    }
    let parent = node.parent().ok_or_else(err)?;
    if parent >= n || !r.nodes.contains_key(&(s, parent)) {
        return Err(err());
    }
    let id = node.turn_id().ok_or_else(err)?;
    let t = r.turns.get(&id).ok_or_else(err)?;
    if t.turn_id != id || t.session_id != s {
        return Err(err());
    }
    match node {
        Node::UserMessage { message, .. } => {
            if t.user_node != n
                || message.role != MessageRole::User
                || message.provider_origin.is_some()
                || message.parts.len() != 1
                || message.parts[0].replay_state.is_some()
                || !matches!(&message.parts[0].semantic, MessageSemantic::Text {text} if *text == t.original_input && text.len() <= MAX_USER_INPUT_BYTES)
            {
                return Err(err());
            }
        }
        Node::Step {
            step_ordinal,
            content,
            ..
        } => {
            let key = StepKey {
                turn_id: id,
                ordinal: *step_ordinal,
            };
            let step = r.steps.get(&key).ok_or_else(err)?;
            let StepState::Closed { end, invocations } = &step.state else {
                return Err(err());
            };
            if step.key != key
                || end.node() != Some(n)
                || content
                    .results
                    .iter()
                    .ne(invocations.iter().map(|i| &i.terminal.result))
            {
                return Err(err());
            }
            bound_reply(t, key, &content.assistant, &slots(step)).map_err(|_| err())?;
        }
        Node::Root => return Err(err()),
    }
    Ok(())
}

pub(super) fn local(r: &Records, scope: &local::Scope) -> Result<()> {
    let s = scope.session;
    let err = || corrupt("sessions", &s.to_string(), "invalid local relations");
    let Some(session) = r.sessions.get(&s) else {
        return if r.turns.is_empty() && r.nodes.is_empty() {
            Ok(())
        } else {
            Err(err())
        };
    };
    if session.session_id != s
        || r.nodes.get(&(s, NodeId(0))) != Some(&Node::Root)
        || !r.nodes.contains_key(&(s, session.current))
    {
        return Err(err());
    }
    for (id, turn) in &r.turns {
        if turn.turn_id != *id
            || turn.session_id != s
            || turn.project_store_key != session.project_store_key
        {
            return Err(err());
        }
        if matches!(turn.state, TurnState::Running) != (session.active_turn == Some(*id)) {
            return Err(err());
        }
        config(&turn.config).map_err(|_| err())?;
        if turn.config.digest().map_err(|_| err())? != turn.config_digest {
            return Err(err());
        }
    }
    for n in &scope.nodes {
        // 查询不存在的目标由入口返回 NotFound；根/current/Turn 引用在本函数中必须存在。
        if r.nodes.contains_key(&(s, *n)) {
            node(r, s, *n)?;
        }
    }
    if let Some(id) = scope.turn
        && let Some(turn) = r.turns.get(&id)
    {
        let state = r.durable(s, id)?;
        durable(&state)?;
        node(r, s, turn.user_node)?;
        let mut parent = turn.user_node;
        for step in &state.steps {
            if let StepState::Closed { end, .. } = &step.state
                && let Some(n) = end.node()
            {
                if !matches!(r.nodes.get(&(s,n)), Some(Node::Step {parent:p,turn_id,step_ordinal,..}) if *p==parent && *turn_id==id && *step_ordinal==step.key.ordinal)
                {
                    return Err(err());
                }
                node(r, s, n)?;
                parent = n;
            }
        }
    }
    let mut ids = BTreeSet::new();
    for step in r.steps.values() {
        for slot in slots(step) {
            let id = slot.binding.invocation_id;
            if !ids.insert(id) {
                return Err(err());
            }
            let index = r.index.get(&id).ok_or_else(err)?;
            let change = r.changes.get(&index.last_change).ok_or_else(err)?;
            if index.address != slot.binding.identity.address()
                || index.last_change.0 == 0
                || index.last_change > r.last_change
                || change.sequence != index.last_change
                || change.project_store_key != session.project_store_key
                || change.row != read::operator(&slot)
            {
                return Err(err());
            }
        }
    }
    Ok(())
}

pub(super) fn records(r: &Records) -> Result<()> {
    let err = |table: &str, key: String, detail: &str| corrupt(table, &key, detail);
    let mut running_by_session: BTreeMap<SessionId, Vec<TurnId>> = BTreeMap::new();
    for t in r
        .turns
        .values()
        .filter(|t| matches!(t.state, TurnState::Running))
    {
        running_by_session
            .entry(t.session_id)
            .or_default()
            .push(t.turn_id);
    }
    for (id, s) in &r.sessions {
        if *id != s.session_id
            || r.nodes.get(&(*id, NodeId(0))) != Some(&Node::Root)
            || !r.nodes.contains_key(&(*id, s.current))
        {
            return Err(err(
                "sessions",
                id.to_string(),
                "session identity/root/current mismatch",
            ));
        }
        let mut expected = 0u64;
        for ((_, n), _) in r.nodes.range((*id, NodeId(0))..=(*id, NodeId(u64::MAX))) {
            if n.0 != expected {
                return Err(err("nodes", id.to_string(), "non-contiguous node sequence"));
            }
            expected = next_sequence(expected, "node")?;
        }
        if running_by_session
            .get(id)
            .map(Vec::as_slice)
            .unwrap_or_default()
            != s.active_turn.as_slice()
        {
            return Err(err(
                "sessions",
                id.to_string(),
                "active turn relation mismatch",
            ));
        }
    }
    for (s, n) in r.nodes.keys() {
        node(r, *s, *n)?;
    }
    let mut all_slots = BTreeMap::new();
    for (id, t) in &r.turns {
        if *id != t.turn_id {
            return Err(err("turns", id.to_string(), "key/value identity mismatch"));
        }
        let s = r
            .sessions
            .get(&t.session_id)
            .ok_or_else(|| err("turns", id.to_string(), "missing session"))?;
        if !matches!(r.nodes.get(&(t.session_id,t.user_node)),Some(Node::UserMessage {turn_id,..}) if *turn_id==*id)
        {
            return Err(err("turns", id.to_string(), "missing user node"));
        }
        let steps = r.turn_steps(*id);
        let next_node = r.next_node(t.session_id)?;
        durable(&DurableTurnState {
            session: s.clone(),
            turn: t.clone(),
            steps: steps.clone(),
            next_node,
        })?;
        let mut parent = t.user_node;
        for step in &steps {
            let slots = match &step.state {
                StepState::Active {
                    active: ActiveStep::Executing { slots, .. },
                } => slots.clone(),
                StepState::Closed { end, invocations } => {
                    if let Some(n) = end.node() {
                        if !matches!(r.nodes.get(&(t.session_id,n)),Some(Node::Step {parent:p,turn_id,step_ordinal,..}) if *p==parent&&*turn_id==*id&&*step_ordinal==step.key.ordinal)
                        {
                            return Err(err("steps", id.to_string(), "node lineage mismatch"));
                        }
                        parent = n;
                    }
                    invocations
                        .iter()
                        .map(|s| InvocationSlot {
                            binding: s.binding.clone(),
                            state: SlotState::Terminal(s.terminal.clone()),
                        })
                        .collect()
                }
                _ => Vec::new(),
            };
            for slot in slots {
                if all_slots
                    .insert(slot.binding.invocation_id, (t, slot))
                    .is_some()
                {
                    return Err(err(
                        "invocation_index",
                        id.to_string(),
                        "duplicate invocation",
                    ));
                }
            }
        }
        if matches!(t.state, TurnState::Running) && s.current != parent {
            return Err(err(
                "sessions",
                s.session_id.to_string(),
                "running turn is not at current node",
            ));
        }
    }
    for (key, step) in &r.steps {
        if *key != step.key || !r.turns.contains_key(&key.turn_id) {
            return Err(err(
                "steps",
                key.turn_id.to_string(),
                "orphan step or key mismatch",
            ));
        }
    }
    if all_slots.len() != r.index.len() {
        return Err(err(
            "invocation_index",
            "set".into(),
            "index key set differs from slots",
        ));
    }
    let mut previous: BTreeMap<InvocationId, (&OperatorInvocationState, ChangeSeq)> =
        BTreeMap::new();
    let mut seq = 0;
    for (key, change) in &r.changes {
        seq = next_sequence(seq, "change")?;
        if key.0 != seq || change.sequence != *key {
            return Err(err(
                "invocation_changes",
                key.0.to_string(),
                "non-contiguous changes",
            ));
        }
        let (turn, slot) = all_slots
            .get(&change.row.invocation_id)
            .ok_or_else(|| err("invocation_changes", key.0.to_string(), "orphan change"))?;
        if change.project_store_key != turn.project_store_key
            || change.row.binding != slot.binding.identity
        {
            return Err(err(
                "invocation_changes",
                key.0.to_string(),
                "change identity mismatch",
            ));
        }
        let legal = invocation_transition(
            previous.get(&change.row.invocation_id).map(|p| p.0),
            &change.row.state,
        );
        if !legal {
            return Err(err(
                "invocation_changes",
                key.0.to_string(),
                "illegal lifecycle transition",
            ));
        }
        previous.insert(change.row.invocation_id, (&change.row.state, *key));
    }
    if r.last_change != ChangeSeq(seq) {
        return Err(err(
            "meta",
            "last_change_seq".into(),
            "high water mark mismatch",
        ));
    }
    for (id, (_, slot)) in all_slots {
        let index = r
            .index
            .get(&id)
            .ok_or_else(|| err("invocation_index", id.to_string(), "missing index"))?;
        let row = read::operator(&slot);
        if index.address != slot.binding.identity.address()
            || previous.get(&id) != Some(&(&row.state, index.last_change))
        {
            return Err(err(
                "invocation_index",
                id.to_string(),
                "index or latest change mismatch",
            ));
        }
    }
    Ok(())
}

pub(super) fn invocation_transition(
    previous: Option<&OperatorInvocationState>,
    next: &OperatorInvocationState,
) -> bool {
    match (previous, next) {
        (None, OperatorInvocationState::Registered) => true,
        (Some(OperatorInvocationState::Registered), OperatorInvocationState::Started) => true,
        (
            Some(OperatorInvocationState::Registered),
            OperatorInvocationState::Terminal {
                outcome: lattice_conversation::ToolResultOutcome::NotExecuted,
                ..
            },
        ) => true,
        (
            Some(OperatorInvocationState::Started),
            OperatorInvocationState::Terminal { outcome, .. },
        ) => *outcome != lattice_conversation::ToolResultOutcome::NotExecuted,
        _ => false,
    }
}
