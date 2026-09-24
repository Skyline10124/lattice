// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;
use std::time::{Duration, Instant};

use crate::connection::{cancel_message, detach_lines, not_submitted_notice, submission_notice};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use futures::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use lattice_api::TurnEvent;
#[cfg(test)]
use lattice_api::local::SessionObservation;
use lattice_api::local::{
    Acceptance, CancelStatus, CompletionConfirmation, ConnectionStatus, DetachReport, HistoryScope,
    LocalClient, LocalState, LocalTurn, LocalUpdate, ObservationPhase, ResetReason, TextAction,
    TurnId, TurnObservation, TurnRequest,
};

use crate::TuiOptions;
use crate::input::{CTRL_C_WINDOW, History, InputBuffer, KeyAction, resolve_key};
use crate::session::SessionContext;
use crate::view::{Dock, Sidebar, Viewport, frame_layout, transcript_lines};

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
    Committed { commit_id: String },
    Visible { commit_id: String },
    CommitFailed { reason: String },
    TurnFailed { reason: String },
    Cancelled { reason: Option<String> },
    LlmRetry { attempt: u32, max_attempts: u32, delay_ms: u64, message: String },
    LlmFailed { attempt: u32, max_attempts: u32, message: String },
    /// `/help` output and startup banner.
    Notice(Vec<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TurnPhase {
    Idle,
    Streaming,
    Committing,
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
    gap_pending: bool,
    // 仅用于当前界面呈现；控制和确认合并由 LocalTurn 决定。
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
            gap_pending: false,
            state: None,
            cancel_status: None,
            kitty_keyboard,
            last_ctrl_c: None,
            should_quit: false,
            term_width: 80,
            term_height: 24,
        }
    }

    /// Pure: fold one `TurnEvent` into `blocks` / `phase`. Exhaustive `match`
    /// with **no `_ =>` arm** (§9).
    #[rustfmt::skip]
    pub(crate) fn apply_turn_event(&mut self, event: TurnEvent) {
        match event {
            TurnEvent::TokenDelta { content } => match self.blocks.last_mut() {
                Some(Block::Assistant(buf)) => buf.push_str(&content),
                _ => self.blocks.push(Block::Assistant(content)),
            },
            TurnEvent::ToolCallStarted { id, name } => {
                self.upsert_tool(id, name, ToolState::Running);
            }
            TurnEvent::ToolCallFinished { id, name, ok, output_len, error_summary } => {
                self.upsert_tool(id, name, if ok { ToolState::Ok { output_len } } else {
                    ToolState::Failed { output_len, error: error_summary }
                });
            }
            TurnEvent::TurnCommitting => {
                if self.phase == TurnPhase::Streaming {
                    self.phase = TurnPhase::Committing;
                }
            }
            TurnEvent::TurnCommitted { commit_id, .. } => {
                self.fail_running_tools("turn ended before tool completion");
                self.push_status(StatusKind::Visible { commit_id });
            }
            TurnEvent::CommitFailed { reason } => {
                self.fail_running_tools(&reason);
                self.push_dock_status(StatusKind::CommitFailed { reason }, DockSeverity::Error);
            }
            TurnEvent::EvidenceDurabilityFailed { attempts } => {
                let reason = format!(
                    "terminal evidence durability failed after {attempts} attempt(s)"
                );
                self.fail_running_tools(&reason);
                self.push_dock_status(StatusKind::CommitFailed { reason }, DockSeverity::Error);
            }
            TurnEvent::RecoveryRequired { reason } => {
                let reason = format!("recovery required: {reason}");
                self.fail_running_tools(&reason);
                self.push_dock_status(StatusKind::CommitFailed { reason }, DockSeverity::Error);
            }
            TurnEvent::TurnFailed { reason } => {
                self.fail_running_tools(&reason);
                self.push_dock_status(StatusKind::TurnFailed { reason }, DockSeverity::Error);
            }
            TurnEvent::TurnCancelled { reason } => {
                self.fail_running_tools(reason.as_deref().unwrap_or("cancelled"));
                self.push_status(StatusKind::Cancelled { reason });
            }
            TurnEvent::LlmRequestError {
                attempt,
                max_attempts,
                will_retry,
                delay_ms,
                message,
                ..
            } => {
                if will_retry {
                    self.push_dock_status(
                        StatusKind::LlmRetry {
                            attempt,
                            max_attempts,
                            delay_ms: delay_ms.unwrap_or(0),
                            message,
                        },
                        DockSeverity::Warn,
                    );
                } else {
                    self.push_dock_status(
                        StatusKind::LlmFailed {
                            attempt,
                            max_attempts,
                            message,
                        },
                        DockSeverity::Error,
                    );
                }
            }
            TurnEvent::ModelStarted { .. } | TurnEvent::ModelFinished { .. } => {},
            TurnEvent::TurnStarted { .. }
            | TurnEvent::ReasoningDelta { .. }
            | TurnEvent::CepCallStarted { .. }
            | TurnEvent::CepCallFinished { .. } => {}
        }
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
            LocalUpdate::State {
                view,
                phase,
                confirmation,
                error,
                ..
            } => {
                self.apply_state(LocalState {
                    view,
                    phase,
                    confirmation,
                    error,
                });
            }
            LocalUpdate::Reset { reason, .. } => self.reset_display(reason),
            LocalUpdate::History { result, stale, .. } => match result {
                Ok(history) => self.replace_history(crate::session::history_blocks(*history)),
                Err(error) => self.push_status(StatusKind::Notice(vec![format!(
                    "{}持久正文不可用；此次内容未验证，/reconnect 重载：{:?}：{}",
                    if stale { "旧切点" } else { "" },
                    error.context,
                    error.error.message
                )])),
            },
            LocalUpdate::Event { event, text, .. } => {
                match text {
                    TextAction::Start => self.blocks.push(Block::Assistant(String::new())),
                    TextAction::StartIncomplete => {
                        self.push_status(StatusKind::Notice(vec![
                            "当前模型段前文可能缺失；仅显示重连后的新后缀".into(),
                        ]));
                        self.blocks.push(Block::Assistant(String::new()));
                    }
                    TextAction::None | TextAction::Continue | TextAction::Finish => {}
                }
                self.apply_turn_event(event);
            }
            LocalUpdate::Submission { turn_id, result } => match result {
                Ok(acceptance) => {
                    self.submitted = None;
                    if matches!(
                        acceptance,
                        Acceptance::Existing {
                            turn: TurnObservation::Committed(_)
                        }
                    ) {
                        self.current_turn = None;
                        self.phase = TurnPhase::Idle;
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
            .requested_turn
            .as_ref()
            .map(|turn| match turn {
                TurnObservation::Pending(turn) => turn.turn_id,
                TurnObservation::Committed(turn) => turn.turn_id,
            })
            .or_else(|| view.active_turn.as_ref().map(|turn| turn.turn_id));
        if observed.is_some() {
            self.observed_turn = observed;
        }
        self.ctx.active_agent = view.session.active_agent_name.clone();
        self.connected = true;
        if matches!(&view.requested_turn, Some(TurnObservation::Committed(_))) {
            self.fail_running_tools("turn ended before tool completion");
        }
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
            self.current_turn = view.active_turn.as_ref().map(|turn| turn.turn_id);
            self.phase = TurnPhase::Idle;
        }
        if state.phase == ObservationPhase::Finished {
            self.submitted = None;
        }
        if let Some(pending) = &view.active_turn {
            self.notice_once(format!(
                "pending Turn {}{}",
                pending.turn_id,
                if state.phase == ObservationPhase::Live {
                    ""
                } else {
                    "；本连接尚未确认 live 所有权；可重连查询，发送新消息前后台会自动关闭崩溃遗留任务"
                }
            ));
        }
        if let Some(error) = &state.error {
            self.notice_once(format!(
                "Turn {}：{}",
                self.observed_turn
                    .map_or_else(|| "未知".into(), |id| id.to_string()),
                error.message
            ));
        }
        self.state = Some(state);
        self.show_completion();
    }

    fn notice_once(&mut self, message: String) {
        let kind = StatusKind::Notice(vec![message]);
        if !self
            .blocks
            .iter()
            .any(|block| matches!(block, Block::Status { kind: known, .. } if *known == kind))
        {
            self.push_status(kind);
        }
    }

    fn show_completion(&mut self) {
        let Some(state) = &self.state else {
            return;
        };
        let Some(TurnObservation::Committed(turn)) = &state.view.requested_turn else {
            return;
        };
        let turn = turn.clone();
        let confirmed = state.confirmation == CompletionConfirmation::Confirmed;
        let mut found = false;
        for block in &mut self.blocks {
            if let Block::Status { kind, .. } = block
                && matches!(kind, StatusKind::Visible { commit_id } | StatusKind::Committed { commit_id } if *commit_id == turn.commit_id)
            {
                found = true;
                *kind = if confirmed {
                    StatusKind::Committed {
                        commit_id: turn.commit_id.clone(),
                    }
                } else {
                    StatusKind::Visible {
                        commit_id: turn.commit_id.clone(),
                    }
                };
            }
        }
        if !found {
            self.push_status(if confirmed {
                StatusKind::Committed {
                    commit_id: turn.commit_id.clone(),
                }
            } else {
                StatusKind::Visible {
                    commit_id: turn.commit_id.clone(),
                }
            });
        }
        let reason = format!("Turn {}：{:?}", turn.turn_id, turn.outcome);
        let kind = match turn.outcome {
            lattice_api::TurnOutcome::Completed { .. } => StatusKind::Notice(vec![reason]),
            lattice_api::TurnOutcome::Cancelled { .. } => StatusKind::Cancelled {
                reason: Some(reason),
            },
            lattice_api::TurnOutcome::Failed { .. }
            | lattice_api::TurnOutcome::Interrupted { .. } => StatusKind::TurnFailed { reason },
        };
        if !self
            .blocks
            .iter()
            .any(|block| matches!(block, Block::Status { kind: known, .. } if *known == kind))
        {
            self.push_status(kind);
        }
    }

    fn reset_display(&mut self, reason: ResetReason) {
        // 已移入历史位置的事件覆盖也只属于旧切点；恢复其 ToolCall-only 显示。
        for block in &mut self.blocks[..self.durable_len] {
            if let Block::Tool {
                state,
                source: source @ ToolSource::Event,
                ..
            } = block
            {
                *state = ToolState::Unknown;
                *source = ToolSource::HistoryCall;
            }
        }
        let statuses: Vec<_> = self
            .blocks
            .drain(self.durable_len..)
            .filter(|block| matches!(block, Block::Status { .. }))
            .collect();
        self.blocks.extend(statuses);
        if reason == ResetReason::Gap {
            self.gap_pending = true;
        }
        if self.gap_pending {
            self.notice_once("增量缺口；将替换持久视图，临时增量不重放".into());
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
        let temporary = self.blocks.split_off(self.durable_len);
        self.blocks = blocks;
        self.durable_len = self.blocks.len();
        for block in temporary {
            if matches!(&block, Block::Tool { id, .. } if history_ids.contains(id)) {
                continue;
            }
            if let Block::Status { kind: StatusKind::Visible { commit_id } | StatusKind::Committed { commit_id }, .. } = &block
                && self.blocks.iter().any(|known| matches!(known, Block::Status { kind: StatusKind::Visible { commit_id: id } | StatusKind::Committed { commit_id: id }, .. } if id == commit_id))
            { continue; }
            if matches!(block, Block::Status { .. }) && self.blocks.contains(&block) {
                continue;
            }
            self.blocks.push(block);
        }
        self.show_completion();
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
        self.gap_pending = false;
        self.current_turn = Some(id);
        self.observed_turn = Some(id);
        self.submitted = Some((id, raw.clone()));
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

/// 进入 raw 模式后并行接收固定状态和正文；启动不等待整段历史读取。
pub(crate) async fn run_app(
    mut options: TuiOptions,
    ctx: SessionContext,
    client: LocalClient,
) -> anyhow::Result<()> {
    let root = options.project_root.clone();
    options.project_root =
        tokio::task::spawn_blocking(move || std::fs::canonicalize(root)).await??;
    let connection = client.observe(ctx.session_id.clone(), None, HistoryScope::Session);
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
        Some(connection),
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
                if let Some(old) = turn.replace(client.observe(
                    app.ctx.session_id.clone(),
                    target,
                    HistoryScope::Session,
                )) {
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
        session_id: app.ctx.session_id.clone(),
        turn_id: app.current_turn.expect("提交前已分配身份"),
        workspace: options.project_root.clone(),
        input: prompt,
    };
    let configuration_client = client.clone();
    let session_id = request.session_id.clone();
    let options = options.clone();
    *turn_stream = Some(
        client.start_turn_with(request, HistoryScope::Session, move || {
            crate::session::freeze_configuration(configuration_client, session_id, options)
        }),
    );
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
        format!("session {}", ctx.session_id),
        format!("agent {}", ctx.active_agent),
        format!("project {}", ctx.project_branch),
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
                session_id: "sess".into(),
                active_agent: "agent".into(),
                project_branch: "main".into(),
            },
            false,
            // History is not under test here; an empty dir keeps it inert.
            Path::new("/nonexistent-tui-test-history"),
        )
    }

    #[test]
    fn snapshot_replaces_deltas_and_preserves_gap_without_terminal_event() {
        let mut app = test_app();
        let id = lattice_api::local::new_turn_id();
        let view = SessionObservation {
            state_oid: "state".into(),
            session: lattice_api::GraphSession {
                session_id: "session".into(),
                label: None,
                active_agent_name: "agent".into(),
                created_at: String::new(),
                updated_at: String::new(),
                project_target_branch: Some("main".into()),
                project_store_key: "key".into(),
            },
            head: lattice_api::GraphHead::Branch {
                name: "main".into(),
            },
            head_commit_id: Some("commit".into()),
            branches: Vec::new(),
            commit_count: 1,
            active_turn: None,
            requested_turn: Some(lattice_api::local::TurnObservation::Committed(Box::new(
                lattice_api::local::CommittedTurnObservation {
                    commit_id: "commit".into(),
                    turn_id: id,
                    parent_ids: Vec::new(),
                    agent_name: "agent".into(),
                    model: "model".into(),
                    provider: "provider".into(),
                    outcome: lattice_api::TurnOutcome::Completed {
                        finish_reason: lattice_api::TurnFinishReason::Stop,
                    },
                    origin: lattice_api::ClosureOrigin::Live,
                    usage: None,
                    workspace_history_checkpoint: lattice_api::WorkspaceHistoryCheckpoint::Empty,
                    created_at: String::new(),
                },
            ))),
        };
        let history = vec![
            Block::User("prompt".into()),
            Block::Assistant("durable".into()),
        ];
        app.blocks.push(Block::Assistant("lost delta".into()));
        app.apply_update(LocalUpdate::Reset {
            reason: ResetReason::Gap,
            cursor: None,
        });
        app.apply_state(LocalState {
            view: Box::new(view.clone()),
            phase: ObservationPhase::Live,
            confirmation: CompletionConfirmation::Unknown,
            error: None,
        });
        app.replace_history(history.clone());
        assert_eq!(app.phase, TurnPhase::Streaming);
        assert_eq!(app.current_turn, Some(id));
        assert_eq!(&app.blocks[..2], &history);
        let text = transcript_text(&app, 200);
        assert!(
            text.contains("缺口") && !text.contains("lost delta"),
            "{text}"
        );
        assert_eq!(text.matches("durable").count(), 1);
        assert_eq!(text.matches("增量缺口").count(), 1);
        app.apply_state(LocalState {
            view: Box::new(view.clone()),
            phase: ObservationPhase::Finished,
            confirmation: CompletionConfirmation::Confirmed,
            error: None,
        });
        app.reset_display(ResetReason::Snapshot);
        app.replace_history(history.clone());
        assert_eq!(&app.blocks[..2], &history);
        assert_eq!(app.phase, TurnPhase::Idle);
        assert!(app.current_turn.is_none());
        let text = transcript_text(&app, 200);
        assert!(
            text.contains("缺口") && text.contains("committed commit"),
            "{text}"
        );
        assert_eq!(text.matches("durable").count(), 1);
        assert_eq!(text.matches("增量缺口").count(), 1);

        for value in [
            serde_json::json!({"completed": {"finish_reason": "stop"}}),
            serde_json::json!({"failed": {"reason": "model_request_failed"}}),
            serde_json::json!({"cancelled": {"reason": "user_cancelled"}}),
            serde_json::json!({"interrupted": {"reason": "process_interrupted_before_model_attempt"}}),
        ] {
            let outcome: lattice_api::TurnOutcome = serde_json::from_value(value).unwrap();
            for confirmed in [false, true] {
                let mut app = test_app();
                let mut terminal = view.clone();
                if let Some(TurnObservation::Committed(turn)) = &mut terminal.requested_turn {
                    turn.outcome = outcome.clone();
                }
                let error = lattice_api::local::LocalError::from(
                    lattice_api::ApiError::StoreUnavailable("测试错误".into()),
                );
                app.apply_state(LocalState {
                    view: Box::new(terminal.clone()),
                    phase: ObservationPhase::Finished,
                    confirmation: if confirmed {
                        CompletionConfirmation::Confirmed
                    } else {
                        CompletionConfirmation::Unknown
                    },
                    error: Some(lattice_api::local::LocalError {
                        message: "original durability failure".into(),
                        ..error.clone()
                    }),
                });
                app.apply_update(LocalUpdate::History {
                    view: Box::new(terminal),
                    stale: false,
                    result: Err(lattice_api::local::HistoryReadError {
                        context: lattice_api::local::HistoryReadContext::Transcript {
                            object_id: "commit".into(),
                        },
                        error,
                    }),
                });
                assert!(app.connected, "正文失败不是断开");
                let text = transcript_text(&app, 200);
                assert!(
                    text.contains(&format!("{outcome:?}")) && text.contains(&id.to_string()),
                    "{text}"
                );
                assert!(
                    text.contains("original durability failure") && text.contains("正文不可用"),
                    "{text}"
                );
                assert_eq!(text.contains("committed commit"), confirmed, "{text}");
                assert_eq!(text.contains("完成确认未知"), !confirmed, "{text}");
                assert!(app.blocks.iter().any(|block| matches!(block,
                    Block::Status { kind: StatusKind::TurnFailed { .. }, .. } if matches!(outcome, lattice_api::TurnOutcome::Failed { .. } | lattice_api::TurnOutcome::Interrupted { .. })
                ) || matches!(block,
                    Block::Status { kind: StatusKind::Cancelled { .. }, .. } if matches!(outcome, lattice_api::TurnOutcome::Cancelled { .. })
                ) || matches!(block,
                    Block::Status { kind: StatusKind::Notice(_), .. } if matches!(outcome, lattice_api::TurnOutcome::Completed { .. })
                )));
                app.connection_lost("connection reset".into());
                let after = transcript_text(&app, 200);
                assert!(after.contains(&text) && after.contains("connection reset"));
                assert_eq!(app.observed_turn, Some(id));
                assert_eq!(app.phase, TurnPhase::Idle);
                assert!(app.apply_builtin("/reconnect"));
                assert_eq!(app.observed_turn, Some(id));
            }
        }
        // 不完整后缀的真实重连、同切点历史与正常新段由 run_loop 的 O2 回归承接。
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

    fn start_tool(app: &mut App, name: &str) {
        app.apply_turn_event(TurnEvent::ToolCallStarted {
            id: "call-1".into(),
            name: name.into(),
        });
    }
    #[test]
    fn token_deltas_coalesce_into_one_assistant_block() {
        let mut app = test_app();
        app.apply_turn_event(TurnEvent::TokenDelta {
            content: "a".into(),
        });
        app.apply_turn_event(TurnEvent::TokenDelta {
            content: "b".into(),
        });
        assert_eq!(app.blocks, vec![Block::Assistant("ab".into())]);
    }

    #[test]
    fn tool_finished_updates_the_block_with_matching_id() {
        let mut app = test_app();
        // Three concurrent tools: with only one in flight, "updates the block
        // with the matching id" would pass even if the code updated any block.
        for (id, name) in [
            ("call-1", "observe.workspace"),
            ("call-2", "mutate.edit"),
            ("call-3", "fetch.web"),
        ] {
            app.apply_turn_event(TurnEvent::ToolCallStarted {
                id: id.into(),
                name: name.into(),
            });
        }
        app.apply_turn_event(TurnEvent::ToolCallFinished {
            id: "call-2".into(),
            name: "mutate.edit".into(),
            ok: false,
            output_len: 0,
            error_summary: Some("precondition failed".into()),
        });

        let states: Vec<(&str, &ToolState)> = app
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Tool { id, state, .. } => Some((id.as_str(), state)),
                _ => None,
            })
            .collect();
        assert_eq!(states.len(), 3, "every started tool keeps its own block");
        for (id, state) in states {
            match (id, state) {
                ("call-2", ToolState::Failed { output_len, error }) => {
                    assert_eq!(*output_len, 0);
                    assert_eq!(error.as_deref(), Some("precondition failed"));
                }
                ("call-2", other) => panic!("call-2 should be Failed, got {other:?}"),
                (other_id, ToolState::Running) => {
                    assert!(matches!(other_id, "call-1" | "call-3"));
                }
                (other_id, other) => {
                    panic!("{other_id} must stay Running, got {other:?}")
                }
            }
        }
        app.apply_turn_event(TurnEvent::ToolCallFinished {
            id: "call-4".into(),
            name: "fetch.web".into(),
            ok: false,
            output_len: 42,
            error_summary: Some("结束先到的错误摘要".into()),
        });
        let orphan: Vec<_> = app
            .blocks
            .iter()
            .filter(|b| matches!(b, Block::Tool { id, .. } if id == "call-4"))
            .collect();
        assert_eq!(orphan.len(), 1);
        assert!(
            matches!(orphan[0], Block::Tool { name, state: ToolState::Failed { output_len: 42, error: Some(error) }, source: ToolSource::Event, .. } if name == "fetch.web" && error == "结束先到的错误摘要")
        );
    }

    #[test]
    fn terminal_event_closes_running_tools() {
        let mut app = test_app();
        start_tool(&mut app, "mutate.edit");
        app.apply_turn_event(TurnEvent::TurnFailed {
            reason: "provider failed".into(),
        });

        assert_failed_tool(&app, 0, "provider failed");
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
                kind: StatusKind::Cancelled { .. },
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
        app.apply_turn_event(TurnEvent::LlmRequestError {
            provider: None,
            model: "mock".into(),
            attempt: 1,
            max_attempts: 3,
            error_kind: "network".into(),
            retryable: true,
            will_retry: true,
            delay_ms: Some(100),
            status_code: None,
            message: "retry-a".into(),
        });
        let (kind, severity) = app.dock_status().expect("retry 应暂驻 Dock");
        assert!(matches!(kind, StatusKind::LlmRetry { message, .. } if message == "retry-a"));
        assert_eq!(severity, DockSeverity::Warn);
        assert!(!transcript_text(&app, 78).contains("retry-a"));

        // INFO 类状态直接进 transcript，Dock 不受影响。
        app.apply_turn_event(TurnEvent::TurnCancelled {
            reason: Some("info-i".into()),
        });
        assert!(transcript_text(&app, 78).contains("info-i"));
        assert!(matches!(
            app.dock_status(),
            Some((StatusKind::LlmRetry { .. }, _))
        ));

        // CommitFailed B → turnover：A 释放到 transcript，B 留在 ERROR Dock。
        app.apply_turn_event(TurnEvent::CommitFailed {
            reason: "commit-b".into(),
        });
        let (kind, severity) = app.dock_status().expect("失败应暂驻 Dock");
        assert!(matches!(kind, StatusKind::CommitFailed { reason } if reason == "commit-b"));
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
            app.apply_turn_event(TurnEvent::LlmRequestError {
                provider: None,
                model: "mock".into(),
                attempt: 1,
                max_attempts: 3,
                error_kind: "network".into(),
                retryable: true,
                will_retry: true,
                delay_ms: Some(0),
                status_code: None,
                message: message.into(),
            });
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
        app.apply_turn_event(TurnEvent::TurnFailed {
            reason: "turn-failed-a".into(),
        });
        app.apply_turn_event(TurnEvent::LlmRequestError {
            provider: None,
            model: "mock".into(),
            attempt: 2,
            max_attempts: 3,
            error_kind: "network".into(),
            retryable: true,
            will_retry: true,
            delay_ms: Some(0),
            status_code: None,
            message: "retry-c".into(),
        });
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
        app.apply_turn_event(TurnEvent::TurnFailed {
            reason: "blocked".into(),
        });

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
