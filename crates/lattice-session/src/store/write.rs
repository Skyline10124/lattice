// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::local::Selection;
use super::*;
use lattice_conversation::{
    InvocationId, MessagePart, MessageRole, MessageSemantic, ModelAttemptKey, ModelMessageAddress,
    NativeMessage, ToolCallAddress,
};
use redb::ReadableTable;
use std::collections::BTreeSet;

impl Records {
    pub(super) fn max_node(&self, s: SessionId) -> Option<NodeId> {
        self.nodes
            .range((s, NodeId(0))..=(s, NodeId(u64::MAX)))
            .next_back()
            .map(|((_, n), _)| *n)
            .into_iter()
            .chain(self.node_tip)
            .max()
    }

    pub(super) fn next_node(&self, s: SessionId) -> Result<NodeId> {
        self.max_node(s)
            .map_or(Ok(NodeId(0)), |n| next_sequence(n.0, "node").map(NodeId))
    }

    pub(super) fn unclosed_turn(&self, s: SessionId) -> Option<TurnId> {
        let tip = self.max_node(s)?;
        if tip == NodeId(0) {
            return None;
        }
        match self.nodes.get(&(s, tip))? {
            Node::Step {
                turn_id, progress, ..
            } if !progress.is_closed() => Some(*turn_id),
            _ => None,
        }
    }

    pub(super) fn turn_steps(&self, s: SessionId, t: TurnId) -> Result<Vec<(NodeId, Node)>> {
        let user_ref = self
            .turn_index
            .get(&t)
            .filter(|r| r.session_id == s)
            .ok_or_else(|| missing("turn", t))?;
        let start = user_ref.node.0.saturating_add(1);
        let mut steps = Vec::new();
        for ((_, n), node) in self.nodes.range((s, NodeId(start))..=(s, NodeId(u64::MAX))) {
            if matches!(node, Node::UserMessage { .. } | Node::Root) {
                break;
            }
            steps.push((*n, node.clone()));
        }
        Ok(steps)
    }

    pub(super) fn turn_config(&self, s: SessionId, t: TurnId) -> Result<FrozenTurnConfig> {
        let user_ref = self
            .turn_index
            .get(&t)
            .filter(|r| r.session_id == s)
            .ok_or_else(|| missing("turn", t))?;
        let Some(Node::UserMessage {
            agent_name,
            model,
            provider,
            effort,
            limits,
            config_digest,
            ..
        }) = self.nodes.get(&(s, user_ref.node))
        else {
            return Err(corrupt(
                "nodes",
                &format!("{s}/{}", user_ref.node.0),
                "missing turn user node",
            ));
        };
        let stored = self.configs.get(config_digest).ok_or_else(|| {
            corrupt(
                "configs",
                &codec::config_key(config_digest),
                "missing referenced config",
            )
        })?;
        Ok(FrozenTurnConfig {
            agent_name: agent_name.clone(),
            model: model.clone(),
            provider: provider.clone(),
            effort: *effort,
            system_prompt: stored.system_prompt.clone(),
            catalog: stored.catalog.clone(),
            limits: *limits,
        })
    }

    pub(super) fn turn_input(&self, s: SessionId, t: TurnId) -> Result<String> {
        let user_ref = self
            .turn_index
            .get(&t)
            .filter(|r| r.session_id == s)
            .ok_or_else(|| missing("turn", t))?;
        let Some(Node::UserMessage { message, .. }) = self.nodes.get(&(s, user_ref.node)) else {
            return Err(corrupt(
                "nodes",
                &format!("{s}/{}", user_ref.node.0),
                "missing turn user node",
            ));
        };
        let Some(MessagePart {
            semantic: MessageSemantic::Text { text },
            ..
        }) = message.parts.first()
        else {
            return Err(corrupt(
                "nodes",
                &format!("{s}/{}", user_ref.node.0),
                "invalid user message parts",
            ));
        };
        Ok(text.clone())
    }

    fn active_step(&self, s: SessionId, t: TurnId) -> Result<(NodeId, FrozenTurnConfig, Node)> {
        let session = self.sessions.get(&s).ok_or_else(|| missing("session", s))?;
        let config = self.turn_config(s, t)?;
        let steps = self.turn_steps(s, t)?;
        let (node_id, step) = steps
            .last()
            .ok_or_else(|| transition(t, "turn has no steps"))?
            .clone();
        let Node::Step { progress, .. } = &step else {
            return Err(transition(t, "turn tip is not a step"));
        };
        if progress.is_closed() || session.current != node_id || self.max_node(s) != Some(node_id) {
            return Err(transition(t, "step is not the mutable session tip"));
        }
        Ok((node_id, config, step))
    }

    fn requesting(
        &self,
        s: SessionId,
        key: ModelAttemptKey,
    ) -> Result<(NodeId, FrozenTurnConfig, Node)> {
        let (node_id, config, step) = self.active_step(s, key.turn_id)?;
        let Node::Step {
            step_ordinal,
            progress,
            attempts,
            ..
        } = &step
        else {
            unreachable!()
        };
        let sent_matches = attempts.last().is_some_and(|a| {
            attempts.len() == key.attempt_ordinal as usize + 1
                && matches!(a.progress, AttemptProgress::Sent)
        });
        if *step_ordinal != key.model_step_ordinal
            || *progress != StepProgress::Requesting
            || !sent_matches
        {
            return Err(transition(
                key.turn_id,
                "request key does not match active sent attempt",
            ));
        }
        Ok((node_id, config, step))
    }

    fn executing(&self, s: SessionId, key: StepKey) -> Result<(NodeId, FrozenTurnConfig, Node)> {
        let (node_id, config, step) = self.active_step(s, key.turn_id)?;
        let Node::Step {
            step_ordinal,
            progress,
            ..
        } = &step
        else {
            unreachable!()
        };
        if *step_ordinal != key.ordinal || *progress != StepProgress::Executing {
            return Err(transition(key.turn_id, "step is not executing"));
        }
        Ok((node_id, config, step))
    }

    fn closed_step(
        &self,
        s: SessionId,
        t: TurnId,
        step_node: NodeId,
        next_node: Option<NodeRef>,
    ) -> Result<ClosedStep> {
        let detail = self.turn_detail(s, t)?;
        let step = detail
            .steps
            .into_iter()
            .find(|step| step.node == step_node)
            .ok_or_else(|| {
                corrupt(
                    "nodes",
                    &format!("{s}/{}", step_node.0),
                    "closed step missing",
                )
            })?;
        Ok(ClosedStep {
            step,
            next_node,
            summary: detail.summary,
        })
    }

    fn touch(&mut self, s: SessionId, now: Timestamp) -> Result<()> {
        self.sessions
            .get_mut(&s)
            .ok_or_else(|| missing("session", s))?
            .updated_at = now;
        Ok(())
    }
}

impl SessionStore {
    fn write<T>(
        &self,
        lock: &SessionExecutionLock,
        selection: Selection,
        f: impl FnOnce(&mut Records, &redb::WriteTransaction) -> Result<T>,
    ) -> Result<T> {
        self.check_lock(lock, lock.session_id)?;
        // 必须在 begin_write 前取得健康锁；commit 释放 redb 写者后仍不能让等待者抢跑。
        let disabled = self.execution_health()?;
        let mut tx = self.db.begin_write()?;
        tx.set_durability(redb::Durability::Immediate)?;
        let (before, scope) =
            codec::attach_path(&self.path, local::write(&tx, lock.session_id, selection))?;
        let mut after = before.clone();
        let value = f(&mut after, &tx)?;
        validate::local(&after, &scope)?;
        if before == after {
            return Ok(value);
        }
        codec::attach_path(&self.path, codec::persist(&tx, &before, &after))?;
        self.commit_write(disabled, tx, value)
    }

    fn commit_write<T>(
        &self,
        mut disabled: std::sync::MutexGuard<'_, bool>,
        tx: redb::WriteTransaction,
        value: T,
    ) -> Result<T> {
        #[cfg(test)]
        write_probe(WriteStage::BeforeCommit);
        #[cfg(any(test, feature = "test-utils", feature = "test-crash-barriers"))]
        let injected = if hit(FaultPoint::Poisoned) {
            Some(redb::CommitError::TransactionPoisoned)
        } else if hit(FaultPoint::UnknownBefore) {
            Some(redb::CommitError::Storage(redb::StorageError::Io(
                injected_io(),
            )))
        } else {
            None
        };
        #[cfg(any(test, feature = "test-utils", feature = "test-crash-barriers"))]
        let committed = if let Some(error) = injected {
            drop(tx);
            Err(error)
        } else {
            tx.commit()
        };
        #[cfg(not(any(test, feature = "test-utils", feature = "test-crash-barriers")))]
        let committed = tx.commit();
        #[cfg(any(test, feature = "test-utils", feature = "test-crash-barriers"))]
        let committed = if committed.is_ok() && hit(FaultPoint::UnknownAfter) {
            Err(redb::CommitError::Storage(redb::StorageError::Io(
                injected_io(),
            )))
        } else {
            committed
        };
        #[cfg(test)]
        write_probe(WriteStage::CommitReturned);
        match committed {
            Ok(()) => Ok(value),
            Err(source) => {
                *disabled = true;
                Err(commit_error(source))
            }
        }
    }

    pub fn create_session(
        &self,
        lock: &SessionExecutionLock,
        project_path: ProjectPath,
        settings: SessionSettings,
        now: Timestamp,
    ) -> Result<SessionRecord> {
        self.check_lock(lock, lock.session_id)?;
        settings.validate()?;
        self.write(lock, Selection::Session, |r, _| {
            let s = lock.session_id;
            if r.sessions.contains_key(&s) {
                return Err(StoreError::SessionCollision { session_id: s });
            }
            let record = SessionRecord {
                session_id: s,
                project_path,
                current: NodeId(0),
                created_at: now,
                updated_at: now,
            };
            r.sessions.insert(s, record.clone());
            r.nodes.insert((s, NodeId(0)), Node::Root);
            r.session_settings.insert(s, settings);
            Ok(record)
        })
    }

    pub fn update_settings(
        &self,
        lock: &SessionExecutionLock,
        project_path: &ProjectPath,
        settings: SessionSettings,
        now: Timestamp,
    ) -> Result<SessionRecord> {
        self.check_lock(lock, lock.session_id)?;
        settings.validate()?;
        self.write(lock, Selection::Session, |r, _| {
            let s = lock.session_id;
            let session = r
                .sessions
                .get_mut(&s)
                .ok_or_else(|| missing("session", s))?;
            check_binding(session, project_path)?;
            session.updated_at = now;
            let record = session.clone();
            r.session_settings.insert(s, settings);
            Ok(record)
        })
    }

    pub fn delete_draft(&self, lock: &SessionExecutionLock, cutoff: Timestamp) -> Result<bool> {
        let s = lock.session_id;
        self.check_lock(lock, s)?;
        let disabled = self.execution_health()?;
        let mut tx = self.db.begin_write()?;
        tx.set_durability(redb::Durability::Immediate)?;
        let (before, scope) =
            codec::attach_path(&self.path, local::write(&tx, s, Selection::Session))?;
        validate::local(&before, &scope)?;
        let Some(session) = before.sessions.get(&s) else {
            return Ok(false);
        };
        // 按节点范围的最大键判断整棵树只有根节点，不看局部加载条数或 current == 0。
        if before.node_tip != Some(NodeId(0)) || session.updated_at >= cutoff {
            return Ok(false);
        }
        let mut after = before;
        after.sessions.remove(&s);
        after.nodes.remove(&(s, NodeId(0)));
        after.session_settings.remove(&s);
        after.node_tip = None;
        validate::local(&after, &scope)?;
        codec::attach_path(&self.path, codec::delete_draft(&tx, s))?;
        self.commit_write(disabled, tx, true)
    }

    pub fn accept_turn(
        &self,
        lock: &SessionExecutionLock,
        input: AcceptTurn,
    ) -> Result<AcceptedTurn> {
        self.check_lock(lock, input.session_id)?;
        self.write(lock, Selection::Accept(input.turn_id), |r, _| {
            if let Some(&node_ref) = r.turn_index.get(&input.turn_id) {
                let s = node_ref.session_id;
                let existing_session = r
                    .sessions
                    .get(&s)
                    .ok_or_else(|| corrupt("sessions", &s.to_string(), "missing turn session"))?;
                let existing_input = r.turn_input(s, input.turn_id)?;
                if s != input.session_id
                    || existing_input != input.input
                    || existing_session.project_path != input.project_path
                {
                    return Err(StoreError::Conflict {
                        turn_id: input.turn_id,
                    });
                }
                return Ok(AcceptedTurn::Existing(r.turn_detail(s, input.turn_id)?));
            }
            if input.input.len() > MAX_USER_INPUT_BYTES {
                return Err(invalid("input", "user input exceeds byte bound"));
            }
            validate::config(&input.config)?;
            let s = input.session_id;
            let session = r.sessions.get(&s).ok_or_else(|| missing("session", s))?;
            check_binding(session, &input.project_path)?;
            let current_settings = r.session_settings.get(&s).ok_or_else(|| {
                corrupt(
                    "session_settings",
                    &s.to_string(),
                    "missing session settings",
                )
            })?;
            current_settings.validate().map_err(|_| {
                corrupt(
                    "session_settings",
                    &s.to_string(),
                    "invalid session settings",
                )
            })?;
            if current_settings.version()? != input.settings_version {
                return Err(StoreError::SettingsChanged { session_id: s });
            }
            let parent = session.current;
            let user_node = r.next_node(s)?;
            let step_node = next_sequence(user_node.0, "node").map(NodeId)?;
            let stored = input.config.stored();
            let config_digest = stored
                .digest()
                .map_err(|_| invalid("config", "cannot digest frozen configuration"))?;
            r.configs.entry(config_digest).or_insert(stored);
            r.nodes.insert(
                (s, user_node),
                Node::UserMessage {
                    parent,
                    turn_id: input.turn_id,
                    accepted_at: input.accepted_at,
                    agent_name: input.config.agent_name,
                    model: input.config.model,
                    provider: input.config.provider,
                    effort: input.config.effort,
                    limits: input.config.limits,
                    config_digest,
                    message: NativeMessage::new(
                        MessageRole::User,
                        None,
                        vec![MessagePart::semantic_only(MessageSemantic::Text {
                            text: input.input,
                        })],
                    ),
                },
            );
            r.turn_index.insert(
                input.turn_id,
                NodeRef {
                    session_id: s,
                    node: user_node,
                },
            );
            r.nodes.insert(
                (s, step_node),
                Node::Step {
                    parent: user_node,
                    turn_id: input.turn_id,
                    step_ordinal: 0,
                    progress: StepProgress::Reserved,
                    attempts: Vec::new(),
                    reply: None,
                    slots: Vec::new(),
                    updated_at: input.accepted_at,
                },
            );
            let session = r.sessions.get_mut(&s).expect("session exists");
            session.current = step_node;
            session.updated_at = input.accepted_at;
            Ok(AcceptedTurn::Accepted(r.turn_detail(s, input.turn_id)?))
        })
    }

    pub fn start_request(
        &self,
        lock: &SessionExecutionLock,
        key: ModelAttemptKey,
        origin: AttemptOrigin,
        now: Timestamp,
    ) -> Result<()> {
        self.write(lock, Selection::Turn(key.turn_id), |r, _| {
            let (node_id, config, mut step) = r.active_step(lock.session_id, key.turn_id)?;
            validate::attempt_origin(&origin, &config.provider)?;
            if key.model_step_ordinal >= MAX_MODEL_STEPS
                || key.attempt_ordinal >= config.limits.max_model_attempts
            {
                return Err(transition(key.turn_id, "request budget exhausted"));
            }
            let Node::Step {
                step_ordinal,
                progress,
                attempts,
                updated_at,
                ..
            } = &mut step
            else {
                unreachable!()
            };
            let ready = match progress {
                StepProgress::Reserved => attempts.is_empty(),
                StepProgress::Requesting => {
                    attempts.last().is_some_and(|a| a.retry_delay_ms.is_some())
                }
                _ => false,
            };
            if *step_ordinal != key.model_step_ordinal
                || key.attempt_ordinal as usize != attempts.len()
                || !ready
            {
                return Err(transition(key.turn_id, "request sequence mismatch"));
            }
            *progress = StepProgress::Requesting;
            attempts.push(AttemptRecord {
                origin,
                usage: None,
                usage_kind: lattice_conversation::UsageCompleteness::Partial,
                cost: None,
                progress: AttemptProgress::Sent,
                retry_delay_ms: None,
            });
            *updated_at = now;
            r.nodes.insert((lock.session_id, node_id), step);
            r.touch(lock.session_id, now)
        })
    }

    pub fn stop_attempt(
        &self,
        lock: &SessionExecutionLock,
        input: AttemptStopped,
        now: Timestamp,
    ) -> Result<AttemptStopResult> {
        validate::diagnostic(&input.cause, &input.diagnostic)?;
        let retryable_cause = matches!(input.cause, StopCause::ProviderError { retryable: true });
        if !retryable_cause && input.retry_delay_ms.is_some() {
            return Err(invalid(
                "retry_delay_ms",
                "non-retryable stop cause must not carry retry delay",
            ));
        }
        self.write(lock, Selection::Turn(input.key.turn_id), |r, _| {
            let (node_id, config, mut step) = r.requesting(lock.session_id, input.key)?;
            let Node::Step {
                progress,
                attempts,
                updated_at,
                ..
            } = &mut step
            else {
                unreachable!()
            };
            let retry =
                retryable_cause && attempts.len() < config.limits.max_model_attempts as usize;
            if retry && input.retry_delay_ms.is_none() {
                return Err(invalid(
                    "retry_delay_ms",
                    "retryable provider error requires retry delay",
                ));
            }
            let last = attempts.last_mut().expect("sent attempt exists");
            if input.origin != last.origin {
                return Err(invalid("origin", "attempt source mismatch"));
            }
            (last.usage, last.cost) = close::reported_usage(input.usage, input.cost);
            last.usage_kind = input.usage_kind;
            last.progress = AttemptProgress::Ended {
                end: AttemptEnd::Stopped {
                    cause: input.cause.clone(),
                },
                diagnostic: input.diagnostic,
            };
            last.retry_delay_ms = if retry { input.retry_delay_ms } else { None };
            *updated_at = now;
            if retry {
                let next_attempt = attempts.len() as u32;
                r.nodes.insert((lock.session_id, node_id), step);
                r.touch(lock.session_id, now)?;
                Ok(AttemptStopResult::RetryReady { next_attempt })
            } else {
                *progress = StepProgress::Closed {
                    end: StepEnd::Stop { cause: input.cause },
                };
                r.nodes.insert((lock.session_id, node_id), step);
                r.touch(lock.session_id, now)?;
                Ok(AttemptStopResult::Closed(Box::new(r.closed_step(
                    lock.session_id,
                    input.key.turn_id,
                    node_id,
                    None,
                )?)))
            }
        })
    }

    pub fn register_calls(
        &self,
        lock: &SessionExecutionLock,
        reply: CompleteReply,
        candidates: &[InvocationId],
        now: Timestamp,
    ) -> Result<RegisteredCalls> {
        validate::assistant(&reply.message)?;
        let count = reply.message.tool_calls().count();
        if count == 0 || count != candidates.len() {
            return Err(invalid(
                "candidates",
                "candidate count does not match nonempty batch",
            ));
        }
        self.write(lock, Selection::Turn(reply.key.turn_id), |r, tx| {
            let (node_id, config, mut step) = r.requesting(lock.session_id, reply.key)?;
            if let Node::Step { attempts, .. } = &step
                && attempts.last().expect("sent attempt exists").origin != reply.origin
            {
                return Err(invalid("origin", "reply source mismatch"));
            }
            validate::reply_origin(&reply.origin, &reply.message)?;
            let mut seen = BTreeSet::new();
            let index = tx.open_table(codec::INDEX).map_err(codec::table_error)?;
            let mut collisions = Vec::new();
            for id in candidates {
                if !seen.insert(*id) || index.get(id.to_string().as_str())?.is_some() {
                    collisions.push(*id);
                }
            }
            if !collisions.is_empty() {
                return Err(StoreError::InvocationCollision { ids: collisions });
            }
            let mut bindings = Vec::new();
            let mut parts = Vec::new();
            let completed = close::completed(&reply);
            for (ordinal, part) in reply.message.parts.into_iter().enumerate() {
                let semantic = match part.semantic {
                    MessageSemantic::Text { text } => MessageSemantic::Text { text },
                    MessageSemantic::Reasoning { text } => MessageSemantic::Reasoning { text },
                    MessageSemantic::ToolCall { call } => {
                        let entry = config
                            .catalog
                            .entries
                            .iter()
                            .find(|e| e.name == call.name)
                            .ok_or_else(|| invalid("reply", "tool not in frozen catalog"))?;
                        let part_ordinal = u32::try_from(ordinal)
                            .map_err(|_| StoreError::SequenceExhausted { kind: "part" })?;
                        let address = ToolCallAddress {
                            message: ModelMessageAddress {
                                session_id: lock.session_id,
                                turn_id: reply.key.turn_id,
                                model_step_ordinal: reply.key.model_step_ordinal,
                            },
                            part_ordinal,
                        };
                        let bound = call.bind(candidates[bindings.len()], address);
                        bindings.push(InvocationBinding {
                            invocation_id: bound.invocation_id,
                            identity: BindingIdentity {
                                session_id: lock.session_id,
                                turn_id: reply.key.turn_id,
                                model_step_ordinal: reply.key.model_step_ordinal,
                                part_ordinal,
                                method: bound.name.clone(),
                                tool_identity: entry.tool_identity.clone(),
                                arguments_digest: bound.arguments_digest,
                            },
                        });
                        MessageSemantic::ToolCall { call: bound }
                    }
                };
                parts.push(MessagePart {
                    semantic,
                    replay_state: part.replay_state,
                });
            }
            let bound =
                NativeMessage::new(reply.message.role, reply.message.provider_origin, parts);
            let new_slots: Vec<_> = bindings
                .iter()
                .map(|b| InvocationSlot {
                    binding: b.clone(),
                    progress: SlotProgress::Registered,
                })
                .collect();
            for slot in &new_slots {
                r.index.insert(
                    slot.binding.invocation_id,
                    InvocationIndexEntry {
                        address: slot.binding.identity.address(),
                    },
                );
            }
            let Node::Step {
                progress,
                attempts,
                reply: step_reply,
                slots,
                updated_at,
                ..
            } = &mut step
            else {
                unreachable!()
            };
            *attempts.last_mut().expect("sent attempt exists") = completed;
            *step_reply = Some(bound.clone());
            *slots = new_slots;
            *progress = StepProgress::Executing;
            *updated_at = now;
            r.nodes.insert((lock.session_id, node_id), step);
            r.touch(lock.session_id, now)?;
            Ok(RegisteredCalls {
                key: StepKey {
                    turn_id: reply.key.turn_id,
                    ordinal: reply.key.model_step_ordinal,
                },
                reply: bound,
                bindings,
            })
        })
    }

    pub fn start_invocation(
        &self,
        lock: &SessionExecutionLock,
        binding: &InvocationBinding,
        now: Timestamp,
    ) -> Result<StartedInvocation> {
        self.check_lock(lock, binding.identity.session_id)?;
        self.write(lock, Selection::Turn(binding.identity.turn_id), |r, _| {
            let key = StepKey {
                turn_id: binding.identity.turn_id,
                ordinal: binding.identity.model_step_ordinal,
            };
            let (node_id, _, mut step) = r.executing(lock.session_id, key)?;
            let Node::Step {
                slots, updated_at, ..
            } = &mut step
            else {
                unreachable!()
            };
            let position = slots
                .iter()
                .position(|s| !matches!(s.progress, SlotProgress::Terminal { .. }))
                .ok_or_else(|| transition(key.turn_id, "no unfinished slot"))?;
            if slots[..position].iter().any(|s| {
                matches!(
                    &s.progress,
                    SlotProgress::Terminal { result } if close::stops_batch(&result.payload)
                )
            }) || slots[position].binding != *binding
                || !matches!(slots[position].progress, SlotProgress::Registered)
            {
                return Err(transition(key.turn_id, "invocation cannot start"));
            }
            slots[position].progress = SlotProgress::Started;
            *updated_at = now;
            r.nodes.insert((lock.session_id, node_id), step);
            r.touch(lock.session_id, now)?;
            Ok(StartedInvocation {
                binding: binding.clone(),
            })
        })
    }

    pub fn finish_invocation(
        &self,
        lock: &SessionExecutionLock,
        input: TerminalInput,
        now: Timestamp,
    ) -> Result<()> {
        self.write(lock, Selection::Invocation(input.invocation_id), |r, _| {
            let address = r
                .index
                .get(&input.invocation_id)
                .ok_or_else(|| missing("invocation", input.invocation_id))?
                .address;
            let key = StepKey {
                turn_id: address.message.turn_id,
                ordinal: address.message.model_step_ordinal,
            };
            let (node_id, _, mut step) = r.executing(lock.session_id, key)?;
            let Node::Step {
                slots, updated_at, ..
            } = &mut step
            else {
                unreachable!()
            };
            let slot = slots
                .iter_mut()
                .find(|s| s.binding.invocation_id == input.invocation_id)
                .ok_or_else(|| transition(key.turn_id, "invocation not in step"))?;
            if !matches!(slot.progress, SlotProgress::Started)
                || matches!(
                    input.terminal.result.payload,
                    ResultPayload::NotExecuted
                        | ResultPayload::Indeterminate(IndeterminateCause::NoResult)
                )
            {
                return Err(transition(
                    key.turn_id,
                    "terminal requires a started invocation",
                ));
            }
            validate::terminal(&slot.binding, &input.terminal.result)?;
            slot.progress = SlotProgress::Terminal {
                result: input.terminal.result,
            };
            *updated_at = now;
            r.nodes.insert((lock.session_id, node_id), step);
            r.touch(lock.session_id, now)
        })
    }

    pub fn close_step(
        &self,
        lock: &SessionExecutionLock,
        input: CloseStep,
        now: Timestamp,
    ) -> Result<ClosedStep> {
        let turn = match &input {
            CloseStep::Finish(reply) => reply.key.turn_id,
            CloseStep::Continue { key } | CloseStep::StopExecuting { key, .. } => key.turn_id,
            CloseStep::StopBetweenRequests { turn_id, .. } => *turn_id,
        };
        self.write(lock, Selection::Turn(turn), |r, _| match input {
            CloseStep::Finish(reply) => {
                validate::assistant(&reply.message)?;
                if reply.message.tool_calls().next().is_some() {
                    return Err(invalid("reply", "finish must have no calls"));
                }
                let (node_id, _, mut step) = r.requesting(lock.session_id, reply.key)?;
                if let Node::Step { attempts, .. } = &step
                    && attempts.last().expect("sent attempt exists").origin != reply.origin
                {
                    return Err(invalid("origin", "reply source mismatch"));
                }
                validate::reply_origin(&reply.origin, &reply.message)?;
                let completed = close::completed(&reply);
                let message = reply
                    .message
                    .bind_zero_tool()
                    .map_err(|_| invalid("reply", "finish contains calls"))?;
                let Node::Step {
                    progress,
                    attempts,
                    reply: step_reply,
                    updated_at,
                    ..
                } = &mut step
                else {
                    unreachable!()
                };
                *attempts.last_mut().expect("sent attempt exists") = completed;
                *step_reply = Some(message);
                *progress = StepProgress::Closed {
                    end: StepEnd::Finish,
                };
                *updated_at = now;
                r.nodes.insert((lock.session_id, node_id), step);
                r.touch(lock.session_id, now)?;
                r.closed_step(lock.session_id, turn, node_id, None)
            }
            CloseStep::Continue { .. } | CloseStep::StopExecuting { .. } => {
                let (key, stop) = match input {
                    CloseStep::Continue { key } => (key, None),
                    CloseStep::StopExecuting { key, stop } => (key, Some(stop)),
                    _ => unreachable!(),
                };
                let (node_id, _, mut step) = r.executing(lock.session_id, key)?;
                let Node::Step {
                    progress,
                    slots,
                    updated_at,
                    ..
                } = &mut step
                else {
                    unreachable!()
                };
                if let Some(stop) = stop
                    && let Some(cause) = close::step_stop_cause(slots, stop)?
                {
                    *progress = StepProgress::Closed {
                        end: StepEnd::Stop { cause },
                    };
                    *updated_at = now;
                    r.nodes.insert((lock.session_id, node_id), step);
                    r.touch(lock.session_id, now)?;
                    return r.closed_step(lock.session_id, key.turn_id, node_id, None);
                }
                if slots
                    .iter()
                    .any(|s| !matches!(s.progress, SlotProgress::Terminal { .. }))
                {
                    return Err(transition(key.turn_id, "cannot continue unfinished calls"));
                }
                if close::first_stopping_cause(slots)?.is_some()
                    || slots
                        .iter()
                        .any(|s| close::stops_batch(&close::resolve_slot(s).payload))
                {
                    return Err(transition(key.turn_id, "cannot continue stopping calls"));
                }
                *progress = StepProgress::Closed {
                    end: StepEnd::Continue,
                };
                *updated_at = now;
                r.nodes.insert((lock.session_id, node_id), step);
                let next_node_id = r.next_node(lock.session_id)?;
                // 确定结果的批次继续结束；外部停止写到下一步，同次提交保留已决定的原因。
                let next_stop = stop.map(StopCause::from).or_else(|| {
                    (key.ordinal + 1 == MAX_MODEL_STEPS).then_some(StopCause::ModelStepLimit)
                });
                r.nodes.insert(
                    (lock.session_id, next_node_id),
                    Node::Step {
                        parent: node_id,
                        turn_id: key.turn_id,
                        step_ordinal: key.ordinal + 1,
                        progress: match next_stop {
                            Some(cause) => StepProgress::Closed {
                                end: StepEnd::Stop { cause },
                            },
                            None => StepProgress::Reserved,
                        },
                        attempts: Vec::new(),
                        reply: None,
                        slots: Vec::new(),
                        updated_at: now,
                    },
                );
                r.sessions
                    .get_mut(&lock.session_id)
                    .expect("session exists")
                    .current = next_node_id;
                r.touch(lock.session_id, now)?;
                r.closed_step(
                    lock.session_id,
                    key.turn_id,
                    node_id,
                    Some(NodeRef {
                        session_id: lock.session_id,
                        node: next_node_id,
                    }),
                )
            }
            CloseStep::StopBetweenRequests { turn_id, cause } => {
                let (node_id, _, mut step) = r.active_step(lock.session_id, turn_id)?;
                let Node::Step {
                    progress,
                    attempts,
                    updated_at,
                    ..
                } = &mut step
                else {
                    unreachable!()
                };
                let between_ready = match progress {
                    StepProgress::Reserved => attempts.is_empty(),
                    StepProgress::Requesting => {
                        attempts.last().is_some_and(|a| a.retry_delay_ms.is_some())
                    }
                    _ => false,
                };
                if !matches!(cause, StopCause::Cancelled | StopCause::Deadline) || !between_ready {
                    return Err(transition(turn_id, "invalid between-request stop"));
                }
                *progress = StepProgress::Closed {
                    end: StepEnd::Stop { cause },
                };
                *updated_at = now;
                r.nodes.insert((lock.session_id, node_id), step);
                r.touch(lock.session_id, now)?;
                r.closed_step(lock.session_id, turn_id, node_id, None)
            }
        })
    }

    pub fn rewind(
        &self,
        lock: &SessionExecutionLock,
        target: NodeId,
        project_path: &ProjectPath,
        now: Timestamp,
    ) -> Result<SessionRecord> {
        self.write(lock, Selection::Node(target), |r, _| {
            let s = r
                .sessions
                .get_mut(&lock.session_id)
                .ok_or_else(|| missing("session", lock.session_id))?;
            check_binding(s, project_path)?;
            if !r.nodes.contains_key(&(lock.session_id, target)) {
                return Err(missing("node", target.0));
            }
            if s.current != target {
                s.current = target;
                s.updated_at = now;
            }
            Ok(s.clone())
        })
    }
}

/// 会话库写入时再核对一次项目绑定（ADR 0012 第 8 节）；后台在写入之前已按同一规则核对过。
fn check_binding(session: &SessionRecord, project_path: &ProjectPath) -> Result<()> {
    if session.project_path == *project_path {
        Ok(())
    } else {
        Err(StoreError::BindingMismatch {
            session_id: session.session_id,
        })
    }
}
