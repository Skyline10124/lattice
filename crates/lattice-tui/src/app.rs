// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;
use std::time::{Duration, Instant};

use crate::TuiOptions;
use crate::connection::{cancel_message, detach_lines, not_submitted_notice, submission_notice};
use crate::input::{CTRL_C_WINDOW, History, InputBuffer, KeyAction, resolve_key};
use crate::session::SessionContext;
use crate::view::{Dock, Sidebar, Viewport, frame_layout, transcript_lines};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use futures::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use lattice_api::TurnEvent;
use lattice_api::local::{
    Acceptance, ActiveStateView, CancelStatus, ConnectionStatus, DetachReport, LocalClient,
    LocalState, LocalTurn, LocalUpdate, MaterialSummary, ModelAttemptKey, ObservationPhase,
    ObservationSnapshot, SessionId, StepKey, TextSegment, ToolResultOutcome, TurnId, TurnRequest,
    TurnSummary,
};

const FRAME: Duration = Duration::from_millis(33);

/// Dock 上 Warn / Error 两个非 Prompt 状态的严重级。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DockSeverity {
    Warn,
    Error,
}

/// 状态事件的展示位置：直接进 transcript，或暂驻 Dock。
/// 不变量：`blocks` 中至多存在一个 `StatusPlacement::Dock(_)`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatusPlacement {
    Transcript,
    Dock(DockSeverity),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Block {
    /// 无视觉内容的归属标记，用于节点正文实际到达后的替换。
    Step(StepKey),
    UserTurn(TurnId),
    /// 渲染作用域，不单独占行；历史不能被重标为当前 Agent 的发言。
    AgentName(String),
    User(String),
    /// Accumulates `TokenDelta`. A new delta appends to the trailing
    /// `Assistant` block when one exists, otherwise pushes a new block.
    Assistant(String),
    Tool {
        id: String,
        name: String,
        state: ToolState,
        source: ToolSource,
    },
    Status {
        kind: StatusKind,
        placement: StatusPlacement,
    },
}
/// 仅用于显示合并的来源；事件成功不能覆盖持久 ToolResult 的真实 outcome 或材料。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolSource {
    HistoryCall,
    HistoryResult,
    Event,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[rustfmt::skip]
pub(crate) enum ToolState {
    Running,
    Unknown,
    Unavailable { outcome: String },
    Ok { output_len: usize },
    Failed { output_len: usize, error: Option<String> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[rustfmt::skip]
pub(crate) enum StatusKind {
    Summary { turn_id: TurnId, summary: TurnSummary },
    TurnFailed { reason: String },
    LlmRetry { attempt: u32, delay_ms: u64, message: String },
    LlmFailed { attempt: u32, message: String },
    /// `/help` output and startup banner.
    Notice(Vec<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TurnPhase {
    Idle,
    Streaming,
    /// 取消请求已发起，接管确认与持久终态分别观察。
    Cancelling,
}

pub(crate) struct App {
    pub(crate) ctx: SessionContext,
    pub(crate) blocks: Vec<Block>,
    pub(crate) input: InputBuffer,
    pub(crate) history: History,
    pub(crate) viewport: Viewport,
    pub(crate) phase: TurnPhase,
    pub(crate) current_turn: Option<TurnId>,
    observed_turn: Option<TurnId>,
    pub(crate) connected: bool,
    cancel_requested: bool,
    reconnect_requested: bool,
    submitted: Option<(TurnId, String)>,
    durable_len: usize,
    generation: u64,
    attempt: Option<lattice_api::local::AttemptSegments>,
    // 控制与观察归 LocalTurn；这里只保存当前显示。
    state: Option<LocalState>,
    cancel_status: Option<CancelStatus>,
    /// Set when `supports_keyboard_enhancement()` reported true at startup.
    pub(crate) kitty_keyboard: bool,
    /// Instant of the last Ctrl+C, for the double-press exit rule.
    pub(crate) last_ctrl_c: Option<Instant>,
    pub(crate) should_quit: bool,
    pub(crate) term_width: u16,
    pub(crate) term_height: u16,
}

impl App {
    pub(crate) fn new(ctx: SessionContext, kitty_keyboard: bool, session_dir: &Path) -> Self {
        Self {
            ctx,
            blocks: Vec::new(),
            input: InputBuffer::new(),
            history: History::load(session_dir),
            viewport: Viewport::new(),
            phase: TurnPhase::Idle,
            current_turn: None,
            observed_turn: None,
            connected: true,
            cancel_requested: false,
            reconnect_requested: false,
            submitted: None,
            durable_len: 0,
            generation: 0,
            attempt: None,
            state: None,
            cancel_status: None,
            kitty_keyboard,
            last_ctrl_c: None,
            should_quit: false,
            term_width: 80,
            term_height: 24,
        }
    }

    pub(crate) fn apply_turn_event(&mut self, event: TurnEvent) {
        // 快照可能比事件窗口领先一个提交；已装入节点的尝试不再重建临时块。
        let key = match &event {
            TurnEvent::ModelStarted { key, .. }
            | TurnEvent::TokenDelta { key, .. }
            | TurnEvent::ReasoningDelta { key, .. }
            | TurnEvent::LlmRequestError { key, .. }
            | TurnEvent::ModelFinished { key, .. } => Some(key),
            _ => None,
        };
        if let Some(key) = key && self.blocks[..self.durable_len].iter().any(|block| {
            matches!(block, Block::Step(step) if step.turn_id == key.turn_id && step.ordinal == key.model_step_ordinal)
        }) { return; }
        match event {
            TurnEvent::ModelStarted { key, .. } => self.start_attempt(key),
            TurnEvent::TokenDelta { key, content } => {
                self.start_attempt(key);
                if let Some(attempt) = &mut self.attempt {
                    attempt.segments.push(TextSegment::Text {
                        text: content.clone(),
                    });
                }
                crate::session::append_assistant_text(&mut self.blocks, content);
            }
            TurnEvent::ReasoningDelta { key, content } => {
                self.start_attempt(key);
                if let Some(attempt) = &mut self.attempt {
                    attempt
                        .segments
                        .push(TextSegment::Reasoning { text: content });
                }
            }
            TurnEvent::ToolCallStarted { id, name } => {
                self.upsert_tool(id.to_string(), name, ToolState::Running)
            }
            TurnEvent::ToolCallFinished {
                id,
                name,
                outcome,
                material,
                error_summary,
            } => {
                self.upsert_tool(
                    id.to_string(),
                    name,
                    tool_state(outcome, &material, error_summary),
                );
            }
            TurnEvent::TurnCompleted { turn, summary, .. }
            | TurnEvent::TurnFailed { turn, summary, .. }
            | TurnEvent::TurnCancelled { turn, summary, .. } => {
                self.fail_running_tools("turn ended before tool completion");
                self.show_summary(turn.turn_id, summary);
                self.phase = TurnPhase::Idle;
            }
            TurnEvent::LlmRequestError {
                key,
                will_retry,
                delay_ms,
                message,
                ..
            } => {
                if will_retry {
                    self.remove_step(StepKey {
                        turn_id: key.turn_id,
                        ordinal: key.model_step_ordinal,
                    });
                    self.attempt = None;
                    self.push_dock_status(
                        StatusKind::LlmRetry {
                            attempt: key.attempt_ordinal,
                            delay_ms: delay_ms.unwrap_or(0),
                            message,
                        },
                        DockSeverity::Warn,
                    );
                } else {
                    self.push_dock_status(
                        StatusKind::LlmFailed {
                            attempt: key.attempt_ordinal,
                            message,
                        },
                        DockSeverity::Error,
                    );
                }
            }
            TurnEvent::StepClosed { .. } => {} // 实际正文到达前保留临时块。
            TurnEvent::TurnStarted { turn, .. } => self.ctx.session_id = Some(turn.session_id),
            TurnEvent::CallsRegistered { .. }
            | TurnEvent::ModelFinished { .. }
            | TurnEvent::CepCallStarted { .. }
            | TurnEvent::CepCallFinished { .. } => {}
        }
    }

    fn start_attempt(&mut self, key: ModelAttemptKey) {
        if self
            .attempt
            .as_ref()
            .is_some_and(|attempt| attempt.key == key)
        {
            return;
        }
        let step = StepKey {
            turn_id: key.turn_id,
            ordinal: key.model_step_ordinal,
        };
        self.remove_step(step);
        self.blocks.push(Block::Step(step));
        self.attempt = Some(lattice_api::local::AttemptSegments {
            key,
            segments: vec![],
        });
    }

    fn remove_step(&mut self, step: StepKey) {
        let mut removing = false;
        let temporary = self.blocks.split_off(self.durable_len);
        self.blocks.extend(temporary.into_iter().filter(|block| {
            match block {
                Block::Step(key) => removing = *key == step,
                Block::UserTurn(_) => removing = false,
                _ => {}
            }
            !removing || matches!(block, Block::Status { .. })
        }));
    }

    fn upsert_tool(&mut self, id: String, name: String, state: ToolState) {
        if let Some(Block::Tool {
            name: known_name,
            state: known,
            source,
            ..
        }) = self
            .blocks
            .iter_mut()
            .find(|block| matches!(block, Block::Tool { id: known, .. } if *known == id))
        {
            if *source == ToolSource::HistoryResult {
                return;
            }
            *known_name = name;
            *known = state;
            *source = ToolSource::Event;
        } else {
            self.blocks.push(Block::Tool {
                id,
                name,
                state,
                source: ToolSource::Event,
            });
        }
    }

    /// Pure: fold one resolved key action. Returns `true` when a turn should be
    /// submitted with the returned prompt.
    #[rustfmt::skip]
    pub(crate) fn apply_action(&mut self, action: KeyAction) -> Option<String> {
        if self.dock_status().is_some() {
            match action {
                // 无修饰 Space：释放当前 WARN / ERROR 回 transcript，
                // Space 被消费，不进入 InputBuffer。
                KeyAction::Insert(' ') => { self.release_dock_status(); return None; }
                // Dock 显示状态时，输入编辑与提交不得改动不可见的 InputBuffer。
                KeyAction::Insert(_)
                | KeyAction::Paste(_)
                | KeyAction::Newline
                | KeyAction::Submit
                | KeyAction::Backspace
                | KeyAction::Delete
                | KeyAction::Left
                | KeyAction::Right
                | KeyAction::Home
                | KeyAction::End
                | KeyAction::Up
                | KeyAction::Down => return None,
                // PageUp / PageDown、Ctrl+C、Ctrl+D 等全局动作沿用现有语义。
                _ => {}
            }
        }
        match action {
            KeyAction::Insert(c) => self.input.insert_char(c),
            KeyAction::Paste(text) => self.input.insert_paste(&text),
            KeyAction::Newline => self.input.newline(),
            KeyAction::Submit => return self.submit(),
            KeyAction::Backspace => self.input.backspace(),
            KeyAction::Delete => self.input.delete(),
            KeyAction::Left => self.input.move_left(),
            KeyAction::Right => self.input.move_right(),
            KeyAction::Home => self.input.home(),
            KeyAction::End => self.input.end(),
            KeyAction::Up => {
                if !self.input.move_up()
                    && let Some(entry) = self.history.prev()
                {
                    self.input.set_text(entry);
                }
            }
            KeyAction::Down => {
                if !self.input.move_down()
                    && let Some(entry) = self.history.next()
                {
                    self.input.set_text(entry);
                }
            }
            KeyAction::ScrollPageUp => self.viewport.page_up(self.transcript_height()),
            KeyAction::ScrollPageDown => {
                let layout = frame_layout(self.term_width, self.term_height);
                let rows = self.content_rows(layout.content_w);
                self.viewport.page_down(rows, self.transcript_height());
            }
            KeyAction::ScrollToBottom => {
                let layout = frame_layout(self.term_width, self.term_height);
                let rows = self.content_rows(layout.content_w);
                self.viewport.to_bottom(rows, self.transcript_height());
            }
            KeyAction::CancelOrClear => self.cancel_or_clear(),
            KeyAction::Quit => self.should_quit = true,
            KeyAction::Ignored => {}
        }
        None
    }

    /// 本地指令不提交 Turn；重连只重新观察，普通退出不触发取消。
    #[rustfmt::skip]
    pub(crate) fn apply_builtin(&mut self, input: &str) -> bool {
        match input {
            "/quit" | "/exit" | "/q" => { self.should_quit = true; true }
            "/reconnect" => { self.reconnect_requested = true; true }
            "/help" | "/?" => { self.push_status(StatusKind::Notice(help_lines(self.kitty_keyboard))); true }
            "/clear" => { self.blocks.clear(); self.durable_len = 0; self.viewport = Viewport::new(); true }
            _ => false,
        }
    }

    /// Number of transcript rows at the shared content width, used by `Viewport`.
    pub(crate) fn content_rows(&self, content_w: u16) -> usize {
        transcript_lines(self, content_w).len()
    }

    pub(crate) fn apply_stream_idle_timeout(&mut self) {
        self.connection_lost(format!(
            "观察 {}s 无增量；执行状态未知，/reconnect 重连",
            lattice_api::STREAM_IDLE_TIMEOUT_SECS
        ));
    }

    fn connection_lost(&mut self, reason: String) {
        self.connected = false;
        for block in &mut self.blocks {
            if let Block::Tool {
                state: state @ ToolState::Running,
                ..
            } = block
            {
                *state = ToolState::Unknown;
            }
        }
        self.push_status(StatusKind::Notice(vec![reason]));
    }

    fn apply_update(&mut self, update: LocalUpdate) {
        match update {
            LocalUpdate::State { view, phase, error } => {
                self.apply_state(LocalState { view, phase, error })
            }
            LocalUpdate::Reset { view, generation } => {
                self.generation = generation;
                self.reset_display(&view);
            }
            LocalUpdate::History {
                result,
                stale,
                generation,
                ..
            } => match result {
                Ok(history) if !stale && generation == self.generation => {
                    self.replace_history(crate::session::history_blocks(*history))
                }
                Ok(_) => {}
                Err(error) => self.push_status(StatusKind::Notice(vec![format!(
                    "{}持久正文不可用；此次内容未验证，/reconnect 重载：{:?}：{}",
                    if stale { "旧切点" } else { "" },
                    error.context,
                    error.error.message
                )])),
            },
            LocalUpdate::Finished { generation, .. } => {
                if generation == self.generation
                    && let Some(state) = &mut self.state
                {
                    state.phase = ObservationPhase::Finished;
                }
            }
            LocalUpdate::Event { event, .. } => self.apply_turn_event(event),
            LocalUpdate::Submission { turn_id, result } => match result {
                Ok(acceptance) => {
                    if let Acceptance::Accepted { turn } | Acceptance::Existing { turn } =
                        &acceptance
                    {
                        self.ctx.session_id = Some(turn.session_id);
                        self.ctx.active_agent = turn.agent_name.clone();
                        self.submitted = None;
                        if turn.summary.is_some() {
                            self.current_turn = None;
                            self.phase = TurnPhase::Idle;
                        }
                    }
                    self.push_status(StatusKind::Notice(vec![submission_notice(&acceptance)]));
                }
                Err(error) => {
                    self.restore_submission();
                    self.push_status(StatusKind::Notice(vec![format!(
                        "请求 {turn_id} 被拒绝或回执失败：{}",
                        error.message
                    )]));
                }
            },
            LocalUpdate::SubmissionUnknown { turn_id, error } => {
                self.push_status(StatusKind::Notice(vec![format!(
                    "提交结果未知：{turn_id}；继续观察同一请求，不自动重发"
                )]));
                if let Some(error) = error {
                    self.push_status(StatusKind::Notice(vec![error.message]));
                }
            }
            LocalUpdate::NotSubmitted { turn_id, .. } => {
                self.restore_submission();
                self.push_status(StatusKind::Notice(vec![not_submitted_notice(turn_id)]));
            }
            LocalUpdate::Cancel {
                turn_id,
                status,
                error,
            } => {
                self.cancel_status = Some(status);
                self.push_status(StatusKind::Notice(vec![format!(
                    "{}：{turn_id}",
                    cancel_message(status)
                )]));
                if let Some(error) = error {
                    self.push_status(StatusKind::Notice(vec![format!(
                        "取消诊断：turn {turn_id}，{:?}：{}",
                        error.kind, error.message
                    )]));
                }
            }
            LocalUpdate::Connection(status) => match status {
                ConnectionStatus::Eof => self.finish_stream(),
                ConnectionStatus::Idle => self.apply_stream_idle_timeout(),
                ConnectionStatus::Error(error) => self.connection_lost(error.message),
            },
        }
    }

    fn apply_state(&mut self, state: LocalState) {
        let view = &state.view;
        let observed = view
            .selected_turn
            .as_ref()
            .map(|turn| turn.turn_id)
            .or(view.session.active_turn);
        if observed.is_some() {
            self.observed_turn = observed;
        }
        self.ctx.session_id = Some(view.session.session_id);
        self.ctx.active_agent = view.current_agent.clone().unwrap_or_default();
        self.connected = true;
        if state.phase == ObservationPhase::Live {
            self.current_turn = observed;
            self.phase = if matches!(
                self.cancel_status,
                Some(CancelStatus::Pending | CancelStatus::Taken)
            ) {
                TurnPhase::Cancelling
            } else {
                TurnPhase::Streaming
            };
        } else if state.phase == ObservationPhase::Finished || self.submitted.is_none() {
            self.current_turn = view.session.active_turn;
            self.phase = TurnPhase::Idle;
        }
        if let Some(turn) = &view.selected_turn
            && let Some(summary) = &turn.summary
        {
            self.submitted = None;
            self.current_turn = view.session.active_turn;
            self.phase = TurnPhase::Idle;
            self.fail_running_tools("turn ended before tool completion");
            self.show_summary(turn.turn_id, summary.clone());
        }
        if let Some(id) = view.session.active_turn
            && !view.owned
        {
            self.notice_once(format!(
                "pending Turn {id}；无 live Owner；发送新消息前后台会自动关闭崩溃遗留任务"
            ));
        }
        if let Some(error) = &state.error {
            self.push_dock_status(
                StatusKind::TurnFailed {
                    reason: error.message.clone(),
                },
                DockSeverity::Error,
            );
        }
        self.state = Some(state);
    }

    fn notice_once(&mut self, message: String) {
        let kind = StatusKind::Notice(vec![message]);
        if !self
            .blocks
            .iter()
            .any(|block| matches!(block, Block::Status { kind:known, .. } if *known == kind))
        {
            self.push_status(kind);
        }
    }

    fn show_summary(&mut self, turn_id: TurnId, summary: TurnSummary) {
        if self.blocks.iter().any(|block| matches!(block, Block::Status { kind:StatusKind::Summary { turn_id:id, .. }, .. } if *id == turn_id)) { return; }
        self.push_status(StatusKind::Summary { turn_id, summary });
    }

    fn reset_display(&mut self, view: &ObservationSnapshot) {
        // 重同步只替换当前活动步骤；已关闭步骤的临时正文等待 History 实际安装。
        if let Some(active) = &view.active {
            self.remove_step(active.key);
            self.attempt = None;
            match &active.state {
                ActiveStateView::Requesting { .. } => {
                    if let Some(attempt) = &view.attempt {
                        self.start_attempt(attempt.key);
                        self.attempt = Some(attempt.clone());
                        for segment in &attempt.segments {
                            if let TextSegment::Text { text } = segment {
                                crate::session::append_assistant_text(
                                    &mut self.blocks,
                                    text.clone(),
                                );
                            }
                        }
                    }
                }
                ActiveStateView::RetryReady { .. } => {}
                ActiveStateView::Executing { reply, slots } => {
                    self.blocks.push(Block::Step(active.key));
                    crate::session::append_step(
                        &mut self.blocks,
                        lattice_api::local::StepContent {
                            assistant: reply.clone(),
                            results: slots
                                .iter()
                                .filter_map(|slot| slot.result.clone())
                                .collect(),
                        },
                    );
                    self.attempt = view.attempt.clone();
                }
            }
        }
    }

    fn replace_history(&mut self, mut blocks: Vec<Block>) {
        let events: std::collections::HashMap<_, _> = self
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Tool {
                    id,
                    source: ToolSource::Event,
                    ..
                } => Some((id.clone(), block.clone())),
                _ => None,
            })
            .collect();
        let mut history_ids = std::collections::HashSet::new();
        for block in &mut blocks {
            if let Block::Tool { id, source, .. } = block {
                history_ids.insert(id.clone());
                if *source == ToolSource::HistoryCall
                    && let Some(event) = events.get(id)
                {
                    *block = event.clone();
                }
            }
        }
        let loaded: Vec<_> = blocks
            .iter()
            .filter(|b| matches!(b, Block::Step(_) | Block::UserTurn(_)))
            .cloned()
            .collect();
        let temporary = self.blocks.split_off(self.durable_len);
        self.blocks = blocks;
        self.durable_len = self.blocks.len();
        let mut replaced = false;
        for block in temporary {
            if matches!(block, Block::Step(_) | Block::UserTurn(_)) {
                replaced = loaded.contains(&block);
            }
            if replaced && !matches!(block, Block::Status { .. }) {
                continue;
            }
            if matches!(&block, Block::Tool { id, .. } if history_ids.contains(id)) {
                continue;
            }
            if matches!(block, Block::Status { .. }) && self.blocks.contains(&block) {
                continue;
            }
            self.blocks.push(block);
        }
    }

    pub(crate) fn fail_running_tools(&mut self, reason: &str) {
        for block in &mut self.blocks {
            if let Block::Tool {
                state: state @ ToolState::Running,
                ..
            } = block
            {
                *state = ToolState::Failed {
                    output_len: 0,
                    error: Some(reason.to_owned()),
                };
            }
        }
    }

    fn finish_stream(&mut self) {
        self.connection_lost("观察连接结束；不代表 Turn 已关闭，/reconnect 重连".into());
    }

    fn expire_ctrl_c_notice(&mut self, now: Instant) -> bool {
        let expired = self
            .last_ctrl_c
            .is_some_and(|then| now.duration_since(then) > CTRL_C_WINDOW);
        if expired {
            self.last_ctrl_c = None;
        }
        expired
    }

    /// 当前 Dock 状态；`blocks` 中至多存在一个 `StatusPlacement::Dock(_)`，
    /// 推导时直接取唯一项，不做 `rev()` 兜底。
    pub(crate) fn dock_status(&self) -> Option<(&StatusKind, DockSeverity)> {
        self.blocks.iter().find_map(|block| match block {
            Block::Status {
                kind,
                placement: StatusPlacement::Dock(severity),
            } => Some((kind, *severity)),
            _ => None,
        })
    }

    /// 直接进 transcript。
    fn push_status(&mut self, kind: StatusKind) {
        self.blocks.push(Block::Status {
            kind,
            placement: StatusPlacement::Transcript,
        });
    }

    /// 先把当前 Dock status 的 placement 改回 `Transcript`，再以
    /// `Dock(severity)` 追加新 status。turnover 全部在内部完成。
    fn push_dock_status(&mut self, kind: StatusKind, severity: DockSeverity) {
        self.release_dock_status();
        self.blocks.push(Block::Status {
            kind,
            placement: StatusPlacement::Dock(severity),
        });
    }

    /// WARN / ERROR 状态下把当前 Dock status 释放回 transcript；它会在
    /// 原本的时间位置出现且只出现一次。无活动状态时是空操作。
    fn release_dock_status(&mut self) {
        for block in &mut self.blocks {
            if let Block::Status {
                placement: placement @ StatusPlacement::Dock(_),
                ..
            } = block
            {
                *placement = StatusPlacement::Transcript;
            }
        }
    }

    fn submit(&mut self) -> Option<String> {
        let raw = self.input.text();
        if raw.trim().is_empty() {
            return None;
        }
        if self.apply_builtin(raw.trim()) {
            self.history.push(raw.trim());
            self.input.clear();
            return None;
        }
        if self.phase != TurnPhase::Idle {
            return None;
        }
        self.history.push(raw.trim());
        self.input.clear();
        let id = lattice_api::local::new_turn_id();
        self.state = None;
        self.cancel_status = None;
        self.attempt = None;
        self.current_turn = Some(id);
        self.observed_turn = Some(id);
        self.submitted = Some((id, raw.clone()));
        self.blocks.push(Block::UserTurn(id));
        self.blocks
            .push(Block::AgentName(self.ctx.active_agent.clone()));
        self.blocks.push(Block::User(raw.clone()));
        self.push_status(StatusKind::Notice(vec![format!(
            "提交身份 {id}；确认丢失仍按此 ID 找回"
        )]));
        self.phase = TurnPhase::Streaming;
        Some(raw)
    }

    #[rustfmt::skip]
    fn cancel_or_clear(&mut self) {
        if self.current_turn.is_some() {
            self.cancel_requested = true;
            self.phase = TurnPhase::Cancelling;
        } else {
            self.input.clear();
        }
    }

    /// 只归还本客户端的草稿，不撤销已知 live、接管或完成显示。
    fn restore_submission(&mut self) {
        if let Some((_, prompt)) = self.submitted.take() {
            self.input.set_text(&prompt);
        }
        if !self
            .state
            .as_ref()
            .is_some_and(|state| state.phase == ObservationPhase::Live)
            && self.cancel_status != Some(CancelStatus::Taken)
        {
            self.phase = TurnPhase::Idle;
            self.current_turn = None;
        }
    }

    /// 与绘制路径使用同一内容宽度和垂直开销。
    /// Dock::height() 是 Dock 占用的唯一来源；bottomline 只在侧栏隐藏时存在。
    fn transcript_height(&self) -> usize {
        let layout = frame_layout(self.term_width, self.term_height);
        let bottom_h = match layout.sidebar {
            Sidebar::Visible => 0,
            Sidebar::Hidden => 1,
        };
        let dock = Dock::new(self);
        self.term_height
            .saturating_sub(dock.height(layout.content_w) + bottom_h) as usize
    }
}

pub(crate) fn session_label(id: Option<SessionId>) -> String {
    id.map_or_else(|| "未接受".into(), |id| id.to_string())
}

fn tool_state(
    outcome: ToolResultOutcome,
    material: &MaterialSummary,
    error: Option<String>,
) -> ToolState {
    let output_len = match material {
        MaterialSummary::Available { byte_length, .. } => *byte_length as usize,
        MaterialSummary::OutputTooLarge { .. } => {
            return ToolState::Unavailable {
                outcome: outcome.as_str().into(),
            };
        }
        MaterialSummary::NoContent => 0,
    };
    if outcome == ToolResultOutcome::Succeeded {
        ToolState::Ok { output_len }
    } else {
        ToolState::Failed {
            output_len,
            error: error.or_else(|| Some(outcome.as_str().into())),
        }
    }
}

/// 进入 raw 模式后并行接收固定状态和正文；启动不等待整段历史读取。
pub(crate) async fn run_app(
    mut options: TuiOptions,
    ctx: SessionContext,
    client: LocalClient,
) -> anyhow::Result<()> {
    let root = options.project_root.clone();
    options.project_root =
        tokio::task::spawn_blocking(move || std::fs::canonicalize(root)).await??;
    let connection = ctx.session_id.map(|id| client.observe(Some(id), None));
    let mut terminal = ratatui::try_init()?;
    let kitty = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    crossterm::execute!(std::io::stdout(), EnableBracketedPaste)?;
    if kitty {
        crossterm::execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
        )?;
    }
    install_cleanup_panic_hook(kitty);
    let _cleanup = TerminalCleanup { kitty };

    let mut app = App::new(ctx, kitty, &options.session_dir);
    app.push_status(StatusKind::Notice(startup_notice(&app.ctx)));
    let mut exit = run_loop(
        &options,
        &mut app,
        &client,
        connection,
        EventStream::new(),
        |app| {
            if let Ok(size) = terminal.size() {
                app.term_width = size.width;
                app.term_height = size.height;
                let layout = frame_layout(size.width, size.height);
                let th = app.transcript_height();
                let rows = app.content_rows(layout.content_w);
                app.viewport.on_content_changed(rows, th);
            }
            terminal.draw(|f| crate::view::draw(f, app))?;
            Ok(())
        },
    )
    .await;
    // 先复原终端，再由共同句柄有界承接既有意图；普通退出不新增取消。
    drop(_cleanup);
    if let Some(turn) = exit.turn {
        exit.handoffs.push(turn.detach().boxed());
    }
    while let Some(report) = exit.handoffs.next().await {
        for line in detach_lines(report) {
            eprintln!("{line}");
        }
    }
    exit.result
}

struct LoopExit {
    result: anyhow::Result<()>,
    turn: Option<LocalTurn>,
    handoffs: FuturesUnordered<BoxFuture<'static, DetachReport>>,
}

/// 产品接 crossterm/ratatui，测试注入按键并检查同一绘制回调。
async fn run_loop<E, D>(
    options: &TuiOptions,
    app: &mut App,
    client: &LocalClient,
    mut turn: Option<LocalTurn>,
    mut events: E,
    mut draw: D,
) -> LoopExit
where
    E: futures::Stream<Item = std::io::Result<Event>> + Unpin,
    D: FnMut(&mut App) -> anyhow::Result<()>,
{
    let mut ticker = tokio::time::interval(FRAME);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut handoffs = FuturesUnordered::new();
    let mut dirty = true;
    let result = loop {
        let cancel = std::mem::take(&mut app.cancel_requested);
        let reconnect = !app.should_quit && std::mem::take(&mut app.reconnect_requested);
        if cancel || reconnect {
            let target = app.current_turn.or(app.observed_turn);
            let needs_selection = turn
                .as_ref()
                .is_none_or(|turn| target.is_some() && target != turn.turn_id());
            if needs_selection {
                // 用户选择不同目标才另建只读句柄；旧控制交接不阻塞新目标或等待旧正文。
                if let Some(old) = turn.replace(client.observe(app.ctx.session_id, target)) {
                    handoffs.push(old.detach().boxed());
                }
                app.state = None;
                app.cancel_status = None;
                app.observed_turn = target;
            } else if reconnect && let Some(turn) = &turn {
                turn.reconnect();
            }
            if cancel && let Some(turn) = &turn {
                turn.cancel();
            }
        }
        if app.should_quit {
            break Ok(());
        }
        tokio::select! {
            event = events.next() => {
                match event {
                    Some(Ok(event)) => {
                        if let Some(prompt) = handle_input_event(app, event) {
                            if let Some(old) = turn.take() { handoffs.push(old.detach().boxed()); }
                            begin_turn(options, app, &mut turn, client, prompt);
                        }
                    }
                    Some(Err(_)) | None => app.should_quit = true,
                }
                dirty = true;
            }
            Some(report) = handoffs.next(), if !handoffs.is_empty() => {
                for line in detach_lines(report) {
                    app.notice_once(format!("历史交接：{line}"));
                }
                dirty = true;
            }
            update = async { turn.as_mut().expect("已有句柄").next().await }, if turn.is_some() => {
                if let Some(update) = update { app.apply_update(update); }
                else { turn = None; app.finish_stream(); }
                dirty = true;
            }
            _ = ticker.tick() => {
                dirty |= app.expire_ctrl_c_notice(Instant::now());
                if !dirty { continue; }
                if let Err(error) = draw(app) { break Err(error); }
                dirty = false;
            }
        }
    };
    LoopExit {
        result,
        turn,
        handoffs,
    }
}

#[rustfmt::skip]
fn handle_input_event(app: &mut App, event: Event) -> Option<String> {
    match event {
        Event::Paste(text) => {
            app.apply_action(KeyAction::Paste(text));
            None
        }
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            let action = if is_ctrl_c(&key) {
                ctrl_c_action(app)
            } else {
                resolve_key(key, app.kitty_keyboard, app.input.is_empty())
            };
            app.apply_action(action)
        }
        Event::Resize(w, h) => {
            app.term_width = w;
            app.term_height = h;
            None
        }
        _ => None,
    }
}

fn is_ctrl_c(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('c' | 'C' | '\u{3}'))
        && (key.modifiers.contains(KeyModifiers::CONTROL) || key.code == KeyCode::Char('\u{3}'))
}

#[rustfmt::skip]
fn ctrl_c_action(app: &mut App) -> KeyAction {
    let now = Instant::now();
    let double = app
        .last_ctrl_c
        .is_some_and(|t| now.duration_since(t) <= CTRL_C_WINDOW);
    app.last_ctrl_c = Some(now);
    if double {
        KeyAction::Quit
    } else {
        KeyAction::CancelOrClear
    }
}

fn begin_turn(
    options: &TuiOptions,
    app: &mut App,
    turn_stream: &mut Option<LocalTurn>,
    client: &LocalClient,
    prompt: String,
) {
    let request = TurnRequest {
        session_id: app.ctx.session_id,
        turn_id: app.current_turn.expect("提交前已分配身份"),
        workspace: options.project_root.clone(),
        input: prompt,
        agent: (!options.agent.is_empty()).then(|| options.agent.clone()),
    };
    let options = options.clone();
    *turn_stream = Some(client.start_turn_with(request, move |agent| {
        crate::session::freeze_configuration(agent, options)
    }));
}

#[rustfmt::skip]
fn help_lines(kitty: bool) -> Vec<String> {
    let nl = if kitty { "Shift+Enter  newline" } else { "Alt+Enter  newline" };
    vec![
        "/help  this message".into(),
        "/reconnect  只重连观察，不恢复或重新提交".into(),
        "/clear  clear transcript".into(),
        "/quit  exit (also Ctrl+D on empty input)".into(),
        "Ctrl+C  cancel turn; twice within 2s to quit".into(),
        "Enter  submit".into(),
        nl.into(),
    ]
}

#[rustfmt::skip]
fn startup_notice(ctx: &SessionContext) -> Vec<String> {
    vec![
        "LATTICE".into(),
        format!("session {}", session_label(ctx.session_id)),
        format!("agent {}", ctx.active_agent),
        "/help  show commands".into(),
        "/clear  clear transcript".into(),
        "/quit  exit".into(),
        "/reconnect  重新读取状态并观察".into(),
    ]
}

#[rustfmt::skip]
fn install_cleanup_panic_hook(kitty: bool) {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
        if kitty {
            let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
        }
        prev(info);
    }));
}

struct TerminalCleanup {
    kitty: bool,
}

#[rustfmt::skip]
impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        if self.kitty {
            let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
        }
        let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
        ratatui::restore();
    }
}

#[cfg(test)]
#[path = "app_loop_tests.rs"]
mod loop_tests;

#[cfg(test)]
#[path = "connection_tests.rs"]
mod connection_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::resolve_key;
    use lattice_api::TurnEvent;
    use std::time::Duration;

    fn test_app() -> App {
        App::new(
            SessionContext {
                session_id: None,
                active_agent: "agent".into(),
            },
            false,
            // History is not under test here; an empty dir keeps it inert.
            Path::new("/nonexistent-tui-test-history"),
        )
    }

    fn assert_failed_tool(app: &App, index: usize, reason: &str) {
        match &app.blocks[index] {
            Block::Tool {
                state: ToolState::Failed { output_len, error },
                ..
            } => {
                assert_eq!(*output_len, 0);
                assert!(error.as_deref().is_some_and(|error| error.contains(reason)));
            }
            other => panic!("expected failed tool, got {other:?}"),
        }
    }

    fn attempt() -> ModelAttemptKey {
        ModelAttemptKey {
            turn_id: TurnId::parse("11111111-1111-4111-8111-111111111111").unwrap(),
            model_step_ordinal: 0,
            attempt_ordinal: 1,
        }
    }
    fn invocation(n: u32) -> lattice_api::local::InvocationId {
        lattice_api::local::InvocationId::parse(&format!("22222222-2222-4222-8222-{n:012}"))
            .unwrap()
    }
    fn retry(message: &str, will_retry: bool) -> TurnEvent {
        TurnEvent::LlmRequestError {
            key: attempt(),
            retryable: will_retry,
            will_retry,
            delay_ms: Some(100),
            message: message.into(),
        }
    }
    fn start_tool(app: &mut App, name: &str) {
        app.apply_turn_event(TurnEvent::ToolCallStarted {
            id: invocation(1),
            name: name.into(),
        });
    }
    #[test]
    fn token_deltas_coalesce_into_one_assistant_block() {
        let mut app = test_app();
        for content in ["a", "b"] {
            app.apply_turn_event(TurnEvent::TokenDelta {
                key: attempt(),
                content: content.into(),
            });
        }
        let assistants: Vec<_> = app
            .blocks
            .iter()
            .filter(|b| matches!(b, Block::Assistant(_)))
            .collect();
        assert_eq!(assistants, vec![&Block::Assistant("ab".into())]);
    }
    #[test]
    fn tool_finished_updates_the_block_with_matching_id() {
        let mut app = test_app();
        for (n, name) in [
            (1, "observe.workspace"),
            (2, "mutate.edit"),
            (3, "fetch.web"),
        ] {
            app.apply_turn_event(TurnEvent::ToolCallStarted {
                id: invocation(n),
                name: name.into(),
            });
        }
        app.apply_turn_event(TurnEvent::ToolCallFinished {
            id: invocation(2),
            name: "mutate.edit".into(),
            outcome: ToolResultOutcome::Failed,
            material: MaterialSummary::NoContent,
            error_summary: Some("precondition failed".into()),
        });
        assert_eq!(app.blocks.len(), 3);
        assert_failed_tool(&app, 1, "precondition failed");
        for index in [0, 2] {
            assert!(matches!(
                &app.blocks[index],
                Block::Tool {
                    state: ToolState::Running,
                    ..
                }
            ));
        }
        app.apply_turn_event(TurnEvent::ToolCallFinished {
            id: invocation(4),
            name: "fetch.web".into(),
            outcome: ToolResultOutcome::Succeeded,
            material: MaterialSummary::Available {
                digest: [0; 32],
                byte_length: 42,
            },
            error_summary: None,
        });
        assert!(matches!(
            &app.blocks[3],
            Block::Tool {
                state: ToolState::Ok { output_len: 42 },
                source: ToolSource::Event,
                ..
            }
        ));
        assert_eq!(app.blocks.len(), 4);
    }
    #[test]
    fn terminal_event_closes_running_tools() {
        let mut app = test_app();
        start_tool(&mut app, "mutate.edit");
        let event:TurnEvent=serde_json::from_value(serde_json::json!({"type":"turn_failed","turn":{"session_id":"11111111111141118111111111111111","turn_id":attempt().turn_id},"step":{"key":{"turn_id":attempt().turn_id,"ordinal":0},"attempts":[],"state":{"state":"closed","end":{"end":"stop","node":null,"cause":{"cause":"process_exit"}},"slots":[]}},"summary":{"outcome":"failed","notice":{"level":"error","text":"process exited"}}})).unwrap();
        app.apply_turn_event(event);
        assert_failed_tool(&app, 0, "turn ended before tool completion");
    }

    #[test]
    fn stream_end_keeps_execution_state_and_marks_observation_unknown() {
        let mut app = test_app();
        app.phase = TurnPhase::Streaming;
        let id = lattice_api::local::new_turn_id();
        app.current_turn = Some(id);
        start_tool(&mut app, "mutate.edit");
        app.finish_stream();
        assert_eq!(app.phase, TurnPhase::Streaming);
        assert_eq!(app.current_turn, Some(id));
        assert!(!app.connected);
        assert!(matches!(
            &app.blocks[0],
            Block::Tool {
                state: ToolState::Unknown,
                ..
            }
        ));
    }

    #[test]
    fn esc_targets_the_current_turn_without_claiming_durable_cancellation() {
        let mut app = test_app();
        let id = lattice_api::local::new_turn_id();
        app.current_turn = Some(id);
        app.phase = TurnPhase::Streaming;
        app.apply_action(KeyAction::CancelOrClear);
        assert!(app.cancel_requested);
        assert_eq!(app.current_turn, Some(id));
        assert_eq!(app.phase, TurnPhase::Cancelling);
        app.apply_action(KeyAction::Quit);
        assert!(app.cancel_requested);
        assert!(!app.blocks.iter().any(|block| matches!(
            block,
            Block::Status {
                kind: StatusKind::Summary { .. },
                ..
            }
        )));
    }

    #[test]
    fn esc_when_idle_clears_input() {
        let mut app = test_app();
        app.input.set_text("hello");
        app.apply_action(KeyAction::CancelOrClear);
        assert!(app.input.is_empty());
        assert_eq!(app.phase, TurnPhase::Idle);
    }

    #[test]
    fn paste_grows_input_without_submitting() {
        let mut app = test_app();
        let submitted = app.apply_action(KeyAction::Paste("a\nb\nc".into()));
        assert!(submitted.is_none());
        assert_eq!(app.input.text(), "a\nb\nc");
        // 3 行内容 + rail：Dock::height 是视口高度的唯一来源。
        assert_eq!(Dock::new(&app).height(frame_layout(80, 24).content_w), 4);
        assert_eq!(app.phase, TurnPhase::Idle);
    }

    #[test]
    fn double_ctrl_c_within_window_quits_single_does_not() {
        let mut app = test_app();
        assert!(!matches!(ctrl_c_action(&mut app), KeyAction::Quit));
        app.apply_action(KeyAction::CancelOrClear);
        assert!(!app.should_quit);
        assert!(matches!(ctrl_c_action(&mut app), KeyAction::Quit));
        app.apply_action(KeyAction::Quit);
        assert!(app.should_quit);

        let mut app = test_app();
        ctrl_c_action(&mut app);
        app.last_ctrl_c = Some(Instant::now() - Duration::from_secs(3));
        assert!(!matches!(ctrl_c_action(&mut app), KeyAction::Quit));
        app.apply_action(KeyAction::CancelOrClear);
        assert!(!app.should_quit);
    }

    #[test]
    fn ctrl_c_notice_expiry_requests_redraw_once() {
        let mut app = test_app();
        let now = Instant::now();
        app.last_ctrl_c = Some(now - CTRL_C_WINDOW - Duration::from_millis(1));
        assert!(app.expire_ctrl_c_notice(now));
        assert!(app.last_ctrl_c.is_none());
        assert!(!app.expire_ctrl_c_notice(now));
    }

    #[test]
    fn ctrl_d_quits_only_on_empty_buffer() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let empty = resolve_key(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
            false,
            true,
        );
        let full = resolve_key(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
            false,
            false,
        );
        assert!(matches!(empty, KeyAction::Quit));
        assert!(matches!(full, KeyAction::Ignored));
    }

    #[test]
    fn builtin_clear_empties_blocks_and_resets_viewport() {
        let mut app = test_app();
        app.blocks.push(Block::User("x".into()));
        app.viewport.on_content_changed(20, 5);
        app.viewport.page_up(5);
        assert!(!app.viewport.following());
        assert!(app.apply_builtin("/clear"));
        assert!(app.blocks.is_empty());
        assert!(app.viewport.following());
        assert_eq!(app.viewport.scroll(), 0);
    }

    #[test]
    fn idle_stream_timeout_keeps_execution_unconfirmed() {
        let mut app = test_app();
        app.phase = TurnPhase::Streaming;
        start_tool(&mut app, "fetch.web");
        app.apply_stream_idle_timeout();
        assert_eq!(app.phase, TurnPhase::Streaming);
        assert!(!app.blocks.iter().any(|block| matches!(
            block,
            Block::Status {
                kind: StatusKind::TurnFailed { .. },
                ..
            }
        )));
    }

    /// §19.2：Warn / Error 的 Dock 生命周期与 transcript 时间顺序。
    /// 释放后的状态必须回到它原本的时间位置，且只出现一次。
    #[test]
    fn dock_lifecycle_keeps_transcript_order_and_draft() {
        let mut app = test_app();
        app.input.set_text("draft");
        // 初始为 Prompt。
        assert!(app.dock_status().is_none());

        // LlmRetry A → 只在 WARN Dock，transcript 暂不显示。
        app.apply_turn_event(retry("retry-a", true));
        let (kind, severity) = app.dock_status().expect("retry 应暂驻 Dock");
        assert!(matches!(kind, StatusKind::LlmRetry { message, .. } if message == "retry-a"));
        assert_eq!(severity, DockSeverity::Warn);
        assert!(!transcript_text(&app, 78).contains("retry-a"));

        // INFO 类状态直接进 transcript，Dock 不受影响。
        app.push_status(StatusKind::Notice(vec!["info-i".into()]));
        assert!(transcript_text(&app, 78).contains("info-i"));
        assert!(matches!(
            app.dock_status(),
            Some((StatusKind::LlmRetry { .. }, _))
        ));

        // LlmFailed B → turnover：A 释放到 transcript，B 留在 ERROR Dock。
        app.apply_turn_event(retry("commit-b", false));
        let (kind, severity) = app.dock_status().expect("失败应暂驻 Dock");
        assert!(matches!(kind, StatusKind::LlmFailed { message, .. } if message == "commit-b"));
        assert_eq!(severity, DockSeverity::Error);
        let text = transcript_text(&app, 78);
        assert!(text.contains("retry-a"), "释放的 warn 保持原时间位置");
        assert!(!text.contains("commit-b"), "新错误留在 Dock");

        // Space：B 释放，Dock 回 Prompt，Space 不进入 InputBuffer，草稿不变。
        let submitted = app.apply_action(KeyAction::Insert(' '));
        assert!(submitted.is_none());
        assert!(app.dock_status().is_none());
        assert_eq!(app.input.text(), "draft");
        assert_eq!(app.phase, TurnPhase::Idle);

        // 最终 transcript 顺序仍为 A, I, B。
        let text = transcript_text(&app, 78);
        assert!(text.contains("retry-a"));
        assert!(text.contains("commit-b"));
        let order: Vec<usize> = ["retry-a", "info-i", "commit-b"]
            .into_iter()
            .map(|marker| {
                transcript_lines(&app, 78)
                    .iter()
                    .position(|line| line.to_string().contains(marker))
                    .unwrap_or_else(|| panic!("缺少 {marker}"))
            })
            .collect();
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(order, sorted, "transcript 顺序必须保持 A, I, B");

        // 不变量：blocks 中至多一个 Dock placement。
        assert_eq!(dock_placement_count(&app), 0);
    }

    /// 同级替换 WARN A -> WARN B：旧状态在 turnover 时进入 transcript。
    #[test]
    fn dock_turnover_replaces_same_severity_without_losing_the_old() {
        let mut app = test_app();
        for message in ["retry-a", "retry-b"] {
            app.apply_turn_event(retry(message, true));
        }
        let (kind, severity) = app.dock_status().expect("最新 warn 暂驻 Dock");
        assert!(matches!(kind, StatusKind::LlmRetry { message, .. } if message == "retry-b"));
        assert_eq!(severity, DockSeverity::Warn);
        let text = transcript_text(&app, 78);
        assert!(text.contains("retry-a"), "旧 warn 已原位释放");
        assert!(!text.contains("retry-b"));
        assert_eq!(
            dock_placement_count(&app),
            1,
            "不变量：至多一个 Dock placement"
        );

        // ERROR A -> WARN B 同理。
        app.apply_turn_event(retry("turn-failed-a", false));
        app.apply_turn_event(retry("retry-c", true));
        assert!(matches!(
            app.dock_status(),
            Some((StatusKind::LlmRetry { message, .. }, DockSeverity::Warn)) if message == "retry-c"
        ));
        let text = transcript_text(&app, 78);
        assert!(text.contains("turn-failed-a"));
        assert!(text.contains("retry-a") && text.contains("retry-b"));
        assert_eq!(dock_placement_count(&app), 1);
    }

    /// §14.3：WARN / ERROR 下屏蔽不可见输入编辑，全局动作照常。
    #[test]
    fn dock_status_blocks_invisible_input_but_keeps_globals() {
        let mut app = test_app();
        app.input.set_text("draft");
        app.apply_turn_event(retry("blocked", false));

        // 普通字符、退格、提交都不改动不可见 InputBuffer。
        assert!(app.apply_action(KeyAction::Insert('x')).is_none());
        assert!(app.apply_action(KeyAction::Backspace).is_none());
        assert!(app.apply_action(KeyAction::Submit).is_none());
        assert_eq!(app.input.text(), "draft");

        // 无修饰 Space 释放后，输入恢复正常。
        app.apply_action(KeyAction::Insert(' '));
        assert!(app.dock_status().is_none());
        assert!(app.apply_action(KeyAction::Insert('x')).is_none());
        assert_eq!(app.input.text(), "draftx");

        // Prompt 态的 Space 保持普通输入行为。
        app.apply_action(KeyAction::Insert(' '));
        assert_eq!(app.input.text(), "draftx ");
    }

    fn transcript_text(app: &App, width: u16) -> String {
        transcript_lines(app, width)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn dock_placement_count(app: &App) -> usize {
        app.blocks
            .iter()
            .filter(|block| {
                matches!(
                    block,
                    Block::Status {
                        placement: StatusPlacement::Dock(_),
                        ..
                    }
                )
            })
            .count()
    }
}
