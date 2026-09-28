// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::local::Selection;
use super::*;
use lancet_protocol::{
    BindingIdentity, InvocationBinding, InvocationId, ModelAttemptKey, ModelMessageAddress,
    ToolCallAddress,
};
use lattice_conversation::{MessagePart, MessageRole, MessageSemantic, NativeMessage};
use redb::ReadableTable;
use std::collections::BTreeSet;

impl Records {
    pub(super) fn turn_steps(&self, t: TurnId) -> Vec<StepRecord> {
        self.steps
            .range(
                StepKey {
                    turn_id: t,
                    ordinal: 0,
                }..=StepKey {
                    turn_id: t,
                    ordinal: u32::MAX,
                },
            )
            .map(|(_, s)| s.clone())
            .collect()
    }
    pub(super) fn next_node(&self, s: SessionId) -> Result<NodeId> {
        self.nodes
            .range((s, NodeId(0))..=(s, NodeId(u64::MAX)))
            .next_back()
            .map(|((_, n), _)| *n)
            .into_iter()
            .chain(self.node_tip)
            .max()
            .map_or(Ok(NodeId(0)), |n| next_sequence(n.0, "node").map(NodeId))
    }
    pub(super) fn durable(&self, s: SessionId, t: TurnId) -> Result<DurableTurnState> {
        let session = self
            .sessions
            .get(&s)
            .ok_or_else(|| missing("session", s))?
            .clone();
        let turn = self
            .turns
            .get(&t)
            .filter(|t| t.session_id == s)
            .ok_or_else(|| missing("turn", t))?
            .clone();
        Ok(DurableTurnState {
            session,
            turn,
            steps: self.turn_steps(t),
            next_node: self.next_node(s)?,
        })
    }
    fn running(&self, s: SessionId, t: TurnId) -> Result<DurableTurnState> {
        let state = self.durable(s, t)?;
        if !matches!(state.turn.state, TurnState::Running) || state.session.active_turn != Some(t) {
            return Err(transition(t, "turn is not running"));
        }
        Ok(state)
    }
    fn requesting(
        &self,
        s: SessionId,
        key: ModelAttemptKey,
    ) -> Result<(DurableTurnState, StepRecord)> {
        let state = self.running(s, key.turn_id)?;
        let step=state.steps.last().filter(|r|r.key.ordinal==key.model_step_ordinal&&matches!(r.state,StepState::Active {active:ActiveStep::Requesting {attempt}} if attempt==key.attempt_ordinal)).ok_or_else(||transition(key.turn_id,"request key does not match active attempt"))?.clone();
        Ok((state, step))
    }
    fn executing(&self, s: SessionId, key: StepKey) -> Result<(DurableTurnState, StepRecord)> {
        let state = self.running(s, key.turn_id)?;
        let step = state
            .steps
            .last()
            .filter(|r| {
                r.key == key
                    && matches!(
                        r.state,
                        StepState::Active {
                            active: ActiveStep::Executing { .. }
                        }
                    )
            })
            .ok_or_else(|| transition(key.turn_id, "step is not executing"))?
            .clone();
        Ok((state, step))
    }
    fn change(&mut self, slot: &InvocationSlot) -> Result<()> {
        let project = self
            .turns
            .get(&slot.binding.identity.turn_id)
            .ok_or_else(|| missing("turn", slot.binding.identity.turn_id))?
            .project_store_key;
        self.last_change = ChangeSeq(next_sequence(self.last_change.0, "change")?);
        self.index.insert(
            slot.binding.invocation_id,
            InvocationIndexEntry {
                address: slot.binding.identity.address(),
                last_change: self.last_change,
            },
        );
        self.changes.insert(
            self.last_change,
            InvocationChange {
                sequence: self.last_change,
                project_store_key: project,
                row: read::operator(slot),
            },
        );
        Ok(())
    }
    fn apply_closure(&mut self, s: SessionId, w: ClosureWrites) -> Result<ClosedStep> {
        for entry in w.changed_invocations {
            self.change(&InvocationSlot {
                binding: entry.binding,
                state: SlotState::Terminal(entry.terminal),
            })?;
        }
        let node = w.node.as_ref().map(|(node, _)| NodeRef {
            session_id: s,
            node: *node,
        });
        if let Some((n, value)) = w.node
            && self.nodes.insert((s, n), value).is_some()
        {
            return Err(corrupt("nodes", &s.to_string(), "closure overwrites node"));
        }
        let summary = close::summary(&w.turn_state);
        self.turns
            .get_mut(&w.closed_step.key.turn_id)
            .ok_or_else(|| missing("turn", w.closed_step.key.turn_id))?
            .state = w.turn_state;
        let session = self
            .sessions
            .get_mut(&s)
            .ok_or_else(|| missing("session", s))?;
        session.current = w.session_current;
        session.updated_at = w.session_updated_at;
        session.active_turn = None;
        self.steps.insert(w.closed_step.key, w.closed_step.clone());
        Ok(ClosedStep {
            record: w.closed_step,
            node,
            summary,
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
        let mut disabled = self.execution_health()?;
        let mut tx = self.db.begin_write()?;
        tx.set_durability(redb::Durability::Immediate)?;
        let (before, scope) = local::write(&tx, lock.session_id, selection)?;
        let mut after = before.clone();
        let value = f(&mut after, &tx)?;
        validate::local(&after, &scope)?;
        if before == after {
            return Ok(value);
        }
        codec::persist(&tx, &before, &after)?;
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
    pub fn accept_turn(
        &self,
        lock: &SessionExecutionLock,
        input: AcceptTurn,
    ) -> Result<AcceptedTurn> {
        self.check_lock(lock, input.session.id())?;
        self.write(lock, Selection::Accept(input.turn_id), |r, _| {
            if let Some(t) = r.turns.get(&input.turn_id) {
                if t.original_input != input.input
                    || t.project_store_key != input.project_store_key
                    || matches!(input.session,AcceptSession::Existing {id} if id!=t.session_id)
                {
                    return Err(StoreError::Conflict {
                        turn_id: input.turn_id,
                    });
                }
                return Ok(AcceptedTurn::Existing(t.clone()));
            }
            if input.input.len() > MAX_USER_INPUT_BYTES {
                return Err(invalid("input", "user input exceeds byte bound"));
            }
            validate::config(&input.config)?;
            let s = input.session.id();
            if matches!(input.session, AcceptSession::New { .. }) {
                if r.sessions.contains_key(&s) {
                    return Err(StoreError::SessionCollision { session_id: s });
                }
                if input.agent_selection != AgentSelection::Explicit {
                    return Err(invalid("agent", "new session requires explicit agent"));
                }
                r.sessions.insert(
                    s,
                    SessionRecord {
                        session_id: s,
                        project_store_key: input.project_store_key,
                        current: NodeId(0),
                        active_turn: None,
                        created_at: input.accepted_at,
                        updated_at: input.accepted_at,
                    },
                );
                r.nodes.insert((s, NodeId(0)), Node::Root);
            }
            let session = r.sessions.get(&s).ok_or_else(|| missing("session", s))?;
            if session.project_store_key != input.project_store_key {
                return Err(invalid("project", "session binding mismatch"));
            }
            if let Some(turn_id) = session.active_turn {
                return Err(StoreError::RecoveryRequired { turn_id });
            }
            if input.agent_selection == AgentSelection::Inherited
                && r.current_agent(s)?.as_deref() != Some(input.config.agent_name.as_str())
            {
                return Err(invalid("agent", "agent does not match current node"));
            }
            let parent = session.current;
            let n = r.next_node(s)?;
            let digest = input
                .config
                .digest()
                .map_err(|_| invalid("config", "cannot digest frozen configuration"))?;
            let turn = TurnRecord {
                turn_id: input.turn_id,
                session_id: s,
                user_node: n,
                original_input: input.input.clone(),
                project_store_key: input.project_store_key,
                config: input.config,
                config_digest: digest,
                accepted_at: input.accepted_at,
                state: TurnState::Running,
            };
            r.nodes.insert(
                (s, n),
                Node::UserMessage {
                    parent,
                    turn_id: input.turn_id,
                    message: NativeMessage::new(
                        MessageRole::User,
                        None,
                        vec![MessagePart::semantic_only(MessageSemantic::Text {
                            text: input.input,
                        })],
                    ),
                },
            );
            r.turns.insert(input.turn_id, turn.clone());
            let session = r.sessions.get_mut(&s).expect("session exists");
            session.current = n;
            session.active_turn = Some(input.turn_id);
            session.updated_at = input.accepted_at;
            Ok(AcceptedTurn::Accepted(turn))
        })
    }
    pub fn start_request(
        &self,
        lock: &SessionExecutionLock,
        key: ModelAttemptKey,
        now: Timestamp,
    ) -> Result<()> {
        self.write(lock, Selection::Turn(key.turn_id), |r, _| {
            let state = r.running(lock.session_id, key.turn_id)?;
            if key.model_step_ordinal >= MAX_MODEL_STEPS
                || key.attempt_ordinal >= state.turn.config.limits.max_model_attempts
            {
                return Err(transition(key.turn_id, "request budget exhausted"));
            }
            let mut record = close::closing_step(&state);
            if key.model_step_ordinal != record.key.ordinal
                || key.attempt_ordinal as usize != record.attempts.len()
                || !matches!(
                    record.state,
                    StepState::Active {
                        active: ActiveStep::RetryReady { .. }
                    }
                )
            {
                return Err(transition(key.turn_id, "request sequence mismatch"));
            }
            record.state = StepState::Active {
                active: ActiveStep::Requesting {
                    attempt: key.attempt_ordinal,
                },
            };
            r.steps.insert(record.key, record);
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
        self.write(lock, Selection::Turn(input.key.turn_id), |r, _| {
            let (state, mut record) = r.requesting(lock.session_id, input.key)?;
            if input.origin != validate::provider(&state.turn.config.provider)? {
                return Err(invalid("origin", "attempt provider mismatch"));
            }
            let retry = matches!(input.cause, StopCause::ProviderError { retryable: true })
                && record.attempts.len() + 1 < state.turn.config.limits.max_model_attempts as usize;
            record.attempts.push(AttemptRecord {
                attempt: input.key.attempt_ordinal,
                origin: input.origin,
                usage: input.usage,
                end: AttemptEnd::Stopped {
                    cause: input.cause.clone(),
                    diagnostic: input.diagnostic,
                },
            });
            if retry {
                let next_attempt = record.attempts.len() as u32;
                record.state = StepState::Active {
                    active: ActiveStep::RetryReady { next_attempt },
                };
                r.steps.insert(record.key, record);
                r.touch(lock.session_id, now)?;
                Ok(AttemptStopResult::RetryReady { next_attempt })
            } else {
                let w = close::closure(
                    &state,
                    record,
                    None,
                    Vec::new(),
                    StepEnd::Stop {
                        node: None,
                        cause: input.cause,
                    },
                    Vec::new(),
                    now,
                )?;
                Ok(AttemptStopResult::Closed(
                    r.apply_closure(lock.session_id, w)?,
                ))
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
            let (state, mut record) = r.requesting(lock.session_id, reply.key)?;
            if reply.origin != validate::provider(&state.turn.config.provider)?
                || reply
                    .message
                    .provider_origin
                    .is_some_and(|p| p != reply.origin)
            {
                return Err(invalid("origin", "reply provider mismatch"));
            }
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
            let completed = close::completed(&reply, lock.session_id);
            for (ordinal, part) in reply.message.parts.into_iter().enumerate() {
                let semantic = match part.semantic {
                    MessageSemantic::Text { text } => MessageSemantic::Text { text },
                    MessageSemantic::Reasoning { text } => MessageSemantic::Reasoning { text },
                    MessageSemantic::ToolCall { call } => {
                        let entry = state
                            .turn
                            .config
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
                                descriptor_identity: entry.descriptor_identity.clone(),
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
            let slots: Vec<_> = bindings
                .iter()
                .map(|b| InvocationSlot {
                    binding: b.clone(),
                    state: SlotState::Registered,
                })
                .collect();
            for slot in &slots {
                r.change(slot)?;
            }
            record.attempts.push(completed);
            record.state = StepState::Active {
                active: ActiveStep::Executing {
                    reply: bound.clone(),
                    slots,
                },
            };
            let key = record.key;
            r.steps.insert(key, record);
            r.touch(lock.session_id, now)?;
            Ok(RegisteredCalls {
                key,
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
            let key=StepKey {turn_id:binding.identity.turn_id,ordinal:binding.identity.model_step_ordinal};let (_,mut step)=r.executing(lock.session_id,key)?;
            let StepState::Active {active:ActiveStep::Executing {slots,..}}=&mut step.state else {unreachable!()};
            let position=slots.iter().position(|s|!matches!(s.state,SlotState::Terminal(_))).ok_or_else(||transition(key.turn_id,"no unfinished slot"))?;
            if slots[..position].iter().any(|s|matches!(&s.state,SlotState::Terminal(t) if close::stopping_cause(&s.binding,&t.result.payload).is_some()))||slots[position].binding!=*binding||!matches!(slots[position].state,SlotState::Registered) {return Err(transition(key.turn_id,"invocation cannot start"));}
            slots[position].state=SlotState::Started;r.change(&slots[position])?;r.steps.insert(key,step);r.touch(lock.session_id,now)?;Ok(StartedInvocation {binding:binding.clone()})
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
            let (_, mut step) = r.executing(lock.session_id, key)?;
            let StepState::Active {
                active: ActiveStep::Executing { slots, .. },
            } = &mut step.state
            else {
                unreachable!()
            };
            let slot = slots
                .iter_mut()
                .find(|s| s.binding.invocation_id == input.invocation_id)
                .ok_or_else(|| transition(key.turn_id, "invocation not in step"))?;
            if !matches!(slot.state, SlotState::Started)
                || !matches!(input.terminal.evidence, InvocationEvidence::Recorded(_))
                || matches!(input.terminal.result.payload, ResultPayload::NotExecuted(_))
            {
                return Err(transition(
                    key.turn_id,
                    "receipt requires a started invocation",
                ));
            }
            validate::terminal(&slot.binding, &input.terminal)?;
            slot.state = SlotState::Terminal(input.terminal);
            r.change(slot)?;
            r.steps.insert(key, step);
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
                let (state, mut record) = r.requesting(lock.session_id, reply.key)?;
                if reply.origin != validate::provider(&state.turn.config.provider)?
                    || reply
                        .message
                        .provider_origin
                        .is_some_and(|p| p != reply.origin)
                {
                    return Err(invalid("origin", "reply provider mismatch"));
                }
                record
                    .attempts
                    .push(close::completed(&reply, lock.session_id));
                let message = reply
                    .message
                    .bind_zero_tool()
                    .map_err(|_| invalid("reply", "finish contains calls"))?;
                let node = close::node_for(&state, &record, message, &[]);
                let end = StepEnd::Finish { node: node.0 };
                r.apply_closure(
                    lock.session_id,
                    close::closure(&state, record, Some(node), Vec::new(), end, Vec::new(), now)?,
                )
            }
            CloseStep::Continue { key } => {
                let (state, mut record) = r.executing(lock.session_id, key)?;
                let StepState::Active {
                    active: ActiveStep::Executing { reply, slots },
                } = &record.state
                else {
                    unreachable!()
                };
                let invocations: Vec<_> = slots
                    .iter()
                    .map(|s| match &s.state {
                        SlotState::Terminal(t) => Ok(ClosedInvocation {
                            binding: s.binding.clone(),
                            terminal: t.clone(),
                        }),
                        _ => Err(transition(key.turn_id, "cannot continue unfinished calls")),
                    })
                    .collect::<Result<_>>()?;
                if first_stopping_cause(&invocations)?.is_some() {
                    return Err(transition(key.turn_id, "cannot continue stopping calls"));
                }
                let (node, value) = close::node_for(&state, &record, reply.clone(), &invocations);
                record.state = StepState::Closed {
                    end: StepEnd::Continue { node },
                    invocations,
                };
                r.nodes.insert((lock.session_id, node), value);
                r.steps.insert(key, record.clone());
                r.sessions
                    .get_mut(&lock.session_id)
                    .expect("session exists")
                    .current = node;
                r.touch(lock.session_id, now)?;
                Ok(ClosedStep {
                    record,
                    node: Some(NodeRef {
                        session_id: lock.session_id,
                        node,
                    }),
                    summary: None,
                })
            }
            CloseStep::StopExecuting { key, stop } => {
                let (state, record) = r.executing(lock.session_id, key)?;
                let StepState::Active {
                    active: ActiveStep::Executing { reply, slots },
                } = &record.state
                else {
                    unreachable!()
                };
                let (invocations, changed) = close::fill_slots(slots, stop)?;
                let first = first_stopping_cause(&invocations)?;
                if first.is_none() && stop != DispatchStop::ProcessExit {
                    return Err(transition(
                        key.turn_id,
                        "close Continue before stopping between requests",
                    ));
                }
                let node = close::node_for(&state, &record, reply.clone(), &invocations);
                let end = StepEnd::Stop {
                    node: Some(node.0),
                    cause: first.unwrap_or(StopCause::ProcessExit),
                };
                r.apply_closure(
                    lock.session_id,
                    close::closure(&state, record, Some(node), invocations, end, changed, now)?,
                )
            }
            CloseStep::StopBetweenRequests { turn_id, cause } => {
                let state = r.running(lock.session_id, turn_id)?;
                let record = close::closing_step(&state);
                if !matches!(
                    cause,
                    StopCause::Cancelled | StopCause::Deadline | StopCause::ModelStepLimit
                ) || !matches!(
                    record.state,
                    StepState::Active {
                        active: ActiveStep::RetryReady { .. }
                    }
                ) || cause == StopCause::ModelStepLimit && record.key.ordinal != MAX_MODEL_STEPS
                {
                    return Err(transition(turn_id, "invalid between-request stop"));
                }
                r.apply_closure(
                    lock.session_id,
                    close::closure(
                        &state,
                        record,
                        None,
                        Vec::new(),
                        StepEnd::Stop { node: None, cause },
                        Vec::new(),
                        now,
                    )?,
                )
            }
        })
    }
    pub fn auto_close(
        &self,
        lock: &SessionExecutionLock,
        turn: TurnId,
        now: Timestamp,
    ) -> Result<AutoCloseResult> {
        self.write(lock, Selection::Turn(turn), |r, _| {
            match plan_auto_close(&r.durable(lock.session_id, turn)?, now)? {
                AutoClosePlan::AlreadyClosed { summary } => {
                    Ok(AutoCloseResult::AlreadyClosed(summary))
                }
                AutoClosePlan::Close(w) => Ok(AutoCloseResult::Closed(
                    r.apply_closure(lock.session_id, *w)?,
                )),
            }
        })
    }
    pub fn rewind(
        &self,
        lock: &SessionExecutionLock,
        target: NodeId,
        now: Timestamp,
    ) -> Result<SessionRecord> {
        self.write(lock, Selection::Node(target), |r, _| {
            let s = r
                .sessions
                .get_mut(&lock.session_id)
                .ok_or_else(|| missing("session", lock.session_id))?;
            if let Some(turn_id) = s.active_turn {
                return Err(StoreError::RecoveryRequired { turn_id });
            }
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
