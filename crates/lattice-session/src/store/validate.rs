// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use lancet_protocol::{CellEvidenceTriple, InvocationBinding, InvocationId, ModelMessageAddress};
use lattice_conversation::{
    BoundToolCall, MessageRole, MessageSemantic, NativeMessage, RawToolCall, ToolCallState,
    validate_tool_call_batch,
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn provider_id(id: &str) -> Result<()> {
    lattice_conversation::validate_provider_id(id)
        .map_err(|error| invalid("provider", &error.to_string()))
}

pub(super) fn attempt_origin(origin: &AttemptOrigin, provider: &str) -> Result<()> {
    provider_id(&origin.provider_id)?;
    if origin.base_url.is_empty()
        || origin.base_url.len() > 8_192
        || origin
            .base_url
            .bytes()
            .any(|byte| byte < 0x20 || byte == 0x7f)
    {
        return Err(invalid("origin", "invalid endpoint"));
    }
    if origin.provider_id != provider {
        return Err(invalid("origin", "attempt provider mismatch"));
    }
    Ok(())
}

pub(super) fn reply_origin<S: ToolCallState>(
    origin: &AttemptOrigin,
    message: &NativeMessage<S>,
) -> Result<()> {
    provider_id(&origin.provider_id)?;
    if message.provider_origin.as_ref() != Some(origin) {
        return Err(invalid("origin", "reply source mismatch"));
    }
    Ok(())
}

pub(super) fn stored_config(digest: &[u8; 32], stored: &StoredTurnConfig) -> Result<()> {
    stored
        .validate()
        .map_err(|_| invalid("config", "invalid stored configuration"))?;
    if stored.digest()? != *digest {
        return Err(invalid("config", "stored configuration digest mismatch"));
    }
    let rebuilt = ToolCatalogSnapshot::new(
        stored.catalog.runtime_build_id.clone(),
        stored.catalog.entries.clone(),
    )
    .map_err(|_| invalid("catalog", "invalid catalog entries"))?;
    if rebuilt != stored.catalog
        || stored.catalog.entries.iter().any(|e| {
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

pub(super) fn user_fields(
    agent_name: &str,
    model: &str,
    provider_name: &str,
    limits: &TurnExecutionLimits,
) -> Result<()> {
    if agent_name.is_empty()
        || agent_name.len() > 128
        || model.is_empty()
        || model.len() > 4096
        || !(1..=3600).contains(&limits.timeout_secs)
    {
        return Err(invalid("config", "configuration bound exceeded"));
    }
    limits.validate()?;
    provider_id(provider_name)?;
    Ok(())
}

pub(super) fn config(config: &FrozenTurnConfig) -> Result<()> {
    config
        .validate()
        .map_err(|_| invalid("config", "invalid frozen configuration"))?;
    user_fields(
        &config.agent_name,
        &config.model,
        &config.provider,
        &config.limits,
    )?;
    let stored = config.stored();
    let digest = stored.digest()?;
    stored_config(&digest, &stored)
}

pub(super) fn assistant<S: ToolCallState>(message: &NativeMessage<S>) -> Result<()> {
    if message.role != MessageRole::Assistant {
        return Err(invalid("reply", "expected assistant role"));
    }
    lattice_conversation::validate_assistant_bounds(message)
        .map_err(|_| invalid("reply", "response byte limit exceeded"))?;
    let calls: Vec<_> = message
        .parts
        .iter()
        .filter_map(|part| match &part.semantic {
            MessageSemantic::ToolCall { call } => Some(RawToolCall {
                provider_call_id: call.provider_call_id().map(str::to_owned),
                name: call.name().into(),
                arguments: lancet_protocol::canonical_json(call.arguments()),
            }),
            _ => None,
        })
        .collect();
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

pub(super) fn terminal(
    binding: &InvocationBinding,
    result: &StoredToolResult,
    ev: &InvocationEvidence,
) -> Result<()> {
    if result.invocation_id != binding.invocation_id || result.name != binding.identity.method {
        return Err(invalid("terminal", "result identity mismatch"));
    }
    // 槽位只在 LANCET 实际返回后推进到 Terminal；未分发或无结果的槽位停在 Registered/Started。
    let InvocationEvidence::Recorded(e) = ev else {
        return Err(invalid("terminal", "missing receipt evidence"));
    };
    evidence(binding, e)?;
    match &result.payload {
        ResultPayload::NotExecuted | ResultPayload::Indeterminate(IndeterminateCause::NoResult) => {
            return Err(invalid(
                "terminal",
                "unreturned slot state must not be stored as terminal",
            ));
        }
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
        _ => {}
    }
    Ok(())
}

fn bound_reply(
    session_id: SessionId,
    origin: &AttemptOrigin,
    catalog: &ToolCatalogSnapshot,
    key: StepKey,
    message: &NativeMessage<BoundToolCall>,
    slots: &[InvocationSlot],
) -> Result<()> {
    reply_origin(origin, message)?;
    if message.tool_calls().count() != slots.len() {
        return Err(invalid("slots", "reply/slot count mismatch"));
    }
    for ((ordinal, call), slot) in message.tool_calls().zip(slots) {
        if let SlotProgress::Terminal { result, evidence } = &slot.progress {
            terminal(&slot.binding, result, evidence)?;
        }
        let entry = catalog
            .entries
            .iter()
            .find(|e| e.name == call.name)
            .ok_or_else(|| invalid("slots", "unknown tool"))?;
        let b = &slot.binding;
        let a = b.identity.address();
        if a.message
            != (ModelMessageAddress {
                session_id,
                turn_id: key.turn_id,
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

pub(super) fn stage(progress: &SlotProgress) -> InvocationStage {
    match progress {
        SlotProgress::Registered => InvocationStage::Registered,
        SlotProgress::Started => InvocationStage::Started,
        SlotProgress::Terminal { .. } => InvocationStage::Terminal,
    }
}

pub(super) fn invocation_transition(
    previous: Option<InvocationStage>,
    next: InvocationStage,
) -> bool {
    matches!(
        (previous, next),
        (None, InvocationStage::Registered)
            | (Some(InvocationStage::Registered), InvocationStage::Started)
            | (Some(InvocationStage::Started), InvocationStage::Terminal)
    )
}

pub(super) fn turn(r: &Records, s: SessionId, t: TurnId) -> Result<()> {
    let err = || corrupt("nodes", &t.to_string(), "invalid turn/step relation");
    let user_ref = r.turn_index.get(&t).ok_or_else(err)?;
    if user_ref.session_id != s || user_ref.node == NodeId(0) {
        return Err(err());
    }
    let Some(Node::UserMessage {
        parent: user_parent,
        turn_id,
        provider: provider_name,
        config_digest,
        ..
    }) = r.nodes.get(&(s, user_ref.node))
    else {
        return Err(err());
    };
    if *turn_id != t || *user_parent >= user_ref.node || !r.nodes.contains_key(&(s, *user_parent)) {
        return Err(err());
    }
    provider_id(provider_name).map_err(|_| err())?;
    let stored = r.configs.get(config_digest).ok_or_else(err)?;
    let steps = r.turn_steps(s, t)?;
    if steps.is_empty() {
        return Err(err());
    }
    let mut expected_parent = user_ref.node;
    for (ordinal, (node_id, step_node)) in steps.iter().enumerate() {
        let expected_id = next_sequence(expected_parent.0, "node").map(NodeId)?;
        let Node::Step {
            parent,
            turn_id: step_turn,
            step_ordinal,
            progress,
            attempts,
            reply,
            slots,
            ..
        } = step_node
        else {
            return Err(err());
        };
        if *node_id != expected_id
            || *parent != expected_parent
            || *step_turn != t
            || *step_ordinal != ordinal as u32
        {
            return Err(err());
        }
        expected_parent = *node_id;
        let continues = matches!(
            progress,
            StepProgress::Closed {
                end: StepEnd::Continue
            }
        );
        if continues != (ordinal + 1 < steps.len()) {
            return Err(err());
        }
        for attempt in attempts {
            attempt_origin(&attempt.origin, provider_name).map_err(|_| err())?;
        }
        if let Some(reply) = reply {
            let completed = attempts.last().ok_or_else(err)?;
            if !matches!(
                completed.progress,
                AttemptProgress::Ended {
                    end: AttemptEnd::Completed,
                    ..
                }
            ) {
                return Err(err());
            }
            bound_reply(
                s,
                &completed.origin,
                &stored.catalog,
                StepKey {
                    turn_id: t,
                    ordinal: *step_ordinal,
                },
                reply,
                slots,
            )
            .map_err(|_| err())?;
        } else if !slots.is_empty() {
            // 保留绑定时，槽位必须有可核对的调用来源。
            return Err(err());
        }
    }
    Ok(())
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
    let user_ref = r.turn_index.get(&id).ok_or_else(err)?;
    if user_ref.session_id != s {
        return Err(err());
    }
    match node {
        Node::UserMessage { .. } => {
            if user_ref.node != n {
                return Err(err());
            }
        }
        Node::Step { step_ordinal, .. } => {
            let expected = user_ref
                .node
                .0
                .checked_add(1)
                .and_then(|base| base.checked_add(u64::from(*step_ordinal)))
                .map(NodeId);
            if expected != Some(n) {
                return Err(err());
            }
        }
        Node::Root => return Err(err()),
    }
    Ok(())
}

pub(super) fn local(r: &Records, scope: &local::Scope) -> Result<()> {
    let s = scope.session;
    let err = || corrupt("sessions", &s.to_string(), "invalid local relations");
    let Some(session) = r.sessions.get(&s) else {
        return if r.turn_index.is_empty() && r.nodes.is_empty() {
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
    for (digest, stored) in &r.configs {
        stored_config(digest, stored).map_err(|_| err())?;
    }
    for n in &scope.nodes {
        if r.nodes.contains_key(&(s, *n)) {
            node(r, s, *n)?;
        }
    }
    for (&turn_id, &node_ref) in &r.turn_index {
        if node_ref.session_id != s
            || !matches!(
                r.nodes.get(&(s, node_ref.node)),
                Some(Node::UserMessage { turn_id: actual, .. }) if *actual == turn_id
            )
        {
            return Err(err());
        }
        if scope.turn == Some(turn_id)
            || r.nodes.get(&(s, session.current)).and_then(Node::turn_id) == Some(turn_id)
            || r.node_tip
                .and_then(|tip| r.nodes.get(&(s, tip)))
                .and_then(Node::turn_id)
                == Some(turn_id)
        {
            turn(r, s, turn_id)?;
        }
    }
    let mut ids = BTreeSet::new();
    for ((node_session, _), n) in &r.nodes {
        if *node_session != s {
            return Err(err());
        }
        if let Node::Step { slots, .. } = n {
            for slot in slots {
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
                    || change.invocation_id != id
                    || change.stage != stage(&slot.progress)
                {
                    return Err(err());
                }
            }
        }
    }
    Ok(())
}

pub(super) fn records(r: &Records) -> Result<()> {
    let err = |table: &str, key: String, detail: &str| corrupt(table, &key, detail);
    for (digest, stored) in &r.configs {
        stored_config(digest, stored).map_err(|_| {
            err(
                "configs",
                codec::config_key(digest),
                "invalid stored config",
            )
        })?;
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
    }
    let mut user_nodes = BTreeMap::new();
    let mut all_slots = BTreeMap::new();
    for ((s, n), value) in &r.nodes {
        node(r, *s, *n)?;
        match value {
            Node::Root => {}
            Node::UserMessage { turn_id, .. } => {
                if user_nodes
                    .insert(
                        *turn_id,
                        NodeRef {
                            session_id: *s,
                            node: *n,
                        },
                    )
                    .is_some()
                {
                    return Err(err(
                        "nodes",
                        turn_id.to_string(),
                        "duplicate user message for turn",
                    ));
                }
            }
            Node::Step { slots, .. } => {
                for slot in slots {
                    if all_slots
                        .insert(slot.binding.invocation_id, (*s, slot))
                        .is_some()
                    {
                        return Err(err(
                            "invocation_index",
                            slot.binding.invocation_id.to_string(),
                            "duplicate invocation",
                        ));
                    }
                }
            }
        }
    }
    if user_nodes != r.turn_index {
        return Err(err(
            "turn_index",
            "set".into(),
            "turn index differs from user message nodes",
        ));
    }
    for (&turn_id, &node_ref) in &r.turn_index {
        turn(r, node_ref.session_id, turn_id)?;
    }
    if all_slots.len() != r.index.len() {
        return Err(err(
            "invocation_index",
            "set".into(),
            "index key set differs from slots",
        ));
    }
    let mut previous: BTreeMap<InvocationId, (InvocationStage, ChangeSeq)> = BTreeMap::new();
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
        if !all_slots.contains_key(&change.invocation_id) {
            return Err(err(
                "invocation_changes",
                key.0.to_string(),
                "orphan change",
            ));
        }
        let legal = invocation_transition(
            previous.get(&change.invocation_id).map(|p| p.0),
            change.stage,
        );
        if !legal {
            return Err(err(
                "invocation_changes",
                key.0.to_string(),
                "illegal lifecycle transition",
            ));
        }
        previous.insert(change.invocation_id, (change.stage, *key));
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
        if index.address != slot.binding.identity.address()
            || previous.get(&id) != Some(&(stage(&slot.progress), index.last_change))
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
