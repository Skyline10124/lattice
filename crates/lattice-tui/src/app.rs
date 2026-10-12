// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;
use std::time::{Duration, Instant};

use crate::TuiOptions;
use crate::blocks::{BlockId, BlockList, IdentifiedBlock, identity_slots};
use crate::connection::{
    cancellation_status, closing_status, detach_lines, discarded_comments_status, handoff_status,
    history_unavailable_status, not_submitted_status, observation_diagnostic_status,
    observation_lost_status, orphaned_turn_status, rejected_status, retry_status,
    submission_status, submission_unknown_status,
};
use crate::input::{CTRL_C_WINDOW, History, InputBuffer, KeyAction, resolve_key};
use crate::session::SessionContext;
#[cfg(test)]
use crate::view::transcript_lines;
use crate::view::{
    Dock, DockIntent, DockState, Focus, Panel, Transcript, Viewport, frame_layout,
    render_transcript,
};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use futures::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use lattice_api::TurnEvent;
use lattice_api::local::{
    Acceptance, ActiveStateView, CancelStatus, ConnectionStatus, DetachReport, LocalClient,
    LocalState, LocalTurn, LocalUpdate, MaterialSummary, ModelAttemptKey, NoticeLevel,
    ObservationPhase, ObservationSnapshot, PublicTurnOutcome, SessionId, StepKey, TextSegment,
    ToolResultOutcome, TurnId, TurnRequest, TurnSummary,
};

const FRAME: Duration = Duration::from_millis(33);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Block {
    /// 无视觉内容的归属标记，用于节点正文实际到达后的替换。
    Step(StepKey),
    UserTurn(TurnId, std::num::NonZeroUsize),
    /// 渲染作用域，不单独占行；历史不能被重标为当前 Agent 的发言。
    AgentName(String),
    User(String),
    CommentedUser(crate::comment::Message),
    /// 保留被拒提交的标记，但不重复挂载已归还的评论。
    RejectedUser(crate::comment::Message),
    HistoryAssistant(crate::comment::HistoryText),
    /// 接受或 Live 观察的显示标记，不伪造正文，也不拥有持久节点。
    AssistantStart {
        turn_id: TurnId,
        agent: String,
    },
    /// Accumulates `TokenDelta`. A new delta appends to the trailing
    /// `Assistant` block when one exists, otherwise pushes a new block.
    Assistant(String),
    Tool {
        id: String,
        name: String,
        state: ToolState,
        source: ToolSource,
    },
    Status(Status),
}

/// 状态消息只描述自己；是否暂驻 Dock 由 Dock 按等级决定，消息上没有位置字段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Status {
    pub(crate) level: NoticeLevel,
    /// 可多行，以换行分隔。
    pub(crate) text: String,
    /// 仅 Turn 结局消息携带所属 Turn 与结局：用于按 TurnId 去重，并决定标题线、符号与配色。
    pub(crate) closing: Option<(TurnId, PublicTurnOutcome)>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TurnPhase {
    Idle,
    Streaming,
    /// 取消请求已发起，接管确认与持久终态分别观察。
    Cancelling,
}

pub(crate) struct App {
    pub(crate) ctx: SessionContext,
    pub(crate) blocks: BlockList,
    /// 启动横幅固定在对话记录最上方。它不是状态消息，也不在 `blocks` 中，
    /// History 替换与 Dock 都碰不到它；身份只供浏览光标锚定。
    pub(crate) banner: Option<(BlockId, Vec<String>)>,
    pub(crate) markdown_cache: std::cell::RefCell<crate::markdown::RenderCache>,
    pub(crate) focus: Focus,
    pub(crate) dock: DockState,
    pub(crate) input: InputBuffer,
    pub(crate) queue: crate::queue::Queue,
    auto_sent_after: std::collections::HashSet<TurnId>,
    pub(crate) history: History,
    pub(crate) viewport: Viewport,
    pub(crate) phase: TurnPhase,
    pub(crate) current_turn: Option<TurnId>,
    observed_turn: Option<TurnId>,
    pub(crate) connected: bool,
    pub(crate) model: Option<String>,
    pub(crate) provider: Option<String>,
    pub(crate) context_tokens: Option<u64>,
    cancel_requested: bool,
    reconnect_requested: bool,
    submitted: Option<(TurnId, crate::queue::Outgoing)>,
    closed_turns: std::collections::HashSet<TurnId>,
    /// 已实时交付提交回执的请求；交接报告不再重述它们的回执。
    delivered_submissions: std::collections::HashSet<TurnId>,
    // None 仅表示打开已有会话后尚未装入第一份路径。
    loaded_user_count: Option<usize>,
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
            loaded_user_count: ctx.session_id.is_none().then_some(0),
            ctx,
            blocks: BlockList::default(),
            banner: None,
            markdown_cache: Default::default(),
            focus: Focus::Dock,
            dock: DockState::default(),
            input: InputBuffer::new(),
            queue: Default::default(),
            auto_sent_after: Default::default(),
            history: History::load(session_dir),
            viewport: Viewport::new(),
            phase: TurnPhase::Idle,
            current_turn: None,
            observed_turn: None,
            connected: true,
            model: None,
            provider: None,
            context_tokens: None,
            cancel_requested: false,
            reconnect_requested: false,
            submitted: None,
            closed_turns: Default::default(),
            delivered_submissions: Default::default(),
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
        // 状态栏消费最近事件，不受正文是否已被 History 接管影响。
        match &event {
            TurnEvent::ModelStarted {
                provider, model, ..
            } => {
                self.model = Some(model.clone());
                self.provider = Some(provider.as_str().to_owned());
            }
            TurnEvent::ModelFinished {
                usage: Some(usage), ..
            } => {
                self.context_tokens = Some(usage.total_tokens);
            }
            _ => {}
        }
        // 快照可能比事件窗口领先一个提交；已装入节点的尝试不再重建临时块。
        // 重试提示是状态消息而非正文，可能晚于关闭快照到达，不受此过滤。
        let key = match &event {
            TurnEvent::ModelStarted { key, .. }
            | TurnEvent::TokenDelta { key, .. }
            | TurnEvent::ReasoningDelta { key, .. }
            | TurnEvent::ModelFinished { key, .. } => Some(key),
            _ => None,
        };
        if let Some(key) = key && self.blocks.iter().take(self.durable_len).any(|block| {
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
                self.show_summary(turn.turn_id, summary, true);
                self.phase = TurnPhase::Idle;
            }
            TurnEvent::LlmRetry {
                key,
                delay_ms,
                notice,
            } => {
                self.remove_step(StepKey {
                    turn_id: key.turn_id,
                    ordinal: key.model_step_ordinal,
                });
                self.attempt = None;
                self.push_status(retry_status(key.attempt_ordinal, delay_ms, notice));
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
        let temporary = self
            .blocks
            .entries
            .split_off(self.durable_len.min(self.blocks.len()));
        self.blocks
            .entries
            .extend(temporary.into_iter().filter(|entry| {
                match &entry.block {
                    Block::Step(key) => removing = *key == step,
                    Block::UserTurn(..) => removing = false,
                    _ => {}
                }
                !removing || matches!(entry.block, Block::Status(_))
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

    /// 保留键先执行；其余动作交给 Dock 栈顶解释，模块不接触 LocalTurn。
    pub(crate) fn apply_action(&mut self, action: KeyAction) -> Option<String> {
        let width = frame_layout(self.term_width, self.term_height).content_w;
        let transcript = self.prepare_transcript();
        let lines = &transcript.lines;
        match action {
            KeyAction::ScrollPageUp | KeyAction::ScrollPageDown | KeyAction::ScrollToBottom => {
                let height = self.transcript_height(lines.len());
                if width == 0 || height == 0 {
                    if !self.queue.is_editing() {
                        self.focus_input(lines.len());
                    }
                    return None;
                }
                match action {
                    KeyAction::ScrollPageUp => self.viewport.page_up(height),
                    KeyAction::ScrollPageDown => self.viewport.page_down(lines.len(), height),
                    _ => self.viewport.to_bottom(lines.len(), height),
                }
                if let Focus::Transcript(cursor) = &mut self.focus {
                    if !matches!(action, KeyAction::ScrollToBottom) {
                        self.viewport.pause();
                    }
                    if !self.queue.is_editing() {
                        self.viewport.clamp_cursor(cursor);
                        cursor.reanchor(&transcript);
                    }
                }
            }
            KeyAction::CancelOrClear => self.cancel_or_clear(),
            KeyAction::Quit => self.should_quit = true,
            action if self.queue.is_editing() => {
                match action {
                    KeyAction::Submit | KeyAction::Clear => {
                        if let Some(anchor) =
                            self.queue.finish_edit(matches!(action, KeyAction::Submit))
                        {
                            self.focus_anchor(&anchor);
                        }
                        return self.auto_submit();
                    }
                    action => self.queue.edit(&action, transcript.editor_width(width)),
                }
                let transcript = self.prepare_transcript();
                if let Some((row, _)) = transcript.editor_cursor {
                    self.viewport.reveal(row);
                }
            }
            // 焦点在对话记录时暂驻消息不阻断其余按键，但无修饰 Space 仍释放它。
            KeyAction::Insert(' ') if self.dock.message_id().is_some() => self.dock.release(),
            action if matches!(self.focus, Focus::Transcript(_)) => match action {
                KeyAction::Submit => {
                    if let Focus::Transcript(cursor) = self.focus {
                        match transcript.comment_navigation(cursor, width) {
                            Some(crate::view::CommentNavigation::Jump(target)) => {
                                self.focus = Focus::Transcript(target);
                                self.viewport.reveal(target.row);
                            }
                            Some(crate::view::CommentNavigation::Compose(anchor)) => {
                                let id = self.blocks.allocate_id();
                                self.queue.start_comment(id, anchor, &self.blocks);
                            }
                            None => {
                                self.queue.start_edit(cursor.block.id);
                            }
                        }
                    }
                    let transcript = self.prepare_transcript();
                    if let Some((row, _)) = transcript.editor_cursor {
                        self.viewport.reveal(row);
                    }
                }
                // 不在编辑时 Esc 只回到输入区；删除草稿或排队消息用 Delete，避免误删。
                KeyAction::Clear => self.focus_input(lines.len()),
                KeyAction::Delete => {
                    if let Focus::Transcript(cursor) = self.focus {
                        if let Some(anchor) = self.queue.remove_draft(cursor.block.id) {
                            self.focus_anchor(&anchor);
                        } else if self.queue.remove(cursor.block.id) {
                            self.prepare_transcript();
                        }
                    }
                }
                // 对话记录中的可打印字符与粘贴被忽略，焦点不动，避免误输入。
                KeyAction::Insert(_) | KeyAction::ModifiedSpace | KeyAction::Paste(_) => {}
                action @ (KeyAction::Up
                | KeyAction::Down
                | KeyAction::Left
                | KeyAction::Right
                | KeyAction::Home
                | KeyAction::End) => {
                    if let Focus::Transcript(cursor) = &mut self.focus {
                        if matches!(action, KeyAction::Down)
                            && cursor.row >= lines.len().saturating_sub(1)
                        {
                            self.focus_input(lines.len());
                        } else {
                            cursor.move_in(&action, lines, width);
                            cursor.reanchor(&transcript);
                            self.viewport.reveal(cursor.row);
                        }
                    }
                }
                _ => {}
            },
            action => match Dock::new(self).route(action) {
                DockIntent::Submit => return self.submit(),
                DockIntent::ReleaseStatus => self.dock.release(),
                DockIntent::ClosePanel => self.dock.close_panel(),
                DockIntent::None => {}
                DockIntent::Edit(action) => match action {
                    KeyAction::Clear => self.input.clear(),
                    KeyAction::Up | KeyAction::Down => {
                        let (row, col) = crate::view::input_cursor(&self.input, width);
                        if matches!(action, KeyAction::Up)
                            && row == 0
                            && width > 0
                            && self.transcript_height(lines.len()) > 0
                            && let Some(cursor) =
                                transcript.cursor(transcript.lines.len().saturating_sub(1), col)
                        {
                            self.viewport.pause();
                            self.viewport.reveal(cursor.row);
                            self.focus = Focus::Transcript(cursor);
                        } else {
                            crate::view::move_input_vertical(
                                &mut self.input,
                                width,
                                matches!(action, KeyAction::Down),
                            );
                        }
                    }
                    KeyAction::HistoryPrev | KeyAction::HistoryNext => {
                        let entry = if matches!(action, KeyAction::HistoryPrev) {
                            self.history.prev()
                        } else {
                            self.history.next()
                        };
                        if let Some(entry) = entry {
                            self.input.set_text(entry);
                        }
                    }
                    action => self.input.edit(&action, width),
                },
            },
        }
        None
    }

    pub(crate) fn turn_closed(&self, turn: TurnId) -> bool {
        self.closed_turns.contains(&turn)
    }

    fn focus_anchor(&mut self, location: &crate::comment::Location) {
        let width = frame_layout(self.term_width, self.term_height).content_w;
        let transcript = render_transcript(self, width);
        if let Some(cursor) = transcript.anchor_cursor(location) {
            self.focus = Focus::Transcript(cursor);
            self.viewport.on_content_changed(
                transcript.lines.len(),
                self.transcript_height(transcript.lines.len()),
            );
            self.viewport.pause();
            self.viewport.reveal(cursor.row);
        }
    }

    fn focus_input(&mut self, rows: usize) {
        self.focus = Focus::Dock;
        self.viewport.to_bottom(rows, self.transcript_height(rows));
    }

    /// 本地指令不提交 Turn；重连只重新观察，普通退出不触发取消。
    #[rustfmt::skip]
    pub(crate) fn apply_builtin(&mut self, input: &str) -> bool {
        match input {
            "/quit" | "/exit" | "/q" => { self.should_quit = true; true }
            "/reconnect" => { self.reconnect_requested = true; true }
            "/help" | "/?" => { self.dock.open_panel(Panel { title: "HELP", lines: help_lines(self.kitty_keyboard) }); true }
            _ => false,
        }
    }

    pub(crate) fn prepare_transcript(&mut self) -> Transcript {
        let width = frame_layout(self.term_width, self.term_height).content_w;
        let transcript = render_transcript(self, width);
        let previous_scroll = self.viewport.scroll();
        let screen_row = match self.focus {
            Focus::Transcript(cursor) => cursor.row.saturating_sub(self.viewport.scroll()),
            Focus::Dock => 0,
        };
        // 合同只要求浏览时 Dock 变化不动视图；焦点在 Dock 且跟随时仍贴底，最新内容不被遮住。
        if self.dock.take_changed() && matches!(self.focus, Focus::Transcript(_)) {
            self.viewport.on_dock_changed(
                transcript.lines.len(),
                self.transcript_height(transcript.lines.len()),
            );
        } else {
            self.viewport.on_content_changed(
                transcript.lines.len(),
                self.transcript_height(transcript.lines.len()),
            );
        }
        if (width == 0
            || self.transcript_height(transcript.lines.len()) == 0
            || transcript.lines.is_empty())
            && !self.queue.is_editing()
        {
            self.focus_input(transcript.lines.len());
        }
        if let Focus::Transcript(mut cursor) = self.focus {
            let previous_row = cursor.row;
            let retained = if let Some((row, col)) = transcript.editor_cursor {
                cursor.row = row;
                cursor.desired_col = col;
                true
            } else {
                transcript.relocate(&mut cursor)
            };
            if !retained {
                self.viewport.keep_screen_row(cursor.row, screen_row);
            } else if self.viewport.following() {
                if self.viewport.scroll() != previous_scroll {
                    self.viewport.clamp_cursor(&mut cursor);
                }
            } else if cursor.row != previous_row {
                self.viewport.keep_screen_row(cursor.row, screen_row);
            }
            cursor.reanchor(&transcript);
            self.focus = Focus::Transcript(cursor);
        }
        transcript
    }

    fn connection_lost(&mut self, cause: &ConnectionStatus) {
        self.queue.pause();
        self.connected = false;
        for block in self.blocks.iter_mut() {
            if let Block::Tool {
                state: state @ ToolState::Running,
                ..
            } = block
            {
                *state = ToolState::Unknown;
            }
        }
        self.push_status(observation_lost_status(cause));
    }

    fn apply_handoff(&mut self, report: DetachReport) {
        // 旧句柄的回执只结清自己的名额，不覆盖新 Turn 的状态或输入。
        if let Some(id) = report.turn_id {
            match &report.submission {
                Some(Ok(Acceptance::Accepted { .. } | Acceptance::Existing { .. })) => {
                    self.queue.release(id);
                }
                Some(Err(_) | Ok(Acceptance::NeedsConfiguration { .. })) => {
                    if self.queue.reject(id) {
                        self.mark_rejected_comments(id);
                        self.discard_missing_comments();
                    }
                }
                None if report.not_submitted.is_some() => {
                    if self.queue.reject(id) {
                        self.mark_rejected_comments(id);
                        self.discard_missing_comments();
                    }
                }
                Some(Ok(Acceptance::ConfirmationUnknown { .. })) | None => {
                    if self.queue.release(id) {
                        self.queue.pause();
                    }
                }
            }
        }
        // 已由本客户端实时交付的回执与结局不再重述；取消、诊断等新事实照常显示。
        let mut report = report;
        if report
            .turn_id
            .is_some_and(|id| self.delivered_submissions.contains(&id))
        {
            report.submission = None;
        }
        if let Some(turn) = report
            .state
            .as_mut()
            .and_then(|state| state.view.selected_turn.as_mut())
            && self.summary_shown(turn.turn_id)
        {
            turn.summary = None;
        }
        for line in detach_lines(report) {
            self.notice_once(handoff_status(&line));
        }
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
                    // 只结合同一个历史切点的 Session 快照：该 Session 唯一进行中 Turn
                    // 之外的节点都已关闭；不能由“Step 已写入”推断它自身的 Turn 关闭。
                    if let Some(state) = &self.state
                        && state.view.history == history.anchor
                    {
                        self.closed_turns
                            .extend(history.entries.iter().filter_map(|entry| {
                                let turn = entry.content.turn_id()?;
                                (state.view.session.active_turn != Some(turn)).then_some(turn)
                            }));
                    }
                    self.replace_history(crate::session::history_blocks(*history))
                }
                Ok(_) => {}
                Err(error) => self.push_status(history_unavailable_status(stale, &error)),
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
                    // 自动发送的一条放回队首并回到空闲；手动提交按原有行为不归还输入区。
                    if matches!(acceptance, Acceptance::NeedsConfiguration { .. }) {
                        if self.queue.reject(turn_id) {
                            self.mark_rejected_comments(turn_id);
                            self.submitted = None;
                            self.restore_submission();
                        } else if self
                            .submitted
                            .as_ref()
                            .is_some_and(|(id, _)| *id == turn_id)
                        {
                            self.mark_rejected_comments(turn_id);
                            if let Some((_, message)) = self.submitted.take() {
                                self.queue.restore(message, &mut self.blocks);
                            }
                        }
                        self.discard_missing_comments();
                    }
                    if matches!(acceptance, Acceptance::ConfirmationUnknown { .. }) {
                        self.queue.pause();
                        if self.queue.release(turn_id) {
                            self.submitted = None;
                        }
                    }
                    if let Acceptance::Accepted { turn } | Acceptance::Existing { turn } =
                        &acceptance
                    {
                        self.queue.release(turn_id);
                        self.ctx.session_id = Some(turn.session_id);
                        self.ctx.active_agent = turn.agent_name.clone();
                        self.submitted = None;
                        if turn.summary.is_none() {
                            self.ensure_assistant_start(turn.turn_id, &turn.agent_name);
                        }
                        if turn.summary.is_some() {
                            self.current_turn = None;
                            self.phase = TurnPhase::Idle;
                        }
                    }
                    self.delivered_submissions.insert(turn_id);
                    // 接受由助手标题线表达，不另写接受行；找回、确认未知等其余结果照常显示。
                    if !matches!(acceptance, Acceptance::Accepted { .. }) {
                        self.push_status(submission_status(&acceptance));
                    }
                }
                Err(error) => {
                    self.delivered_submissions.insert(turn_id);
                    self.reject_submission(turn_id);
                    self.push_status(rejected_status(turn_id, &error));
                }
            },
            LocalUpdate::SubmissionUnknown { turn_id, error } => {
                self.queue.pause();
                if self.queue.release(turn_id) {
                    self.submitted = None;
                }
                self.push_status(submission_unknown_status(turn_id, error.as_ref()));
            }
            LocalUpdate::NotSubmitted { turn_id, .. } => {
                self.reject_submission(turn_id);
                self.push_status(not_submitted_status(turn_id));
            }
            LocalUpdate::Cancel {
                turn_id,
                status,
                error,
            } => {
                self.cancel_status = Some(status);
                self.push_status(cancellation_status(turn_id, status, error.as_ref()));
            }
            LocalUpdate::Connection(status) => self.connection_lost(&status),
        }
    }

    fn apply_state(&mut self, state: LocalState) {
        let view = &state.view;
        // 要在下面改写 current_turn 之前判断：本进程提交过或看着它运行过的 Turn，结局才是新消息。
        let watched = view.selected_turn.as_ref().is_some_and(|turn| {
            self.current_turn == Some(turn.turn_id)
                || self.delivered_submissions.contains(&turn.turn_id)
        });
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
            if let Some(turn) = &view.selected_turn {
                self.ensure_assistant_start(turn.turn_id, &turn.agent_name);
            }
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
            self.show_summary(turn.turn_id, summary.clone(), watched);
        }
        if let Some(id) = view.session.active_turn
            && !view.owned
        {
            self.notice_once(orphaned_turn_status(id));
        }
        if let Some(error) = &state.error {
            self.push_status(observation_diagnostic_status(error));
        }
        self.state = Some(state);
    }

    fn ensure_assistant_start(&mut self, turn_id: TurnId, agent: &str) {
        if !self.blocks.iter().any(
            |block| matches!(block, Block::AssistantStart { turn_id: id, .. } if *id == turn_id),
        ) {
            self.blocks.push(Block::AssistantStart {
                turn_id,
                agent: agent.into(),
            });
        }
    }

    fn notice_once(&mut self, status: Status) {
        if !self
            .blocks
            .iter()
            .any(|block| matches!(block, Block::Status(known) if *known == status))
        {
            self.push_status(status);
        }
    }

    fn summary_shown(&self, turn_id: TurnId) -> bool {
        self.blocks.iter().any(|block| {
            matches!(block, Block::Status(Status { closing: Some((id, _)), .. }) if *id == turn_id)
        })
    }

    /// `watched` 表示本进程提交过或看着它运行过这个 Turn：它的结局是新消息，交给 Dock。
    /// 重新打开 Session 时带出的旧结局只是历史，直接进对话记录，不打断输入。
    fn show_summary(&mut self, turn_id: TurnId, summary: TurnSummary, watched: bool) {
        self.closed_turns.insert(turn_id);
        if summary.outcome != lattice_api::local::PublicTurnOutcome::Completed {
            self.queue.pause();
        }
        if self.summary_shown(turn_id) {
            return;
        }
        let status = closing_status(turn_id, summary);
        if watched {
            self.push_status(status);
        } else {
            self.blocks.push(Block::Status(status));
        }
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

    fn replace_history(&mut self, blocks: Vec<Block>) {
        self.loaded_user_count = Some(
            blocks
                .iter()
                .rev()
                .find_map(|block| match block {
                    Block::UserTurn(_, ordinal) => Some(ordinal.get()),
                    _ => None,
                })
                .unwrap_or(0),
        );
        let old_slots = identity_slots(&self.blocks.entries);
        let mut blocks: Vec<_> = blocks
            .into_iter()
            .map(|block| self.blocks.identify(block))
            .collect();
        let new_slots = identity_slots(&blocks);
        // 接受标记归属于 UserMessage；正文已到则由持久助手/工具接管，否则放回 USER 之后。
        // 不把它当作状态提示，否则 History 会把它挪到下一节点之前或遗失。
        let mut loaded_starts = std::collections::HashSet::new();
        for marker in self
            .blocks
            .entries
            .iter()
            .filter(|entry| matches!(entry.block, Block::AssistantStart { .. }))
        {
            let Block::AssistantStart { turn_id, .. } = &marker.block else {
                continue;
            };
            let Some(start) = blocks
                .iter()
                .position(|entry| matches!(&entry.block, Block::UserTurn(id, _) if id == turn_id))
            else {
                continue;
            };
            loaded_starts.insert(*turn_id);
            let end = blocks
                .iter()
                .enumerate()
                .skip(start.saturating_add(1))
                .find(|(_, entry)| matches!(entry.block, Block::AgentName(_) | Block::UserTurn(..)))
                .map_or(blocks.len(), |(index, _)| index);
            if !blocks
                .get(start..end)
                .unwrap_or_default()
                .iter()
                .any(|entry| {
                    matches!(&entry.block, Block::Assistant(text) if !text.is_empty())
                        || matches!(&entry.block, Block::HistoryAssistant(text) if !text.text.is_empty())
                        || matches!(entry.block, Block::Tool { .. })
                })
            {
                let after_user = blocks
                    .iter()
                    .enumerate()
                    .take(end)
                    .skip(start.saturating_add(1))
                    .take_while(|(_, entry)| matches!(entry.block, Block::User(_) | Block::CommentedUser(_)))
                    .last()
                    .map_or(start.saturating_add(1), |(index, _)| {
                        index.saturating_add(1)
                    });
                blocks.insert(after_user.min(blocks.len()), marker.clone());
            }
        }
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
        for entry in &mut blocks {
            if let Block::Tool { id, source, .. } = &entry.block {
                history_ids.insert(id.clone());
                if *source == ToolSource::HistoryCall
                    && let Some(event) = events.get(id)
                {
                    entry.block = event.clone();
                }
            }
        }
        // 已装入节点所在组的起点：history_blocks 在每个 UserMessage 节点前放一个 AgentName。
        let starts: Vec<(Block, usize)> = blocks
            .iter()
            .enumerate()
            .filter(|(_, entry)| matches!(entry.block, Block::Step(_) | Block::UserTurn(..)))
            .map(|(i, entry)| {
                let previous = i.saturating_sub(1);
                let scoped = i > 0
                    && blocks
                        .get(previous)
                        .is_some_and(|e| matches!(e.block, Block::AgentName(_)));
                (entry.block.clone(), if scoped { previous } else { i })
            })
            .collect();
        let start_of = |block: &Block| {
            starts
                .iter()
                .find(|(marker, _)| match (marker, block) {
                    (Block::UserTurn(left, _), Block::UserTurn(right, _)) => left == right,
                    _ => marker == block,
                })
                .map(|(_, i)| *i)
        };
        // 结局行跟着它的 Turn 走：新历史给出这个 Turn 的结局时，旧的那条让位并把身份交给它，
        // 暂驻 Dock 的结局因此继续暂驻；新历史里已经没有这个 Turn 时，结局行不保留。
        let mut closings = std::collections::HashMap::new();
        let mut loaded_turns = std::collections::HashSet::new();
        for (at, entry) in blocks.iter().enumerate() {
            match &entry.block {
                Block::Status(Status {
                    closing: Some((turn, _)),
                    ..
                }) => {
                    closings.insert(*turn, at);
                }
                Block::UserTurn(turn, _) => {
                    loaded_turns.insert(*turn);
                }
                _ => {}
            }
        }
        let parked = self.dock.message_id();
        // 临时提示按时间顺序保留：放在它之后出现的第一个已装入节点之前，没有则留在末尾。
        // 旧持久块与已装入节点的实时块由新历史取代；未装入的实时块按原顺序接在末尾。
        let old = std::mem::take(&mut self.blocks.entries);
        let durable_len = self.durable_len;
        let mut inserts: Vec<(usize, IdentifiedBlock)> = Vec::new();
        let mut pending = Vec::new();
        let mut tail = Vec::new();
        let mut replaced = false;
        // 最近一个已装入节点之后进入 tail 的块，属于该节点所在的 Turn。
        let mut turn_tail = 0;
        for (index, entry) in old.into_iter().enumerate() {
            let block = &entry.block;
            if index == durable_len {
                replaced = false;
            }
            if matches!(block, Block::AssistantStart { turn_id, .. } if loaded_starts.contains(turn_id))
            {
                continue;
            }
            if let Block::Status(Status {
                closing: Some((turn, _)),
                ..
            }) = block
            {
                if let Some(&at) = closings.get(turn) {
                    if let Some(taken_over) = blocks.get_mut(at) {
                        taken_over.id = entry.id;
                    }
                    // 结局行是这一轮的最后一行：此前的临时提示与未装入的实时块排在它之前，不漂到末尾。
                    let before = tail.drain(turn_tail.min(tail.len())..);
                    inserts.extend(before.chain(pending.drain(..)).map(|live| (at, live)));
                    continue;
                }
                if !loaded_turns.contains(turn) && parked != Some(entry.id) {
                    continue;
                }
            }
            if matches!(block, Block::Status(_)) {
                pending.push(entry);
            } else if matches!(block, Block::Step(_) | Block::UserTurn(..)) {
                if let Some(start) = start_of(block) {
                    inserts.extend(pending.drain(..).map(|status| (start, status)));
                    replaced = true;
                    turn_tail = tail.len();
                } else if index >= durable_len {
                    tail.append(&mut pending);
                    tail.push(entry);
                    replaced = false;
                }
            } else if index < durable_len
                || replaced
                || matches!(block, Block::Tool { id, .. } if history_ids.contains(id))
            {
                continue;
            } else {
                tail.append(&mut pending);
                tail.push(entry);
            }
        }
        tail.append(&mut pending);
        let mut result: Vec<IdentifiedBlock> = Vec::with_capacity(
            blocks
                .len()
                .saturating_add(inserts.len())
                .saturating_add(tail.len()),
        );
        // 回退到其他位置后，旧节点在新历史中的顺序可能与原来不同；稳定排序保持同一位置上提示的先后。
        inserts.sort_by_key(|(at, _)| *at);
        // 内容相同的提示只留一份；Dock 暂驻的那条按身份保留，不因与已释放的提示同文而随 History 消失。
        let duplicate = |result: &[IdentifiedBlock], status: &IdentifiedBlock| {
            parked != Some(status.id) && result.iter().any(|known| known.block == status.block)
        };
        let mut inserts = inserts.into_iter().peekable();
        for (index, entry) in blocks.into_iter().enumerate() {
            while let Some((_, status)) = inserts.next_if(|(at, _)| *at == index) {
                if !duplicate(&result, &status) {
                    result.push(status);
                }
            }
            result.push(entry);
        }
        self.durable_len = result.len();
        for entry in tail {
            if !(matches!(entry.block, Block::Status(_)) && duplicate(&result, &entry)) {
                result.push(entry);
            }
        }
        self.blocks.entries = result;
        self.blocks.inherit_discarded(old_slots, new_slots);
        self.discard_missing_comments();
    }

    pub(crate) fn fail_running_tools(&mut self, reason: &str) {
        for block in self.blocks.iter_mut() {
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
        self.connection_lost(&ConnectionStatus::Eof);
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

    /// 新到状态消息的唯一入口：消息按时间位置进入对话记录，是否暂驻由 Dock 自己决定。
    fn push_status(&mut self, status: Status) {
        let id = self.blocks.allocate_id();
        self.dock.offer(id, &status);
        self.blocks.entries.push(IdentifiedBlock {
            id,
            block: Block::Status(status),
        });
    }

    pub(crate) fn show_banner(&mut self) {
        self.banner = Some((self.blocks.allocate_id(), startup_banner()));
    }

    fn submit(&mut self) -> Option<String> {
        let raw = self.input.text();
        if raw.trim().is_empty() && self.queue.draft_count() == 0 {
            if self.phase == TurnPhase::Idle && self.input.is_empty() {
                self.queue.resume();
                return self.send_queue_head();
            }
            return None;
        }
        if self.apply_builtin(raw.trim()) {
            self.history.push(raw.trim());
            self.input.clear();
            return None;
        }
        if self.phase != TurnPhase::Idle {
            let id = self.blocks.allocate_id();
            if self.queue.enqueue(id, raw) {
                self.input.clear();
            }
            return None;
        }
        let ordinal = std::num::NonZeroUsize::new(self.loaded_user_count?.saturating_add(1))?;
        self.input.clear();
        let id = lattice_api::local::new_turn_id();
        let message = self.queue.take_message(raw);
        Some(self.start_submission(message, id, ordinal))
    }

    fn auto_submit(&mut self) -> Option<String> {
        let state = self.state.as_ref()?;
        let turn = state.view.selected_turn.as_ref()?;
        if !turn
            .summary
            .as_ref()
            .is_some_and(|s| s.outcome == lattice_api::local::PublicTurnOutcome::Completed)
            || self.auto_sent_after.contains(&turn.turn_id)
        {
            return None;
        }
        let completed = turn.turn_id;
        let raw = self.send_queue_head()?;
        self.auto_sent_after.insert(completed);
        Some(raw)
    }

    fn send_queue_head(&mut self) -> Option<String> {
        if self.should_quit || self.phase != TurnPhase::Idle {
            return None;
        }
        let ordinal = std::num::NonZeroUsize::new(self.loaded_user_count?.saturating_add(1))?;
        let id = lattice_api::local::new_turn_id();
        let raw = self.queue.start_send(id)?;
        Some(self.start_submission(raw, id, ordinal))
    }

    // 手动与排队共用提交路径，不借用或覆盖正在编辑的输入区。
    fn start_submission(
        &mut self,
        message: crate::queue::Outgoing,
        id: TurnId,
        ordinal: std::num::NonZeroUsize,
    ) -> String {
        self.history.push(message.text.trim());
        let raw = message.encode();
        self.state = None;
        self.cancel_status = None;
        self.attempt = None;
        self.current_turn = Some(id);
        self.observed_turn = Some(id);
        self.submitted = Some((id, message));
        // UserTurn 先于 AgentName：历史装入时以 UserTurn 定位已装入节点，其前的临时提示才会放回该轮之前。
        self.blocks.push(Block::UserTurn(id, ordinal));
        self.blocks
            .push(Block::AgentName(self.ctx.active_agent.clone()));
        self.blocks.push(crate::session::user_block(raw.clone()));
        self.phase = TurnPhase::Streaming;
        raw
    }

    #[rustfmt::skip]
    fn cancel_or_clear(&mut self) {
        if self.current_turn.is_some() {
            self.cancel_requested = true;
            self.phase = TurnPhase::Cancelling;
        } else if self.dock.shows_input() {
            self.input.clear();
        }
    }

    fn discard_missing_comments(&mut self) {
        let steps = self
            .blocks
            .iter()
            .take(self.durable_len)
            .filter_map(|block| match block {
                Block::Step(key) => Some(*key),
                _ => None,
            })
            .collect();
        let dropped = self.queue.retain_steps(&steps);
        if !dropped.is_empty() {
            self.push_status(discarded_comments_status(&dropped));
        }
    }

    /// 自动发送的一条放回队首且不进输入区；手动提交归还输入区。两者都回到空闲。
    fn reject_submission(&mut self, turn_id: TurnId) {
        let automatic = self.queue.reject(turn_id);
        let manual = self
            .submitted
            .as_ref()
            .is_some_and(|(id, _)| *id == turn_id);
        if automatic || manual {
            self.mark_rejected_comments(turn_id);
        }
        if manual {
            if automatic {
                self.submitted = None;
            }
            self.restore_submission();
        }
        if automatic || manual {
            self.discard_missing_comments();
        }
    }

    fn mark_rejected_comments(&mut self, turn_id: TurnId) {
        let mut scope = None;
        for (index, entry) in self.blocks.entries.iter_mut().enumerate() {
            if let Block::UserTurn(id, _) = &entry.block {
                scope = Some(*id);
            }
            if index >= self.durable_len
                && scope == Some(turn_id)
                && let Block::CommentedUser(message) = &entry.block
            {
                entry.block = Block::RejectedUser(message.clone());
            }
        }
    }

    /// 只归还本客户端的草稿，不撤销已知 live、接管或完成显示。
    fn restore_submission(&mut self) {
        if let Some((_, prompt)) = self.submitted.take() {
            let text = self.queue.restore(prompt, &mut self.blocks);
            self.input.set_text(&text);
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

    /// 与绘制路径使用同一内容宽度及 Dock 高度，statusline 始终占一行；
    /// `rows` 是同一宽度下的对话记录行数，Dock 高度随它变化。
    pub(crate) fn transcript_height(&self, rows: usize) -> usize {
        let layout = frame_layout(self.term_width, self.term_height);
        self.term_height
            .saturating_sub(1)
            .saturating_sub(Dock::new(self).height(layout.content_w, rows)) as usize
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
    // try_init 可能在 raw/备用屏幕已改变后失败，恢复 guard 必须先于初始化。
    let mut cleanup = TerminalCleanup { kitty: false };
    let mut terminal = ratatui::try_init()?;
    let kitty = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    install_cleanup_panic_hook(kitty);
    cleanup.kitty = kitty;
    enable_terminal_modes(&mut std::io::stdout(), kitty)?;

    let mut app = App::new(ctx, kitty, &options.session_dir);
    app.show_banner();
    let mut exit = run_loop(
        &options,
        &mut app,
        &client,
        connection,
        EventStream::new(),
        |app| {
            terminal.draw(|f| crate::view::draw(f, app))?;
            Ok(())
        },
    )
    .await;
    // 先复原终端，再由共同句柄有界承接既有意图；普通退出不新增取消。
    drop(cleanup);
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
                app.apply_handoff(report);
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
        if let Some(prompt) = app.auto_submit() {
            if let Some(old) = turn.take() {
                handoffs.push(old.detach().boxed());
            }
            begin_turn(options, app, &mut turn, client, prompt);
            dirty = true;
        }
    };
    // 输入结束、普通退出和绘制失败都保存尚未发送的原文；不保存编辑器草稿。
    for text in app.queue.drain() {
        app.history.push(&text);
    }
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
                resolve_key(key, app.kitty_keyboard, app.input.is_empty() && matches!(app.focus, Focus::Dock) && app.dock.shows_input())
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
    let nl = if kitty { "Shift+Enter  换行" } else { "Alt+Enter  换行" };
    vec![
        "/help  显示本帮助".into(),
        "/reconnect  只重连观察，不恢复或重新提交".into(),
        "/quit  退出（输入为空时也可按 Ctrl+D）".into(),
        "Esc  清空输入；关闭本面板".into(),
        "Space  关闭警告或错误，移入对话记录".into(),
        "Ctrl+C  取消运行中 Turn；空闲清空输入；两秒内再按退出".into(),
        "Ctrl+↑/↓  输入历史".into(),
        "Enter  提交".into(),
        nl.into(),
    ]
}

#[rustfmt::skip]
/// session 与 agent 由状态行（或侧栏）显示并随接受更新，这里不重复。
fn startup_banner() -> Vec<String> {
    vec![
        "LATTICE".into(),
        "/help  显示命令".into(),
        "/quit  退出".into(),
        "/reconnect  重新读取状态并观察".into(),
    ]
}

fn enable_terminal_modes(writer: &mut impl std::io::Write, kitty: bool) -> std::io::Result<()> {
    crossterm::execute!(
        writer,
        crossterm::cursor::SetCursorStyle::BlinkingBlock,
        EnableBracketedPaste
    )?;
    if kitty {
        crossterm::execute!(
            writer,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    Ok(())
}

fn restore_terminal_modes(writer: &mut impl std::io::Write, kitty: bool) -> std::io::Result<()> {
    // 每项独立尝试；形状恢复失败也不能跳过键盘/粘贴清理。
    let shape = crossterm::execute!(writer, crossterm::cursor::SetCursorStyle::DefaultUserShape);
    let paste = crossterm::execute!(writer, DisableBracketedPaste);
    let keyboard = if kitty {
        crossterm::execute!(writer, PopKeyboardEnhancementFlags)
    } else {
        Ok(())
    };
    shape.and(paste).and(keyboard)
}

#[rustfmt::skip]
fn install_cleanup_panic_hook(kitty: bool) {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore_terminal_modes(&mut std::io::stdout(), kitty);
        prev(info);
    }));
}

struct TerminalCleanup {
    kitty: bool,
}

#[rustfmt::skip]
impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        let _ = restore_terminal_modes(&mut std::io::stdout(), self.kitty);
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
    #[test]
    fn terminal_cleanup_restores_on_normal_error_and_panic_paths() {
        const PROBE: &str = "LATTICE_TUI_CLEANUP_PROBE";
        if let Ok(mode) = std::env::var(PROBE) {
            let run = || -> std::io::Result<()> {
                let _cleanup = TerminalCleanup { kitty: false };
                if mode == "init-error" {
                    ratatui::try_init()?;
                    panic!("closed stdin must reject terminal initialization");
                }
                install_cleanup_panic_hook(false);
                enable_terminal_modes(&mut std::io::stdout(), false)?;
                if mode == "panic" {
                    panic!("cleanup probe panic");
                }
                if mode == "error" {
                    return Err(std::io::Error::other("cleanup probe error"));
                }
                Ok(())
            };
            assert_eq!(run().is_ok(), mode == "normal");
            return;
        }
        for mode in ["normal", "error", "init-error", "panic"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "app::tests::terminal_cleanup_restores_on_normal_error_and_panic_paths",
                    "--nocapture",
                ])
                .env(PROBE, mode)
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            assert_eq!(
                output.status.success(),
                mode != "panic",
                "mode {mode}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let restored = output.stdout.windows(5).position(|s| s == b"\x1b[0 q");
            assert!(restored.is_some(), "{mode} must restore default shape");
            let enabled = output.stdout.windows(5).position(|s| s == b"\x1b[1 q");
            if mode == "init-error" {
                assert!(enabled.is_none());
            } else {
                assert!(enabled.unwrap() < restored.unwrap());
            }
        }
    }

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

    #[test]
    fn r2_history_preserves_node_ids_and_unloaded_suffix_but_not_other_turns() {
        let mut app = test_app();
        app.input.set_text("prompt");
        app.apply_action(KeyAction::Submit);
        let turn_id = app.current_turn.unwrap();
        let key = ModelAttemptKey {
            turn_id,
            model_step_ordinal: 0,
            attempt_ordinal: 1,
        };
        app.apply_turn_event(TurnEvent::TokenDelta {
            key,
            content: "stream".into(),
        });
        let body_id = |app: &App, text: &str| {
            app.blocks
                .entries
                .iter()
                .find(|e| matches!(&e.block, Block::User(s) | Block::Assistant(s) if s == text))
                .unwrap()
                .id
        };
        let user_id = body_id(&app, "prompt");
        let stream_id = body_id(&app, "stream");
        app.push_status(status(NoticeLevel::Info, "notice"));
        let status_id = app
            .blocks
            .entries
            .iter()
            .find(|e| matches!(e.block, Block::Status(_)))
            .unwrap()
            .id;
        let history = |include_second: bool| {
            let mut blocks = vec![
                Block::AgentName("agent".into()),
                Block::UserTurn(turn_id, std::num::NonZeroUsize::MIN),
                Block::User("prompt".into()),
                Block::Step(StepKey {
                    turn_id,
                    ordinal: 0,
                }),
                Block::Assistant("stream saved".into()),
                Block::Tool {
                    id: invocation(1).to_string(),
                    name: "test".into(),
                    state: ToolState::Ok { output_len: 0 },
                    source: ToolSource::HistoryResult,
                },
            ];
            if include_second {
                blocks.extend([
                    Block::Step(StepKey {
                        turn_id,
                        ordinal: 1,
                    }),
                    Block::Assistant("suffix saved".into()),
                ]);
            }
            blocks
        };
        for _ in 0..2 {
            app.replace_history(history(false));
            assert_eq!(
                body_id(&app, "prompt"),
                user_id,
                "AgentName and statuses do not shift node positions"
            );
            assert_eq!(body_id(&app, "stream saved"), stream_id);
            assert!(app.blocks.entries.iter().any(|e| e.id == status_id));
        }
        let later = ModelAttemptKey {
            model_step_ordinal: 1,
            ..key
        };
        app.apply_turn_event(TurnEvent::TokenDelta {
            key: later,
            content: "suffix".into(),
        });
        let suffix_id = body_id(&app, "suffix");
        app.replace_history(history(false));
        assert_eq!(body_id(&app, "stream saved"), stream_id);
        assert_eq!(
            body_id(&app, "suffix"),
            suffix_id,
            "unloaded node retains its id"
        );
        app.replace_history(history(true));
        assert_eq!(body_id(&app, "suffix saved"), suffix_id);
        assert_eq!(body_id(&app, "stream saved"), stream_id);
        // CLI rewind 到根再发送另一轮，文字和位置相同也不是同一节点。
        let other = lattice_api::local::new_turn_id();
        app.replace_history(vec![
            Block::AgentName("agent".into()),
            Block::UserTurn(other, std::num::NonZeroUsize::MIN),
            Block::User("prompt".into()),
            Block::Step(StepKey {
                turn_id: other,
                ordinal: 0,
            }),
            Block::Assistant("stream saved".into()),
        ]);
        assert_ne!(body_id(&app, "prompt"), user_id);
        assert_ne!(body_id(&app, "stream saved"), stream_id);
        assert!(!app.blocks.entries.iter().any(|e| e.id == suffix_id));
        let ids: std::collections::HashSet<_> = app.blocks.entries.iter().map(|e| e.id).collect();
        assert_eq!(ids.len(), app.blocks.len());
    }

    #[test]
    fn existing_session_preserves_input_until_history_is_loaded() {
        let mut app = App::new(
            SessionContext {
                session_id: Some(SessionId::parse("11111111111141118111111111111111").unwrap()),
                active_agent: "agent".into(),
            },
            false,
            Path::new("/nonexistent-tui-test-history"),
        );
        app.input.set_text("kept prompt");
        assert_eq!(app.apply_action(KeyAction::Submit), None);
        assert_eq!(app.input.text(), "kept prompt");
        app.input.set_text("/quit");
        assert_eq!(app.apply_action(KeyAction::Submit), None);
        assert!(app.should_quit);
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
    fn retry(level: NoticeLevel, message: &str) -> TurnEvent {
        TurnEvent::LlmRetry {
            key: attempt(),
            delay_ms: 100,
            notice: lattice_api::local::Notice {
                level,
                text: message.into(),
            },
        }
    }
    fn status(level: NoticeLevel, text: &str) -> Status {
        Status {
            level,
            text: text.into(),
            closing: None,
        }
    }
    fn start_tool(app: &mut App, name: &str) {
        app.apply_turn_event(TurnEvent::ToolCallStarted {
            id: invocation(1),
            name: name.into(),
        });
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
    fn ctrl_c_targets_current_turn_but_esc_only_clears_input() {
        let mut app = test_app();
        let id = lattice_api::local::new_turn_id();
        app.current_turn = Some(id);
        app.phase = TurnPhase::Streaming;
        app.input.set_text("draft");
        handle_input_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );
        assert!(app.input.is_empty());
        assert!(!app.cancel_requested);
        assert_eq!(app.phase, TurnPhase::Streaming);
        handle_input_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        );
        assert!(app.cancel_requested);
        assert_eq!(app.current_turn, Some(id));
        assert_eq!(app.phase, TurnPhase::Cancelling);
        handle_input_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        );
        assert!(app.should_quit && app.cancel_requested);
        assert!(!app.blocks.iter().any(|block| matches!(
            block,
            Block::Status(Status {
                closing: Some(_),
                ..
            })
        )));
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
        app.input.set_text("clear on first Ctrl+C");
        ctrl_c_action(&mut app);
        app.last_ctrl_c = Some(Instant::now() - Duration::from_secs(3));
        assert!(!matches!(ctrl_c_action(&mut app), KeyAction::Quit));
        app.apply_action(KeyAction::CancelOrClear);
        assert!(app.input.is_empty());
        assert!(!app.should_quit);
    }

    #[test]
    fn idle_stream_timeout_keeps_execution_unconfirmed() {
        let mut app = test_app();
        app.phase = TurnPhase::Streaming;
        start_tool(&mut app, "fetch.web");
        app.apply_update(LocalUpdate::Connection(ConnectionStatus::Idle));
        assert_eq!(app.phase, TurnPhase::Streaming);
        // 无增量只是观察中断：一条 Warn，不是错误，也不是 Turn 结局。
        let statuses = app
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Status(status) => Some(status),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            matches!(
                statuses.as_slice(),
                [Status {
                    level: NoticeLevel::Warn,
                    closing: None,
                    ..
                }]
            ),
            "{statuses:?}"
        );
    }

    /// 消息之间不叠：替换（Warn→Warn、Warn→Error、Error→Warn）把旧消息落回对话记录的
    /// 原时间位置，释放后每条恰好出现一次；Info 不进 Dock；草稿不受影响。
    #[test]
    fn dock_replacement_and_release_keep_transcript_order_and_draft() {
        let mut app = test_app();
        app.input.set_text("draft");
        assert!(app.dock.shows_input());
        let docked = |app: &App| {
            app.dock
                .message(&app.blocks)
                .map(|status| (status.level.clone(), status.text.clone()))
        };
        let docked_is = |app: &App, level: NoticeLevel, marker: &str| {
            docked(app).is_some_and(|(known, text)| known == level && text.contains(marker))
        };

        app.apply_turn_event(retry(NoticeLevel::Warn, "retry-0"));
        app.apply_turn_event(retry(NoticeLevel::Warn, "retry-a"));
        assert!(docked_is(&app, NoticeLevel::Warn, "retry-a"));
        let text = transcript_text(&app, 78);
        assert!(text.contains("retry-0") && !text.contains("retry-a"));

        app.push_status(status(NoticeLevel::Info, "info-i"));
        assert!(
            docked_is(&app, NoticeLevel::Warn, "retry-a"),
            "Info 不进 Dock"
        );
        assert!(transcript_text(&app, 78).contains("info-i"));

        app.push_status(status(NoticeLevel::Error, "commit-b"));
        assert!(docked_is(&app, NoticeLevel::Error, "commit-b"));
        assert!(transcript_text(&app, 78).contains("retry-a"));
        app.apply_turn_event(retry(NoticeLevel::Warn, "retry-c"));
        assert!(docked_is(&app, NoticeLevel::Warn, "retry-c"));
        assert!(transcript_text(&app, 78).contains("commit-b"));

        // Space：释放当前消息，Space 不进入输入，草稿不变。
        assert!(app.apply_action(KeyAction::Insert(' ')).is_none());
        assert!(app.dock.shows_input());
        assert_eq!(app.input.text(), "draft");
        assert_eq!(app.phase, TurnPhase::Idle);

        // 每条消息恰好出现一次，且保持到达顺序。
        let lines = transcript_lines(&app, 78)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let order: Vec<usize> = ["retry-0", "retry-a", "info-i", "commit-b", "retry-c"]
            .into_iter()
            .map(|marker| {
                let rows = lines
                    .iter()
                    .enumerate()
                    .filter(|(_, line)| line.contains(marker))
                    .map(|(row, _)| row)
                    .collect::<Vec<_>>();
                assert_eq!(rows.len(), 1, "{marker} 应恰好出现一次");
                rows[0]
            })
            .collect();
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(order, sorted, "保持到达顺序");
    }

    /// 重试提示的等级取自后端的 notice，不由前端写死；外壳为中文，诊断原文不动。
    #[test]
    fn llm_retry_uses_the_backend_notice_level() {
        for (level, docked) in [
            (NoticeLevel::Info, false),
            (NoticeLevel::Warn, true),
            (NoticeLevel::Error, true),
        ] {
            let mut app = test_app();
            app.apply_turn_event(retry(level.clone(), "HTTP 429 rate_limit_exceeded"));
            let pushed = app
                .blocks
                .iter()
                .find_map(|block| match block {
                    Block::Status(status) => Some(status.clone()),
                    _ => None,
                })
                .unwrap();
            assert_eq!(pushed.level, level);
            assert_eq!(
                pushed.text,
                "第 2 次请求失败，100 ms 后重试：HTTP 429 rate_limit_exceeded"
            );
            assert_eq!(
                app.dock.message(&app.blocks),
                docked.then_some(&pushed),
                "{level:?}"
            );
        }
    }

    fn dock_title(app: &mut App) -> &'static str {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| crate::view::draw(frame, app))
            .unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        let shown = ["USER", "HELP", "WARN", "ERROR"]
            .into_iter()
            .filter(|title| screen.contains(&format!("── {title} ─")))
            .collect::<Vec<_>>();
        assert_eq!(shown.len(), 1, "Dock 只显示栈顶：{shown:?}");
        shown[0]
    }

    /// Dock 模块栈的四种状态与两种按键：栈顶是面板时只有 Esc 关闭，栈顶是消息时只有
    /// 无修饰 Space 释放；另一种按键不动栈，也不改不可见的输入。
    #[test]
    fn dock_stack_shows_the_top_module_and_routes_space_and_esc() {
        #[derive(Debug)]
        enum Step {
            Help,
            Notice(NoticeLevel),
            Space,
            Esc,
        }
        use Step::*;
        let cases: [(&str, Vec<(Step, &str)>); 4] = [
            ("输入区", vec![(Notice(NoticeLevel::Info), "USER")]),
            (
                "输入区、面板",
                vec![(Help, "HELP"), (Space, "HELP"), (Esc, "USER")],
            ),
            (
                "输入区、消息",
                vec![
                    (Notice(NoticeLevel::Warn), "WARN"),
                    (Esc, "WARN"),
                    (Notice(NoticeLevel::Error), "ERROR"),
                    (Esc, "ERROR"),
                    (Space, "USER"),
                ],
            ),
            (
                "输入区、面板、消息",
                vec![
                    (Help, "HELP"),
                    (Notice(NoticeLevel::Warn), "WARN"),
                    (Esc, "WARN"),
                    (Space, "HELP"),
                    (Space, "HELP"),
                    (Esc, "USER"),
                ],
            ),
        ];
        let press = |app: &mut App, code: KeyCode| {
            handle_input_event(app, Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
        };
        for (name, steps) in cases {
            let mut app = test_app();
            app.input.set_text("draft");
            assert_eq!(dock_title(&mut app), "USER", "{name}");
            for (index, (step, top)) in steps.into_iter().enumerate() {
                let label = format!("{name} 第 {index} 步 {step:?}");
                let draft = app.input.text();
                // `/help` 作为本地指令清空输入；其余步骤都不得改动输入缓冲。
                let input = match step {
                    Help => {
                        app.input.set_text("/help");
                        assert!(app.apply_action(KeyAction::Submit).is_none());
                        String::new()
                    }
                    Notice(level) => {
                        app.push_status(status(level, "status-marker"));
                        draft
                    }
                    Space => {
                        press(&mut app, KeyCode::Char(' '));
                        draft
                    }
                    Esc => {
                        press(&mut app, KeyCode::Esc);
                        draft
                    }
                };
                assert_eq!(app.input.text(), input, "{label}");
                assert_eq!(dock_title(&mut app), top, "{label}");
                assert_eq!(app.dock.shows_input(), top == "USER", "{label}");
                assert_eq!(app.focus, Focus::Dock, "{label}");
            }
            // 面板不是状态消息：关闭后对话记录里没有帮助文字。
            assert!(!transcript_text(&app, 78).contains("/reconnect"), "{name}");
            // 回到输入区后恢复普通编辑。
            let draft = app.input.text();
            press(&mut app, KeyCode::Char('x'));
            assert_eq!(app.input.text(), format!("{draft}x"), "{name}");
        }
    }

    /// 启动横幅固定在对话记录最上方：History 装入与替换都不把它当作状态消息重新排位。
    #[test]
    fn startup_banner_stays_on_top_across_history_load_and_replacement() {
        let mut app = test_app();
        app.show_banner();
        app.push_status(status(NoticeLevel::Info, "before-history"));
        let turn = |n: u32| TurnId::parse(&format!("33333333-3333-4333-8333-{n:012}")).unwrap();
        let history = |turns: u32| {
            (1..=turns)
                .flat_map(|n| {
                    [
                        Block::AgentName("agent".into()),
                        Block::UserTurn(turn(n), std::num::NonZeroUsize::new(n as usize).unwrap()),
                        Block::User(format!("prompt-{n}")),
                        Block::Step(StepKey {
                            turn_id: turn(n),
                            ordinal: 0,
                        }),
                        Block::Assistant(format!("answer-{n}")),
                    ]
                })
                .collect::<Vec<_>>()
        };
        for turns in [1, 2, 1] {
            app.replace_history(history(turns));
            let lines = transcript_lines(&app, 78)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            assert_eq!(
                lines.iter().take(4).map(String::as_str).collect::<Vec<_>>(),
                [
                    "LATTICE",
                    "/help  显示命令",
                    "/quit  退出",
                    "/reconnect  重新读取状态并观察"
                ],
                "{turns}"
            );
            assert_eq!(
                lines.iter().filter(|line| line.contains("LATTICE")).count(),
                1
            );
            // 它之后没有节点的状态消息留在末尾；横幅若仍是状态消息，会与它一起落到历史之后。
            let row = |marker: &str| lines.iter().position(|line| line.contains(marker)).unwrap();
            assert!(row("prompt-1") < row("before-history"), "{lines:?}");
        }
        assert_eq!(
            app.blocks
                .iter()
                .filter(|block| matches!(block, Block::Status(_)))
                .count(),
            1,
            "横幅不在状态消息之列"
        );
    }

    /// §14.3：WARN / ERROR 下屏蔽不可见输入编辑，全局动作照常。
    #[test]
    fn dock_status_blocks_invisible_input_but_keeps_globals() {
        for level in [NoticeLevel::Warn, NoticeLevel::Error] {
            let mut app = test_app();
            app.input.set_text("draft");
            app.push_status(status(level, "blocked"));
            for code in [
                KeyCode::Char('x'),
                KeyCode::Esc,
                KeyCode::Enter,
                KeyCode::Backspace,
                KeyCode::Delete,
                KeyCode::Left,
                KeyCode::Right,
                KeyCode::Up,
                KeyCode::Down,
                KeyCode::Home,
                KeyCode::End,
            ] {
                assert!(
                    handle_input_event(
                        &mut app,
                        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
                    )
                    .is_none()
                );
            }
            for modifiers in [
                KeyModifiers::SHIFT,
                KeyModifiers::ALT,
                KeyModifiers::CONTROL,
            ] {
                handle_input_event(
                    &mut app,
                    Event::Key(KeyEvent::new(KeyCode::Char(' '), modifiers)),
                );
            }
            handle_input_event(
                &mut app,
                Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL)),
            );
            handle_input_event(
                &mut app,
                Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL)),
            );
            handle_input_event(&mut app, Event::Paste("hidden\npaste".into()));
            assert_eq!(app.input.text(), "draft");
            assert_eq!(app.input.cursor, (0, 5));
            assert!(app.dock.message_id().is_some());
            handle_input_event(
                &mut app,
                Event::Key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)),
            );
            assert!(!app.viewport.following());
            handle_input_event(
                &mut app,
                Event::Key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL)),
            );
            assert!(app.viewport.following());
            handle_input_event(
                &mut app,
                Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            );
            assert_eq!(
                app.input.text(),
                "draft",
                "空闲 Warn/Error 下 Ctrl+C 不清空草稿"
            );
            assert!(app.last_ctrl_c.is_some() && !app.should_quit);
            handle_input_event(
                &mut app,
                Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            );
            assert!(app.should_quit);
            app.should_quit = false;
            app.last_ctrl_c = None;
            app.current_turn = Some(lattice_api::local::new_turn_id());
            app.phase = TurnPhase::Streaming;
            handle_input_event(
                &mut app,
                Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            );
            assert!(app.cancel_requested);
            assert_eq!(app.input.text(), "draft");
            handle_input_event(
                &mut app,
                Event::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
            );
            assert!(app.dock.shows_input());
            assert_eq!(app.input.text(), "draft", "释放键被消费");
            handle_input_event(
                &mut app,
                Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            );
            assert_eq!(app.input.text(), "draftx", "释放后恢复普通字符编辑");
            handle_input_event(
                &mut app,
                Event::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
            );
            assert_eq!(app.input.text(), "draftx ");
        }
    }

    fn transcript_text(app: &App, width: u16) -> String {
        transcript_lines(app, width)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }
}
