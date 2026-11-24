// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use lattice_conversation::{
    BoundToolCall, MessageRole, MessageSemantic, ModelMessageAddress, NativeMessage, ProviderId,
    RawToolCall, ToolCallState, validate_tool_call_batch,
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn attempt_origin(origin: &AttemptOrigin, provider: &ProviderId) -> Result<()> {
    if &origin.provider_id != provider {
        return Err(invalid("origin", "attempt provider mismatch"));
    }
    Ok(())
}

pub(super) fn reply_origin<S: ToolCallState>(
    origin: &AttemptOrigin,
    message: &NativeMessage<S>,
) -> Result<()> {
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
    let rebuilt = ToolCatalogSnapshot::new(stored.catalog.entries.clone())
        .map_err(|_| invalid("catalog", "工具目录条目无效"))?;
    if rebuilt != stored.catalog {
        return Err(invalid("catalog", "工具目录排序不一致"));
    }
    Ok(())
}

pub(super) fn config(config: &FrozenTurnConfig) -> Result<()> {
    config
        .validate()
        .map_err(|_| invalid("config", "invalid frozen configuration"))?;
    let stored = config.stored();
    let digest = stored.digest()?;
    stored_config(&digest, &stored)
}

pub(super) fn assistant<S: ToolCallState>(message: &NativeMessage<S>) -> Result<()> {
    lattice_conversation::validate_image_placement(message)
        .map_err(|err| invalid("reply", &err.to_string()))?;
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
                arguments: lattice_conversation::canonical_json(call.arguments()),
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

pub(super) fn terminal(binding: &InvocationBinding, result: &StoredToolResult) -> Result<()> {
    if result.invocation_id != binding.invocation_id || result.name != binding.identity.method {
        return Err(invalid("terminal", "result identity mismatch"));
    }
    // 槽位只在执行器实际返回非 Deadline 结果后推进到 Terminal；未分发或无结果的槽位停在 Registered/Started。
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
            let bytes = lattice_conversation::canonical_json(value);
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
        if let SlotProgress::Terminal { result } = &slot.progress {
            terminal(&slot.binding, result)?;
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
            || b.identity.tool_identity != entry.tool_identity
            || b.identity.arguments_digest != call.arguments_digest
            || call.arguments_digest != lattice_conversation::arguments_digest(&call.arguments)
        {
            return Err(invalid("slots", "binding/reply mismatch"));
        }
    }
    Ok(())
}

pub(super) fn turn(r: &Records, s: SessionId, t: TurnId) -> Result<()> {
    let err = || corrupt("nodes", &t.to_string(), "invalid turn/step relation");
    let start_ref = r.turn_index.get(&t).ok_or_else(err)?;
    if start_ref.session_id != s || start_ref.node == NodeId(0) {
        return Err(err());
    }
    let Some(Node::Step {
        parent: step0_parent,
        turn_id,
        step_ordinal: 0,
        turn_start: Some(meta),
        ..
    }) = r.nodes.get(&(s, start_ref.node))
    else {
        return Err(err());
    };
    if *turn_id != t
        || *step0_parent >= start_ref.node
        || !r.nodes.contains_key(&(s, *step0_parent))
    {
        return Err(err());
    }
    meta.limits.validate().map_err(|_| err())?;
    let stored = r.configs.get(&meta.config_digest).ok_or_else(err)?;
    if !meta.input_nodes.is_empty() {
        let mut cursor = *step0_parent;
        for &input_id in meta.input_nodes.iter().rev() {
            if input_id != cursor {
                return Err(err());
            }
            let Some(Node::Input { parent, .. }) = r.nodes.get(&(s, input_id)) else {
                return Err(err());
            };
            if *parent >= input_id || !r.nodes.contains_key(&(s, *parent)) {
                return Err(err());
            }
            cursor = *parent;
        }
    }
    let steps = r.turn_steps(s, t)?;
    if steps.is_empty() {
        return Err(err());
    }
    let mut expected_parent = *step0_parent;
    for (ordinal, (node_id, step_node)) in steps.iter().enumerate() {
        let expected_id = start_ref
            .node
            .0
            .checked_add(ordinal as u64)
            .map(NodeId)
            .ok_or(StoreError::SequenceExhausted { kind: "node" })?;
        let Node::Step {
            parent,
            turn_id: step_turn,
            step_ordinal,
            turn_start,
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
            || (*step_ordinal == 0) != turn_start.is_some()
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
            attempt_origin(&attempt.origin, &meta.provider).map_err(|_| err())?;
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

fn user_message(s: SessionId, n: NodeId, message: &NativeMessage<BoundToolCall>) -> Result<()> {
    let semantics = message
        .parts
        .iter()
        .map(|part| part.semantic.clone())
        .collect();
    match lattice_conversation::normalize_user_message(semantics) {
        Ok(normalized) if normalized == *message => Ok(()),
        Ok(_) | Err(_) => Err(corrupt(
            "nodes",
            &format!("{s}/{}", n.0),
            "用户消息不符合规范化形态",
        )),
    }
}

fn check_message_blobs(
    r: &Records,
    s: SessionId,
    n: NodeId,
    message: &NativeMessage<BoundToolCall>,
) -> Result<()> {
    for part in &message.parts {
        if let MessageSemantic::Image { image } = &part.semantic {
            let Some(meta) = r.blobs.get(&image.sha256) else {
                return Err(corrupt(
                    "nodes",
                    &format!("{s}/{}", n.0),
                    "节点引用的图片在 blobs 表中不存在",
                ));
            };
            if meta.media_type != image.media_type || meta.byte_length != image.byte_length {
                return Err(corrupt(
                    "nodes",
                    &format!("{s}/{}", n.0),
                    "节点引用的图片元数据与 blobs 表不一致",
                ));
            }
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
    match node {
        Node::Root => return Err(err()),
        Node::Input { input, .. } => {
            user_message(s, n, input.message())?;
        }
        Node::ContextOp { op, .. } => {
            if op.edits().is_empty() {
                return Err(err());
            }
            for edit in op.edits() {
                if let ContextEdit::ReplaceInput { message, .. } = edit {
                    user_message(s, n, message.as_message())?;
                }
            }
        }
        Node::Step {
            turn_id,
            step_ordinal,
            turn_start,
            ..
        } => {
            if (*step_ordinal == 0) != turn_start.is_some() {
                return Err(err());
            }
            let start_ref = r.turn_index.get(turn_id).ok_or_else(err)?;
            if start_ref.session_id != s {
                return Err(err());
            }
            let expected = start_ref
                .node
                .0
                .checked_add(u64::from(*step_ordinal))
                .map(NodeId);
            if expected != Some(n) {
                return Err(err());
            }
            if *step_ordinal > 0 {
                if parent != NodeId(n.0 - 1) {
                    return Err(err());
                }
                let Some(Node::Step {
                    turn_id: prev_turn,
                    step_ordinal: prev_ordinal,
                    progress:
                        StepProgress::Closed {
                            end: StepEnd::Continue,
                        },
                    ..
                }) = r.nodes.get(&(s, parent))
                else {
                    return Err(err());
                };
                if *prev_turn != *turn_id || *prev_ordinal != *step_ordinal - 1 {
                    return Err(err());
                }
            }
        }
    }
    Ok(())
}

pub(super) fn local(r: &Records, scope: &local::Scope) -> Result<()> {
    let s = scope.session;
    let err = || corrupt("sessions", &s.to_string(), "invalid local relations");
    let Some(session) = r.sessions.get(&s) else {
        return if r.turn_index.is_empty() && r.nodes.is_empty() && r.session_settings.is_empty() {
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
    session_settings(r, s)?;
    for n in &scope.nodes {
        if r.nodes.contains_key(&(s, *n)) {
            node(r, s, *n)?;
        }
    }
    for (&turn_id, &node_ref) in &r.turn_index {
        if node_ref.session_id != s
            || !matches!(
                r.nodes.get(&(s, node_ref.node)),
                Some(Node::Step {
                    turn_id: actual,
                    step_ordinal: 0,
                    turn_start: Some(_),
                    ..
                }) if *actual == turn_id
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
    for ((node_session, node_id), n) in &r.nodes {
        if *node_session != s {
            return Err(err());
        }
        match n {
            Node::Input { input, .. } => user_message(s, *node_id, input.message())?,
            Node::ContextOp { op, .. } => {
                if op.edits().is_empty() {
                    return Err(err());
                }
                for edit in op.edits() {
                    if let ContextEdit::ReplaceInput { message, .. } = edit {
                        user_message(s, *node_id, message.as_message())?;
                    }
                }
            }
            Node::Step {
                step_ordinal,
                turn_start,
                slots,
                ..
            } => {
                if (*step_ordinal == 0) != turn_start.is_some() {
                    return Err(err());
                }
                for slot in slots {
                    let id = slot.binding.invocation_id;
                    if !ids.insert(id) {
                        return Err(err());
                    }
                    let index = r.index.get(&id).ok_or_else(err)?;
                    if index.address != slot.binding.identity.address() {
                        return Err(err());
                    }
                }
            }
            Node::Root => {}
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
        session_settings(r, *id)?;
        let mut expected = 0u64;
        for ((_, n), _) in r.nodes.range((*id, NodeId(0))..=(*id, NodeId(u64::MAX))) {
            if n.0 != expected {
                return Err(err("nodes", id.to_string(), "non-contiguous node sequence"));
            }
            expected = next_sequence(expected, "node")?;
        }
    }
    if r.session_settings
        .keys()
        .any(|id| !r.sessions.contains_key(id))
    {
        return Err(err(
            "session_settings",
            "set".into(),
            "orphan session settings",
        ));
    }
    let mut start_nodes = BTreeMap::new();
    let mut all_slots = BTreeMap::new();
    for ((s, n), value) in &r.nodes {
        node(r, *s, *n)?;
        match value {
            Node::Root => {}
            Node::Input { input, .. } => {
                check_message_blobs(r, *s, *n, input.message())?;
            }
            Node::ContextOp { op, .. } => {
                for edit in op.edits() {
                    if let ContextEdit::ReplaceInput { message, .. } = edit {
                        check_message_blobs(r, *s, *n, message.as_message())?;
                    }
                }
            }
            Node::Step {
                turn_id,
                step_ordinal,
                turn_start,
                slots,
                ..
            } => {
                if *step_ordinal == 0 {
                    if turn_start.is_none() {
                        return Err(err(
                            "nodes",
                            format!("{s}/{}", n.0),
                            "Step 0 缺少 turn_start 元数据",
                        ));
                    }
                    if start_nodes
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
                            "duplicate start step for turn",
                        ));
                    }
                } else if turn_start.is_some() {
                    return Err(err(
                        "nodes",
                        format!("{s}/{}", n.0),
                        "非 Step 0 节点不能携带 turn_start 元数据",
                    ));
                }
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
    if start_nodes != r.turn_index {
        return Err(err(
            "turn_index",
            "set".into(),
            "turn index differs from step 0 nodes",
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
    for (id, (_, slot)) in all_slots {
        let index = r
            .index
            .get(&id)
            .ok_or_else(|| err("invocation_index", id.to_string(), "missing index"))?;
        if index.address != slot.binding.identity.address() {
            return Err(err(
                "invocation_index",
                id.to_string(),
                "index address mismatch",
            ));
        }
    }
    enum DfsAction {
        Enter(NodeId),
        Exit(lattice_context::ContextUndo),
    }

    // 按增量可回溯游标与显式栈迭代 DFS 单遍遍历每棵会话树，线性时间覆盖全树全部 ContextOp，
    // 并核验所有独立触发 Step 0（input_nodes 为空）的父路径有效模型上下文非空。
    for &s in r.sessions.keys() {
        let mut children: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
        for ((_, n), value) in r.nodes.range((s, NodeId(0))..=(s, NodeId(u64::MAX))) {
            if let Some(parent) = value.parent() {
                children.entry(parent).or_default().push(*n);
            }
        }

        let mut cursor = lattice_context::ContextCursor::new();
        let mut stack = vec![DfsAction::Enter(NodeId(0))];
        while let Some(action) = stack.pop() {
            match action {
                DfsAction::Enter(n) => {
                    let node = r.nodes.get(&(s, n)).ok_or_else(|| {
                        err("nodes", format!("{s}/{}", n.0), "missing session node")
                    })?;
                    if let Node::Step {
                        step_ordinal: 0,
                        turn_start: Some(meta),
                        ..
                    } = node
                        && meta.input_nodes.is_empty()
                        && !cursor.has_projected_items()
                    {
                        return Err(err(
                            "nodes",
                            format!("{s}/{}", n.0),
                            "独立触发的 Step 0 父路径有效模型上下文为空",
                        ));
                    }
                    let record = match node {
                        Node::Root => None,
                        Node::Input { input, .. } => {
                            Some(lattice_context::PathRecord::Input { id: n, input })
                        }
                        Node::ContextOp { op, .. } => {
                            Some(lattice_context::PathRecord::Op { id: n, op })
                        }
                        Node::Step { .. } => node
                            .step_content()
                            .map(|content| lattice_context::PathRecord::Step { id: n, content }),
                    };
                    if let Some(record) = record {
                        let undo = cursor.advance(record).map_err(|e| {
                            err(
                                "nodes",
                                format!("{s}/{}", n.0),
                                &format!("无效的模型上下文路径：{e}"),
                            )
                        })?;
                        // LIFO：先压入当前节点的 Exit，再逆序压入全部子节点 Enter。
                        stack.push(DfsAction::Exit(undo));
                    }
                    if let Some(kids) = children.get(&n) {
                        for &child in kids.iter().rev() {
                            stack.push(DfsAction::Enter(child));
                        }
                    }
                }
                DfsAction::Exit(undo) => {
                    cursor.rollback(undo);
                }
            }
        }
    }
    Ok(())
}

fn session_settings(r: &Records, session: SessionId) -> Result<()> {
    let error = || {
        corrupt(
            "session_settings",
            &session.to_string(),
            "invalid session settings",
        )
    };
    let settings = r.session_settings.get(&session).ok_or_else(error)?;
    settings.validate().map_err(|_| error())?;
    Ok(())
}
