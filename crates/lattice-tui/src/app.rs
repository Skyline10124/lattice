// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::TuiOptions;
use crate::blocks::{BlockId, BlockList, IdentifiedBlock};
use crate::clipboard::{ClipboardError, Content, Staging};
use crate::connection::{
    KeptInput, Viewed, cancellation_status, clipboard_failed_status, clipboard_stage_failed_status,
    configure_failed_status, detach_lines, effort_changed_status, execution_diagnostic_status,
    handoff_status, model_query_failed_status, model_query_notice, not_accepted_status,
    not_submitted_status, observation_lost_status, older_turns_unavailable_status,
    orphaned_turn_status, paste_image_failed_status, preflight_failed_status, recovered_status,
    rejected_status, submission_unknown_status, submit_busy_status, submit_configuring_status,
};
use crate::input::{
    CTRL_C_WINDOW, History, ImageSource, InputBuffer, KeyAction, PendingImage, resolve_key,
};
use crate::session::{Generating, SessionContext};
use crate::view::{
    Dock, DockIntent, DockState, Focus, ModelQuery, Panel, PanelChoice, Transcript, Viewport,
    render_transcript,
};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use futures::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use lattice_local::{
    CancelStatus, Channel, ConfigureReply, ConfigureRequest, ConfigureSession, ConnectionStatus,
    ContextOp, ConversationEntry, DetachReport, LocalClient, LocalError, LocalHistory, LocalTurn,
    LocalUpdate, NodeId, NoticeLevel, PlannedTurnRequest, PublicTurnOutcome, SessionHead,
    SessionId, SettingsVersion, StepKey, SubmissionReceipt, SubmissionResult, SubmissionUpdate,
    TextSegment, TextUpdate, TurnId, TurnInputItem, TurnRequest, UserInputPart, ViewUpdate,
};
use lattice_local::{Effort, ModelCandidateQuery, ModelRef, UsageSummary};
#[cfg(test)]
use lattice_local::{ModelId, ProviderId};
use tokio::time::Instant;

const FRAME: Duration = Duration::from_millis(33);

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Block {
    /// 一条持久化的用户输入条目锚点，不占视觉行。
    Input {
        node: NodeId,
        channel: Channel,
    },
    /// 上下文操作条目，渲染来源通道与逐条编辑摘要。
    ContextOp {
        node: NodeId,
        op: Box<ContextOp>,
    },
    /// Step 条目的开头，不占行。
    Step {
        node: NodeId,
        key: StepKey,
        agent: Option<String>,
    },
    User(String),
    /// `tui/comment` 通道内自包含评论列表，原地渲染评论卡片。
    SentComments(Vec<crate::comment::SentComment>),
    HistoryAssistant(crate::comment::HistoryText),
    /// 生成中的文字，或由相邻多段拼成、不能评论的回复正文。
    Assistant(String),
    Tool {
        id: String,
        name: String,
        state: ToolState,
    },
    Status(Status),
}

/// 状态消息只描述自己；是否暂驻 Dock 由 Dock 按等级决定，消息上没有位置字段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Status {
    pub(crate) level: NoticeLevel,
    /// 可多行，以换行分隔。
    pub(crate) text: String,
    /// 仅 Turn 结局携带所属 Turn 与结局：它属于这一轮最后一个 Step 条目，并决定符号与配色。
    pub(crate) closing: Option<(TurnId, PublicTurnOutcome)>,
    /// 仅重试提示携带它所属的尝试：它属于这个 Step 条目。
    pub(crate) retry: Option<lattice_local::ModelAttemptKey>,
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

/// 首次 Ctrl+C 取消的那一轮。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CancelTarget {
    /// 本客户端的提交，经它的 LocalTurn 登记取消意图；尚未发出时不再发出。
    Own,
    /// 其他客户端提交、会话头所示正在执行的一轮，按 TurnId 直接取消。
    Other(TurnId),
}

/// 往上翻到已装入的最早一轮时取更早的轮；`epoch` 是发出时的视图代次。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OlderRequest {
    pub(crate) session_id: SessionId,
    pub(crate) before: NodeId,
    pub(crate) epoch: u64,
}

/// 由 `/model` 或 `/effort` 触发、交由应用循环异步发出的会话设置修改请求。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingConfigure {
    pub(crate) generation: u64,
    pub(crate) model: Option<ModelRef>,
    pub(crate) effort: Option<Effort>,
}

/// 已被后台接受的本端轮次因果水位：用于判定完整确认的视图快照是否必然生成于该次接受之后。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AcceptedTurnWatermark {
    turn_id: TurnId,
    start_node: NodeId,
    accepted_view_gen: u64,
}

pub(crate) struct App {
    pub(crate) session_id: SessionId,
    project_root: PathBuf,
    pub(crate) blocks: BlockList,
    /// 启动横幅固定在对话记录最上方。它不是状态消息，也不在 `blocks` 中，
    /// 条目替换与 Dock 都碰不到它；身份只供浏览光标锚定。
    pub(crate) banner: Option<(BlockId, Vec<String>)>,
    pub(crate) markdown_cache: std::cell::RefCell<crate::markdown::RenderCache>,
    pub(crate) focus: Focus,
    pub(crate) dock: DockState,
    pub(crate) input: InputBuffer,
    pub(crate) drafts: crate::drafts::Drafts,
    pub(crate) history: History,
    pub(crate) viewport: Viewport,
    pub(crate) phase: TurnPhase,
    pub(crate) connected: bool,
    query_generation: u64,
    model_query: Option<ModelQuery>,
    configure_generation: u64,
    pub(crate) configure_in_flight: BTreeSet<u64>,
    pub(crate) configure_request: Option<PendingConfigure>,
    pub(crate) turn_usage: Option<UsageSummary>,
    pub(crate) context_tokens: Option<u64>,
    /// 由应用循环取走并执行的请求。
    pub(crate) cancel_requested: Option<CancelTarget>,
    pub(crate) view_wanted: bool,
    pub(crate) older_request: Option<OlderRequest>,
    clipboard_request: bool,
    staging: Option<Staging>,
    cancelling: bool,
    /// 最新收到的会话头（来自启动读取/configure、运行中 configure 回复或视图推送），
    /// 供状态行、选择面板和 expected_settings 读取。
    latest_head: SessionHead,
    /// 已确认的会话头：它当前位置的条目已经到达。各轮是否结束与正在执行的一轮只按它判定；
    /// 视图开始前为空。
    pub(crate) head: Option<SessionHead>,
    /// 刚到达、当前位置的条目尚未到达的会话头。后台在同一次推送里紧接着给出那个条目，
    /// 收到之前不拿新会话头配旧条目。
    pending_head: Option<SessionHead>,
    /// 正在显示的条目，按 NodeId 维护；块序列中的段由它们生成。
    entries: BTreeMap<NodeId, ConversationEntry>,
    /// 当前窗口之前是否还有更早历史；来自会话头 `has_older` 与 `history` 回复 `has_more`。
    pub(crate) has_older: bool,
    /// `replace_pending` 期间暂存的新视图 `has_older`，待真正替换条目时落入 `has_older`。
    pending_has_older: Option<bool>,
    /// 已被后台接受但视图尚未收到本轮任何 `Step` 的轮次身份，用于派生等待对话同步提示。
    pub(crate) pending_accepted: Option<TurnId>,
    /// 生成中的文字，按所属 Step 节点。
    texts: HashMap<NodeId, Generating>,
    /// 已经结束的轮；由未结束变为结束的那一刻，结局才是新到的消息。
    ended: HashSet<TurnId>,
    /// 新视图开始后、首条会话头到达前为真：那条会话头之后是初始条目。
    view_starting: bool,
    /// `begin_view()` 递增的重连视图代数；仅新建 `watch` 视图递增，路径替换不递增。
    next_view_gen: u64,
    /// 当前活跃 `watch` 视图对应的代数；新视图首条会话头到达时更新为 `next_view_gen`。
    active_view_gen: u64,
    /// 当前活跃 `watch` 流上已观察到的最大节点号；新视图首条会话头到达时重置，仅随会话头与条目推进。
    watch_max_node: Option<NodeId>,
    /// 新视图的首条会话头已到、首个初始条目未到：重连失败时已显示的内容保留，首个条目到达才替换。
    replace_pending: bool,
    /// 新视图或路径替换的首条会话头到它当前位置的条目为止：其间的条目是初始条目，
    /// 带出的提示与结局不是新消息。
    initial: bool,
    /// 视图代次：新视图开始或路径替换时递增，迟到的历史查询结果据此丢弃。
    epoch: u64,
    older_pending: bool,
    orphan_warned: Option<TurnId>,
    diagnostic: Option<LocalError>,
    /// 本客户端尚未得到结果的提交。
    submitted: Option<(TurnId, crate::drafts::Outgoing)>,
    /// 本客户端最近一次提交；会话头所示正在执行的是它时，取消经它的 LocalTurn。
    own_turn: Option<TurnId>,
    /// 已被接受、会话头尚未指向它的那一轮：其间仍按正在运行处理。
    awaiting_head: Option<TurnId>,
    /// 本端已确认接受轮次的因果水位；用于判定后续已确认视图是否生成于本次接受之后。
    accepted_turn: Option<AcceptedTurnWatermark>,
    /// 结果未知的那次提交，按 TurnId 留到收到它的确定结果为止。
    unknown_submission: Option<(TurnId, crate::drafts::Outgoing)>,
    /// 已实时显示的提交回执：请求与结果种类。交接报告只省略相同的事实。
    shown_receipts: HashSet<(TurnId, std::mem::Discriminant<SubmissionResult>)>,
    pub(crate) cancel_status: Option<CancelStatus>,
    /// Set when `supports_keyboard_enhancement()` reported true at startup.
    pub(crate) kitty_keyboard: bool,
    /// Instant of the last Ctrl+C, for the double-press exit rule.
    pub(crate) last_ctrl_c: Option<Instant>,
    pub(crate) should_quit: bool,
    pub(crate) term_width: u16,
    pub(crate) term_height: u16,
}

impl App {
    pub(crate) fn new(
        ctx: SessionContext,
        kitty_keyboard: bool,
        session_dir: &Path,
        project_root: PathBuf,
    ) -> Self {
        let SessionContext {
            session_id,
            initial_head,
            initial_adjustment,
        } = ctx;
        let mut app = Self {
            view_wanted: true,
            query_generation: 0,
            model_query: None,
            configure_generation: 0,
            configure_in_flight: BTreeSet::new(),
            configure_request: None,
            session_id,
            project_root,
            blocks: BlockList::default(),
            banner: None,
            markdown_cache: Default::default(),
            focus: Focus::Dock,
            dock: DockState::default(),
            input: InputBuffer::new(),
            drafts: Default::default(),
            history: History::load(session_dir),
            viewport: Viewport::new(),
            phase: TurnPhase::Idle,
            connected: true,
            turn_usage: None,
            context_tokens: None,
            cancel_requested: None,
            older_request: None,
            clipboard_request: false,
            staging: None,
            cancelling: false,
            latest_head: initial_head.clone(),
            head: None,
            pending_head: None,
            entries: BTreeMap::new(),
            has_older: initial_head.has_older.unwrap_or(false),
            pending_has_older: None,
            pending_accepted: None,
            texts: HashMap::new(),
            ended: HashSet::new(),
            view_starting: false,
            next_view_gen: 0,
            active_view_gen: 0,
            watch_max_node: None,
            replace_pending: false,
            initial: false,
            epoch: 0,
            older_pending: false,
            orphan_warned: None,
            diagnostic: None,
            submitted: None,
            own_turn: None,
            awaiting_head: None,
            accepted_turn: None,
            unknown_submission: None,
            shown_receipts: Default::default(),
            cancel_status: None,
            kitty_keyboard,
            last_ctrl_c: None,
            should_quit: false,
            term_width: 80,
            term_height: 24,
        };
        if let Some(adjustment) = initial_adjustment {
            app.push_status(effort_changed_status(adjustment.from, adjustment.to));
        }
        app
    }

    /// 保留键先执行；其余动作交给 Dock 栈顶解释，模块不接触 LocalTurn。
    pub(crate) fn apply_action(&mut self, action: KeyAction) -> Option<TurnRequest> {
        let width = self.term_width;
        let transcript = self.prepare_transcript();
        let lines = &transcript.lines;
        match action {
            KeyAction::ScrollPageUp | KeyAction::ScrollPageDown | KeyAction::ScrollToBottom => {
                let height = self.transcript_height(lines.len());
                if width == 0 || height == 0 {
                    if !self.drafts.is_editing() {
                        self.focus_input(lines.len());
                    }
                    return None;
                }
                match action {
                    KeyAction::ScrollPageUp => {
                        self.viewport.page_up(height);
                        self.request_older_at_top();
                    }
                    KeyAction::ScrollPageDown => self.viewport.page_down(lines.len(), height),
                    _ => self.viewport.to_bottom(lines.len(), height),
                }
                if let Focus::Transcript(cursor) = &mut self.focus {
                    if !matches!(action, KeyAction::ScrollToBottom) {
                        self.viewport.pause();
                    }
                    if !self.drafts.is_editing() {
                        self.viewport.clamp_cursor(cursor);
                        cursor.reanchor(&transcript);
                    }
                }
            }
            KeyAction::CancelOrClear => self.cancel_or_clear(),
            KeyAction::Quit => self.should_quit = true,
            action if self.drafts.is_editing() => {
                match action {
                    KeyAction::Submit | KeyAction::Clear => {
                        if let Some(finished) =
                            self.drafts.finish_edit(matches!(action, KeyAction::Submit))
                        {
                            self.focus_after_draft_change(&finished.location, finished.saved_id);
                        }
                    }
                    action => self.drafts.edit(&action, transcript.editor_width(width)),
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
                        match transcript.comment_navigation(cursor) {
                            Some(crate::view::CommentNavigation::Compose(anchor)) => {
                                let id = self.blocks.allocate_id();
                                self.drafts.start_comment(id, anchor);
                            }
                            None => {
                                self.drafts.start_edit(cursor.block.id);
                            }
                        }
                    }
                    let transcript = self.prepare_transcript();
                    if let Some((row, _)) = transcript.editor_cursor {
                        self.viewport.reveal(row);
                    }
                }
                // 不在编辑时 Esc 只回到输入区；删除草稿用 Delete，避免误删。
                KeyAction::Clear => self.focus_input(lines.len()),
                KeyAction::Delete => {
                    if let Focus::Transcript(cursor) = self.focus
                        && let Some(anchor) = self.drafts.remove_draft(cursor.block.id)
                    {
                        self.focus_after_draft_change(&anchor, None);
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
                            if matches!(action, KeyAction::Up) {
                                self.request_older_at_top();
                            }
                        }
                    }
                }
                _ => {}
            },
            action => match Dock::new(self).route(action) {
                DockIntent::Submit => return self.submit(),
                DockIntent::ReleaseStatus => self.dock.release(),
                DockIntent::ClosePanel => self.dock.close_panel(),
                DockIntent::EditPanel(action) => self.dock.edit_panel(action),
                DockIntent::Choose(choice) => {
                    let (model, effort) = match choice {
                        PanelChoice::Model(model) => {
                            (Some(ModelRef::new(model.provider, model.model)), None)
                        }
                        PanelChoice::Effort(effort) => (None, Some(effort)),
                    };
                    self.configure_generation = self.configure_generation.saturating_add(1);
                    let generation = self.configure_generation;
                    self.configure_in_flight.insert(generation);
                    self.configure_request = Some(PendingConfigure {
                        generation,
                        model,
                        effort,
                    });
                    self.dock.close_panel();
                }
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
                    KeyAction::Paste(raw) => {
                        if let Err(error) = self.input.paste(&raw) {
                            self.push_status(paste_image_failed_status(&error));
                        }
                    }
                    KeyAction::ClipboardPaste => {
                        self.clipboard_request = true;
                    }
                    action => self.input.edit(&action, width),
                },
            },
        }
        None
    }

    /// 已结束一轮的回复才可评论。
    pub(crate) fn turn_closed(&self, turn: TurnId) -> bool {
        self.ended.contains(&turn)
    }

    fn focus_after_draft_change(
        &mut self,
        location: &crate::comment::Location,
        saved_id: Option<BlockId>,
    ) {
        let width = self.term_width;
        let transcript = render_transcript(self, width);
        self.viewport.on_content_changed(
            transcript.lines.len(),
            self.transcript_height(transcript.lines.len()),
        );
        if let Some(cursor) = transcript.anchor_cursor(location) {
            self.focus = Focus::Transcript(cursor);
            self.viewport.pause();
            self.viewport.reveal(cursor.row);
            return;
        }
        if let Some(id) = saved_id
            && let Some(cursor) = transcript.block_cursor(id)
        {
            self.focus = Focus::Transcript(cursor);
            self.viewport.pause();
            self.viewport.reveal(cursor.row);
            return;
        }
        if transcript.lines.is_empty() {
            self.focus_input(0);
            return;
        }
        if let Focus::Transcript(mut cursor) = self.focus {
            cursor.row = cursor.row.min(transcript.lines.len().saturating_sub(1));
            transcript.relocate(&mut cursor);
            cursor.reanchor(&transcript);
            self.focus = Focus::Transcript(cursor);
            self.viewport.pause();
            self.viewport.reveal(cursor.row);
        } else if let Some(cursor) = transcript.cursor(transcript.lines.len().saturating_sub(1), 0)
        {
            self.focus = Focus::Transcript(cursor);
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
            "/reconnect" => { self.view_wanted = true; true }
            "/help" | "/?" => { self.dock.open_panel(Panel::Help(help_lines(self.kitty_keyboard))); true }
            "/model" => { self.query_generation += 1; self.dock.open_panel(Panel::models(self.query_generation, self.current_target().map(|target| target.to_string()))); self.model_query = self.dock.query(); true }
            "/effort" => { self.query_generation += 1; self.dock.open_panel(Panel::efforts(self.query_generation, self.current_target(), self.current_effort())); self.model_query = self.dock.query(); true }
            _ => false,
        }
    }

    pub(crate) fn current_head(&self) -> &SessionHead {
        &self.latest_head
    }

    pub(crate) fn current_effort(&self) -> Option<Effort> {
        self.current_head().effort
    }

    pub(crate) fn current_target(&self) -> Option<ModelRef> {
        let head = self.current_head();
        Some(ModelRef::new(head.provider.clone()?, head.model.clone()?))
    }

    #[cfg(test)]
    pub(crate) fn current_agent(&self) -> String {
        self.current_head().agent.clone()
    }

    fn current_settings_version(&self) -> lattice_local::SettingsVersion {
        self.current_head().settings_version
    }

    /// 返回用户当前在屏幕上实际看到的历史位置：
    /// 重连后处于 `replace_pending`（新视图首条 `Head` 已到、首个初始条目未到）时屏幕仍展示旧视图 `self.head`，
    /// 必须携带旧位置以防外部 `rewind` 后被静默挂到回退后的节点。
    fn current_expected_position(&self) -> NodeId {
        if self.replace_pending {
            self.head.as_ref().unwrap_or(&self.latest_head).position
        } else {
            self.latest_head.position
        }
    }

    fn sync_head_panels(&mut self) {
        let target = self.current_target();
        let effort = self.current_effort();
        self.dock
            .update_current(target.as_ref().map(ToString::to_string), effort);
        if self
            .dock
            .effort_target()
            .is_some_and(|previous| previous != target.as_ref())
        {
            self.query_generation += 1;
            self.dock
                .retarget_effort(self.query_generation, target, effort);
            self.model_query = self.dock.query();
        }
    }

    pub(crate) fn apply_configure(
        &mut self,
        generation: u64,
        result: lattice_local::Result<ConfigureReply>,
    ) {
        self.configure_in_flight.remove(&generation);
        if generation != self.configure_generation {
            return;
        }
        match result {
            Ok(reply) => {
                self.latest_head = reply.head;
                self.sync_head_panels();
                if let Some(adjustment) = reply.adjustment {
                    self.push_status(effort_changed_status(adjustment.from, adjustment.to));
                }
            }
            Err(error) => {
                self.push_status(configure_failed_status(&error));
            }
        }
    }

    fn apply_models(
        &mut self,
        request: ModelQuery,
        result: lattice_local::Result<lattice_local::ModelCandidates>,
    ) {
        // 关闭、重开或目标改变后，成功和失败都只属于旧面板。
        if self.dock.query().as_ref() != Some(&request) {
            return;
        }
        match result {
            Ok(result) => {
                for warning in &result.warnings {
                    self.push_status(model_query_notice(&warning.message));
                }
                let current = match &request.query {
                    ModelCandidateQuery::Target(_) => {
                        self.current_effort().map(|effort| effort.to_string())
                    }
                    ModelCandidateQuery::List { .. } => {
                        self.current_target().map(|target| target.to_string())
                    }
                };
                self.dock.populate(result, current);
            }
            Err(error) => {
                self.dock.close_panel();
                self.push_status(model_query_failed_status(&error.message));
            }
        }
    }

    pub(crate) fn prepare_transcript(&mut self) -> Transcript {
        let width = self.term_width;
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
            && !self.drafts.is_editing()
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
        // 旧句柄的交接报告只生成状态消息，不覆盖新 Turn 的状态或归还输入。
        if let Some(id) = report.turn_id {
            match report.submission.as_ref().map(|receipt| &receipt.result) {
                Some(SubmissionResult::Accepted { turn }) => {
                    self.record_accepted_submission(id, turn.start_node);
                    self.reconcile_confirmed_view();
                }
                Some(SubmissionResult::Rejected { .. } | SubmissionResult::NotAccepted) => {
                    self.resolve_unknown(id);
                }
                _ => {}
            }
        }
        // 已由本客户端实时显示的同一回执与结局不再重述；未知之后的确定结果、取消与诊断照常显示。
        let mut report = report;
        if report.submission.as_ref().is_some_and(|receipt| {
            self.shown_receipts
                .contains(&(receipt.turn_id, std::mem::discriminant(&receipt.result)))
        }) {
            report.submission = None;
        }
        let viewed = if report.turn_id.is_some_and(|id| self.summary_shown(id)) {
            Viewed::ClosingShown
        } else {
            Viewed::Unknown
        };
        for line in detach_lines(report, viewed) {
            self.notice_once(handoff_status(&line));
        }
        self.refresh_phase();
    }

    /// 会话视图的推送。
    pub(crate) fn apply_view(&mut self, update: LocalUpdate) {
        match update {
            LocalUpdate::Head(head) => self.apply_head(head),
            LocalUpdate::Entry { entry } => self.apply_entry(*entry),
            LocalUpdate::Text(text) => self.apply_text(text),
            LocalUpdate::Connection { status } => self.connection_lost(&status),
            // 视图出错后不再推送，与连接出错一样是观察中断。
            LocalUpdate::Error { error } => self.connection_lost(&ConnectionStatus::Error(error)),
            // 只读视图既不提交也不取消。
            LocalUpdate::Submit { .. }
            | LocalUpdate::Configure { .. }
            | LocalUpdate::Cancel { .. } => {}
        }
        self.settle(true);
        if self.initial && self.pending_head.is_none() {
            // 起始会话头当前位置的条目已经到达，初始条目给完。
            self.initial = false;
        }
        self.reconcile_confirmed_view();
        self.refresh_status_fields();
        self.refresh_phase();
    }

    /// 本客户端提交句柄的推送。
    pub(crate) fn apply_submission(&mut self, update: LocalUpdate) {
        match update {
            LocalUpdate::Submit {
                update: SubmissionUpdate::Receipt(receipt),
            } => self.apply_receipt(receipt),
            LocalUpdate::Submit {
                update: SubmissionUpdate::Sending { .. },
            } => {}
            LocalUpdate::Cancel {
                turn_id,
                status,
                error,
                ..
            } => {
                self.cancel_status = Some(status);
                self.push_status(cancellation_status(turn_id, status, error.as_ref()));
            }
            // 提交句柄不开视图，不推会话内容；会话内容只取自会话视图。
            LocalUpdate::Head(_)
            | LocalUpdate::Entry { .. }
            | LocalUpdate::Text(_)
            | LocalUpdate::Configure { .. }
            | LocalUpdate::Connection { .. }
            | LocalUpdate::Error { .. } => {}
        }
        self.refresh_phase();
    }

    fn apply_receipt(&mut self, receipt: SubmissionReceipt) {
        let turn_id = receipt.turn_id;
        self.shown_receipts
            .insert((turn_id, std::mem::discriminant(&receipt.result)));
        match receipt.result {
            SubmissionResult::Accepted { turn } => {
                self.record_accepted_submission(turn_id, turn.start_node);
                self.record_accepted_turn(turn_id, turn.start_node);
                if !self.has_turn_step(turn_id) {
                    self.pending_accepted = Some(turn_id);
                }
                // 已确认的会话头到达这一轮之前，仍按正在运行处理。
                if !self.head_reached(turn_id) {
                    self.awaiting_head = Some(turn_id);
                }
                self.reconcile_confirmed_view();
                if receipt.recovered {
                    self.notice_once(recovered_status(turn_id));
                }
            }
            SubmissionResult::Rejected { error } => {
                if self.pending_accepted == Some(turn_id) {
                    self.pending_accepted = None;
                }
                if self
                    .accepted_turn
                    .is_some_and(|watermark| watermark.turn_id == turn_id)
                {
                    self.accepted_turn = None;
                }
                let kept = self.reject_submission(turn_id);
                self.push_status(rejected_status(turn_id, &error, kept));
            }
            // 不来自查询的未被接受是取消发生在请求写出之前，这次提交没有发出。
            SubmissionResult::NotAccepted => {
                if self.pending_accepted == Some(turn_id) {
                    self.pending_accepted = None;
                }
                if self
                    .accepted_turn
                    .is_some_and(|watermark| watermark.turn_id == turn_id)
                {
                    self.accepted_turn = None;
                }
                let kept = self.reject_submission(turn_id);
                self.push_status(if receipt.recovered {
                    not_accepted_status(turn_id, kept)
                } else {
                    not_submitted_status(turn_id, kept)
                });
            }
            SubmissionResult::Unknown { error } => {
                // 不还原已提交的输入，避免重复提交。
                if let Some((id, message)) = self.submitted.take_if(|(id, _)| *id == turn_id) {
                    self.unknown_submission = Some((id, message));
                }
                if self.pending_accepted == Some(turn_id) {
                    self.pending_accepted = None;
                }
                if self
                    .accepted_turn
                    .is_some_and(|watermark| watermark.turn_id == turn_id)
                {
                    self.accepted_turn = None;
                }
                self.push_status(submission_unknown_status(turn_id, error.as_ref()));
            }
        }
        self.refresh_status_fields();
    }

    /// 其他客户端提交的一轮的取消回执。
    pub(crate) fn apply_cancel_result(
        &mut self,
        turn_id: TurnId,
        result: lattice_local::Result<bool>,
    ) {
        let (status, error) = match result {
            Ok(true) => (CancelStatus::Taken, None),
            Ok(false) => (CancelStatus::NotTaken, None),
            Err(error) => (CancelStatus::Unknown, Some(error)),
        };
        self.cancel_status = Some(status);
        self.push_status(cancellation_status(turn_id, status, error.as_ref()));
        self.refresh_phase();
    }

    /// 应用循环开始了新的视图：递增重连视图代数，它的首条会话头之后是初始条目。
    pub(crate) fn begin_view(&mut self) {
        self.next_view_gen = self.next_view_gen.saturating_add(1);
        self.view_starting = true;
    }

    pub(crate) fn has_turn_step(&self, turn: TurnId) -> bool {
        self.entries
            .values()
            .any(|entry| matches!(entry, ConversationEntry::Step { step, .. } if step.key.turn_id == turn))
    }

    pub(crate) fn submission_wait_text(&self) -> Option<&'static str> {
        if self.submitted.is_some() {
            Some("正在提交请求…")
        } else if self
            .pending_accepted
            .is_some_and(|turn| !self.has_turn_step(turn))
        {
            Some("请求已接受，等待对话同步…")
        } else {
            None
        }
    }

    fn last_summary(&self, turn: TurnId) -> Option<&lattice_local::TurnSummary> {
        self.entries.values().rev().find_map(|entry| match entry {
            ConversationEntry::Step {
                step,
                summary: Some(summary),
                ..
            } if step.key.turn_id == turn => Some(summary),
            _ => None,
        })
    }

    /// 会话头所示正在执行的那一轮：当前位置那一轮有 Owner 且尚未结束。
    fn running_turn(&self) -> Option<TurnId> {
        let head = self.head.as_ref()?;
        head.turn_id
            .filter(|turn| head.owner && !self.turn_ended(*turn))
    }

    /// 正在运行：本客户端的提交尚未得到结果，或会话头说明当前位置那一轮有 Owner 且尚未结束。
    fn refresh_phase(&mut self) {
        let running = self.submitted.is_some()
            || self.awaiting_head.is_some()
            || self.running_turn().is_some();
        if !running {
            self.cancelling = false;
        }
        self.phase = match (running, self.cancelling) {
            (false, _) => TurnPhase::Idle,
            (true, false) => TurnPhase::Streaming,
            (true, true) => TurnPhase::Cancelling,
        };
    }

    fn cancel_target(&self) -> Option<CancelTarget> {
        if self.submitted.is_some() {
            return Some(CancelTarget::Own);
        }
        let running = self.running_turn().or(self.awaiting_head)?;
        Some(if Some(running) == self.own_turn {
            CancelTarget::Own
        } else {
            CancelTarget::Other(running)
        })
    }

    /// 新视图的初始条目到达或路径替换时丢弃此前的条目及其生成中的文字；状态消息保留。
    fn discard_entries(&mut self) {
        self.entries.clear();
        self.texts.clear();
        self.ended.clear();
        let mut remove = vec![false; self.blocks.len()];
        let mut index = 0;
        while index < self.blocks.len() {
            let range = self.blocks.group(index);
            let discard = matches!(
                &self.blocks.entries[index].block,
                Block::Input { .. } | Block::ContextOp { .. } | Block::Step { .. }
            );
            if discard {
                remove[range.clone()].fill(true);
                index = range.end;
            } else {
                index += 1;
            }
        }
        let mut remove = remove.into_iter();
        self.blocks
            .entries
            .retain(|_| !remove.next().unwrap_or(false));
    }

    /// 本轮从替换后的条目重算；会话头当前位置未到齐时不把已装入的小计当作完整值。
    fn refresh_status_fields(&mut self) {
        let latest = self.entries.values().rev().find_map(|entry| match entry {
            ConversationEntry::Step { step, .. } => Some(step.key.turn_id),
            _ => None,
        });
        self.turn_usage = latest
            .filter(|turn| {
                self.pending_head.is_none()
                    && !self.replace_pending
                    && self.awaiting_head.is_none_or(|accepted| accepted == *turn)
            })
            .map(|turn| {
                UsageSummary::from_attempts(
                    self.entries
                        .values()
                        .filter_map(|entry| match entry {
                            ConversationEntry::Step { step, .. } if step.key.turn_id == turn => {
                                Some(&step.attempts)
                            }
                            _ => None,
                        })
                        .flatten()
                        .map(|attempt| (attempt.usage, attempt.usage_kind, attempt.cost)),
                )
            });
        self.context_tokens = self
            .entries
            .values()
            .rev()
            .find_map(|entry| match entry {
                ConversationEntry::Step { step, .. } => step
                    .attempts
                    .iter()
                    .rev()
                    .find_map(|attempt| attempt.usage.and_then(|usage| usage.normalized())),
                _ => None,
            })
            .filter(|_| self.pending_head.is_none())
            .and_then(|usage| match (usage.input, usage.output) {
                (Some(input), Some(output)) => input.checked_add(output),
                (input, output) => input.or(output),
            });
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

    /// 条目中新到的重试提示或结局交给 Dock；它仍留在所属条目的位置上。
    fn offer_in(&mut self, range: std::ops::Range<usize>, wanted: impl Fn(&Status) -> bool) {
        if let Some(entry) = self.blocks.entries[range]
            .iter()
            .find(|entry| matches!(&entry.block, Block::Status(status) if wanted(status)))
            && let Block::Status(status) = &entry.block
        {
            self.dock.offer(entry.id, status);
        }
    }

    pub(crate) fn show_banner(&mut self) {
        self.banner = Some((self.blocks.allocate_id(), startup_banner()));
    }

    /// 第一步：由输入区与本地评论草稿构造 1 项或 2 项计划请求并做本地上限与帧长度预检；
    /// 不读图片字节，不改动输入与草稿。
    fn plan_request(
        &self,
        turn_id: TurnId,
        input_parts: &[UserInputPart<PendingImage>],
        comments: &[crate::comment::DraftComment],
        expected_settings: SettingsVersion,
        expected_position: NodeId,
    ) -> lattice_local::Result<PlannedTurnRequest> {
        let tui_channel = Channel::parse("tui").expect("固定通道名 tui 应合法");
        let mut inputs = Vec::new();
        if !comments.is_empty() {
            let comment_channel =
                Channel::parse("tui/comment").expect("固定通道名 tui/comment 应合法");
            inputs.push(TurnInputItem {
                channel: comment_channel,
                parts: vec![UserInputPart::Text {
                    text: crate::comment::encode_draft_comments(comments),
                }],
            });
        }
        if !input_parts.is_empty() {
            inputs.push(TurnInputItem {
                channel: tui_channel.clone(),
                parts: input_parts
                    .iter()
                    .map(|part| match part {
                        UserInputPart::Text { text } => UserInputPart::Text { text: text.clone() },
                        UserInputPart::Image { image } => {
                            UserInputPart::Image { image: image.meta }
                        }
                    })
                    .collect(),
            });
        }
        let plan = PlannedTurnRequest {
            turn_id,
            session: self.session_id,
            workspace: self.project_root.clone(),
            trigger_channel: tui_channel,
            inputs,
            expected_settings: Some(expected_settings),
            expected_position: Some(expected_position),
        };
        lattice_local::check_turn_plan(&plan)?;
        Ok(plan)
    }

    /// 第二步：重读并核对普通输入项中的待发图片字节，构造实发请求。
    fn materialize_request(
        plan: &PlannedTurnRequest,
        input_parts: &[UserInputPart<PendingImage>],
    ) -> lattice_local::Result<TurnRequest> {
        let mut image_ordinal = 0usize;
        let mut inputs = Vec::with_capacity(plan.inputs.len());
        for item in &plan.inputs {
            if item.channel.as_str() == "tui/comment" {
                let parts = item
                    .parts
                    .iter()
                    .map(|part| match part {
                        UserInputPart::Text { text } => UserInputPart::Text { text: text.clone() },
                        UserInputPart::Image { .. } => {
                            unreachable!("tui/comment 仅包含自包含评论文本")
                        }
                    })
                    .collect();
                inputs.push(TurnInputItem {
                    channel: item.channel.clone(),
                    parts,
                });
            } else {
                let mut loaded = Vec::with_capacity(input_parts.len());
                for part in input_parts {
                    match part {
                        UserInputPart::Text { text } => {
                            loaded.push(UserInputPart::Text { text: text.clone() });
                        }
                        UserInputPart::Image { image } => {
                            image_ordinal = image_ordinal.saturating_add(1);
                            let bytes = match &image.source {
                                ImageSource::File(path) | ImageSource::Temp(path) => {
                                    lattice_local::read_verified_image(
                                        image_ordinal,
                                        path,
                                        &image.meta,
                                    )?
                                }
                            };
                            loaded.push(UserInputPart::Image { image: bytes });
                        }
                    }
                }
                inputs.push(TurnInputItem {
                    channel: item.channel.clone(),
                    parts: loaded,
                });
            }
        }
        Ok(TurnRequest {
            turn_id: plan.turn_id,
            session: plan.session,
            workspace: plan.workspace.clone(),
            trigger_channel: plan.trigger_channel.clone(),
            inputs,
            expected_settings: plan.expected_settings,
            expected_position: plan.expected_position,
        })
    }

    fn submit(&mut self) -> Option<TurnRequest> {
        let raw = self.input.text();
        if self.input.is_blank() && self.drafts.draft_count() == 0 {
            return None;
        }
        if self.input.image_count() == 0 && self.apply_builtin(raw.trim()) {
            self.history.push(raw.trim());
            self.input.clear();
            return None;
        }
        if self.phase != TurnPhase::Idle {
            self.push_status(submit_busy_status());
            return None;
        }
        if !self.configure_in_flight.is_empty() {
            self.push_status(submit_configuring_status());
            return None;
        }
        let turn_id = lattice_local::new_turn_id();
        let expected_settings = self.current_settings_version();
        let expected_position = self.current_expected_position();
        let comments = self.drafts.draft_comments();
        let input_parts = if self.input.is_blank() {
            Vec::new()
        } else {
            self.input.parts()
        };
        let request = match self
            .plan_request(
                turn_id,
                &input_parts,
                &comments,
                expected_settings,
                expected_position,
            )
            .and_then(|plan| Self::materialize_request(&plan, &input_parts))
        {
            Ok(request) => request,
            Err(error) => {
                self.push_status(preflight_failed_status(&error));
                return None;
            }
        };
        let input = std::mem::take(&mut self.input);
        let message = self.drafts.take_message(input);
        Some(self.start_submission(message, request))
    }

    fn start_submission(
        &mut self,
        message: crate::drafts::Outgoing,
        request: TurnRequest,
    ) -> TurnRequest {
        let id = request.turn_id;
        let text = message.input.text();
        self.history.push(text.trim());
        self.cancel_status = None;
        self.own_turn = Some(id);
        self.submitted = Some((id, message));
        self.unknown_submission = None;
        self.pending_accepted = None;
        self.accepted_turn = None;
        self.refresh_phase();
        request
    }

    fn cancel_or_clear(&mut self) {
        if let Some(target) = self.cancel_target() {
            self.cancel_requested = Some(target);
            self.cancelling = true;
            self.phase = TurnPhase::Cancelling;
        } else if self.dock.shows_input() {
            self.input.clear();
        }
    }

    /// 提交的内容在输入区为空时完整放回，输入区已有用户的草稿时不覆盖：
    /// 仅文字时留在输入历史中，图文混合时文字留在输入历史、图片未放回，纯图片时标记为未放回。
    /// 评论草稿始终原样恢复，不因屏幕锚点缺失丢弃；仅含评论且输入区为空时返回 `Restored`。
    /// 结果未知之后才确定未被接受的提交同样处理。只归还本客户端的草稿，不撤销已知的执行、接管或完成显示。
    fn reject_submission(&mut self, turn_id: TurnId) -> KeptInput {
        let Some((_, message)) = self
            .unknown_submission
            .take_if(|(id, _)| *id == turn_id)
            .or_else(|| self.submitted.take_if(|(id, _)| *id == turn_id))
        else {
            return KeptInput::Nothing;
        };
        let had_comments = !message.comments.is_empty();
        // 评论不进输入历史，总是完整恢复为草稿，绝不丢弃。
        let input = self.drafts.restore(message, &mut self.blocks);
        let has_text = !input.text().trim().is_empty();
        let has_images = input.image_count() > 0;
        if input.is_empty() {
            if had_comments && self.input.is_empty() {
                KeptInput::Restored
            } else {
                KeptInput::Nothing
            }
        } else if self.input.is_empty() {
            self.input = input;
            self.input.move_to_end();
            KeptInput::Restored
        } else if has_text && has_images {
            KeptInput::HistoryWithoutImages
        } else if has_text {
            KeptInput::History
        } else {
            KeptInput::Discarded
        }
    }

    /// 收到这次提交的确定结果：不再保留结果未知时留下的原文。
    fn resolve_unknown(&mut self, turn_id: TurnId) {
        if self
            .unknown_submission
            .as_ref()
            .is_some_and(|(id, _)| *id == turn_id)
        {
            self.unknown_submission = None;
        }
    }

    /// 判定当前活跃视图是否已因果晚于某次接受：
    /// 要么当前 `watch` 是在客户端已知该次接受之后新建的，要么同 `watch` 流已观察到不小于 `start_node` 的节点。
    fn view_confirmed_after(&self, start_node: NodeId, accepted_view_gen: u64) -> bool {
        self.active_view_gen > accepted_view_gen
            || self.watch_max_node.is_some_and(|max| max >= start_node)
    }

    /// 记录或合并本端已接受轮次的因果水位；同一轮次重复确认时保留更早的视图代数。
    fn record_accepted_turn(&mut self, turn_id: TurnId, start_node: NodeId) {
        let view_gen = self.next_view_gen;
        match &mut self.accepted_turn {
            Some(existing) if existing.turn_id == turn_id => {
                existing.start_node = start_node;
                existing.accepted_view_gen = existing.accepted_view_gen.min(view_gen);
            }
            _ => {
                self.accepted_turn = Some(AcceptedTurnWatermark {
                    turn_id,
                    start_node,
                    accepted_view_gen: view_gen,
                });
            }
        }
    }

    /// 将本端已确认接受的提交从在途/未知槽位移出，并记录该轮的因果水位。
    fn record_accepted_submission(&mut self, turn_id: TurnId, start_node: NodeId) {
        let _ = self.unknown_submission.take_if(|(id, _)| *id == turn_id);
        let _ = self.submitted.take_if(|(id, _)| *id == turn_id);
        if self.own_turn == Some(turn_id) {
            self.record_accepted_turn(turn_id, start_node);
        }
    }

    /// 视图完整确认后，按双因果水位核销待同步等待状态。
    fn reconcile_confirmed_view(&mut self) {
        if self.view_starting
            || self.replace_pending
            || self.initial
            || self.pending_head.is_some()
            || self.head.is_none()
        {
            return;
        }
        if let Some(wm) = self.accepted_turn
            && self.view_confirmed_after(wm.start_node, wm.accepted_view_gen)
        {
            if self.awaiting_head == Some(wm.turn_id) && !self.head_reached(wm.turn_id) {
                self.awaiting_head = None;
            }
            if self.pending_accepted == Some(wm.turn_id) && !self.has_turn_step(wm.turn_id) {
                self.pending_accepted = None;
            }
            if self.awaiting_head != Some(wm.turn_id) && self.pending_accepted != Some(wm.turn_id) {
                self.accepted_turn = None;
            }
        }
    }

    /// 与绘制路径使用同一内容宽度及 Dock 高度，statusline 始终占一行；
    /// `rows` 是同一宽度下的对话记录行数，Dock 高度随它变化。
    pub(crate) fn transcript_height(&self, rows: usize) -> usize {
        self.term_height
            .saturating_sub(1)
            .saturating_sub(Dock::new(self).height(self.term_width, rows)) as usize
    }

    fn apply_head(&mut self, head: SessionHead) {
        // 新视图的首条会话头或路径已替换：之后到它当前位置的条目为止是初始条目。
        // 路径替换立即丢弃此前的条目；新视图等首个初始条目到达再替换，重连失败时内容保留。
        let was_starting = std::mem::take(&mut self.view_starting);
        if was_starting {
            self.active_view_gen = self.next_view_gen;
            self.watch_max_node = Some(head.position);
        } else {
            self.watch_max_node = Some(
                self.watch_max_node
                    .map_or(head.position, |max| max.max(head.position)),
            );
        }
        let starts = was_starting || head.path_replaced;
        if head.path_replaced {
            self.pending_has_older = head.has_older;
            self.replace_entries();
            let clear_awaiting = match (self.awaiting_head, self.accepted_turn) {
                (Some(awaiting), Some(wm)) if wm.turn_id == awaiting => {
                    self.view_confirmed_after(wm.start_node, wm.accepted_view_gen)
                }
                _ => true,
            };
            if clear_awaiting {
                self.awaiting_head = None;
            }
        } else if starts {
            self.replace_pending = true;
            self.pending_has_older = head.has_older;
        }
        if starts {
            self.initial = true;
        }
        self.session_id = head.session_id;
        self.connected = true;
        if head.diagnostic != self.diagnostic {
            if let Some(error) = &head.diagnostic {
                self.push_status(execution_diagnostic_status(error));
            }
            self.diagnostic = head.diagnostic.clone();
        }
        // 当前位置是根节点时没有对应的条目，会话头立即确认。
        let root = head.position == NodeId(0);
        self.latest_head = head.clone();
        self.pending_head = Some(head);
        self.sync_head_panels();
        if root {
            self.confirm_head();
        }
    }

    /// 收到待确认会话头当前位置的条目：之后按它判定各轮。
    fn confirm_head(&mut self) {
        if std::mem::take(&mut self.replace_pending) {
            self.replace_entries();
        }
        let is_root = self
            .pending_head
            .as_ref()
            .is_some_and(|head| head.position == NodeId(0));
        self.head = self.pending_head.take();
        if is_root {
            self.has_older = false;
        }
        if self
            .awaiting_head
            .is_some_and(|turn| self.head_reached(turn))
        {
            self.awaiting_head = None;
        }
    }

    /// 已确认的会话头到达了这一轮：指向它，或当前路径上已有它的 Step（位置已经越过它）。
    /// 每批推送先给会话头，再按路径顺序给到当前位置为止的条目，所以确认时这一轮的
    /// Step 若在本批中，已经到达。
    fn head_reached(&self, turn: TurnId) -> bool {
        self.pending_head.is_none()
            && (self
                .head
                .as_ref()
                .is_some_and(|head| head.turn_id == Some(turn))
                || self.has_turn_step(turn))
    }

    /// 新视图的初始条目开始到达或路径已替换：丢弃此前的条目，迟到的历史查询结果作废。
    fn replace_entries(&mut self) {
        self.replace_pending = false;
        self.discard_entries();
        self.has_older = self.pending_has_older.take().unwrap_or(false);
        self.epoch = self.epoch.wrapping_add(1);
        self.older_pending = false;
        self.orphan_warned = None;
    }

    /// 同一 NodeId 原位替换；其余新条目接在末尾。
    fn apply_entry(&mut self, entry: ConversationEntry) {
        let node = entry.node();
        self.watch_max_node = Some(self.watch_max_node.map_or(node, |max| max.max(node)));
        if self.replace_pending {
            self.replace_entries();
        }
        if let ConversationEntry::Step { step, .. } = &entry {
            let turn = step.key.turn_id;
            if self.pending_accepted == Some(turn) {
                self.pending_accepted = None;
            }
            if step.key.ordinal == 0 {
                // 视图中出现这次提交的 Step 0，同样说明已被接受；回执可能更晚到或者丢失。
                // 它只证明已接受，忙状态仍保留到会话头确认。
                let was_unknown = self
                    .unknown_submission
                    .as_ref()
                    .is_some_and(|(id, _)| *id == turn);
                let was_submitted = self.submitted.as_ref().is_some_and(|(id, _)| *id == turn);
                if was_unknown || was_submitted {
                    self.record_accepted_submission(turn, node);
                }
                if was_unknown {
                    self.notice_once(recovered_status(turn));
                }
                if was_submitted && !self.head_reached(turn) {
                    self.awaiting_head = Some(turn);
                }
            }
        }
        let turn = entry_turn(&entry);
        let known_retries: HashSet<u32> = match self.entries.insert(node, entry) {
            Some(ConversationEntry::Step { retries, .. }) => {
                retries.iter().map(|retry| retry.attempt).collect()
            }
            _ => HashSet::new(),
        };
        if self
            .texts
            .get(&node)
            .is_some_and(|generating| crate::session::superseded(&self.entries[&node], generating))
        {
            self.texts.remove(&node);
        }
        let blocks = crate::session::entry_blocks(
            &self.entries[&node],
            turn.is_some_and(|t| self.ended.contains(&t)),
            self.texts.get(&node),
        );
        let range = match self.blocks.node_start(node) {
            Some(start) => self.blocks.replace(self.blocks.group(start), blocks),
            None => {
                let end = self.blocks.len();
                self.blocks.replace(end..end, blocks)
            }
        };
        // 推送使条目多出的重试提示是新到的消息；初始条目中已有的不是。
        if !self.initial {
            let fresh: Vec<_> = self.blocks.entries[range.clone()]
                .iter()
                .filter_map(|entry| match &entry.block {
                    Block::Status(Status {
                        retry: Some(key), ..
                    }) if !known_retries.contains(&key.attempt_ordinal) => Some(*key),
                    _ => None,
                })
                .collect();
            for key in fresh {
                self.offer_in(range.clone(), |status| status.retry == Some(key));
            }
        }
        if self
            .pending_head
            .as_ref()
            .is_some_and(|head| head.position == node)
        {
            self.confirm_head();
        }
    }

    /// 生成中的文字按所属节点与尝试累积；新的尝试从头开始。
    fn apply_text(&mut self, update: TextUpdate) {
        let generating = self.texts.entry(update.node).or_default();
        if generating.attempt != update.attempt || update.offset == 0 {
            *generating = Generating {
                attempt: update.attempt,
                ..Generating::default()
            };
        }
        for segment in update.segments {
            match segment {
                TextSegment::Text { text } => {
                    generating.received = generating.received.saturating_add(text.len());
                    generating.text.push_str(&text);
                }
                TextSegment::Reasoning { text } => {
                    generating.received = generating.received.saturating_add(text.len());
                }
            }
        }
        if self
            .entries
            .get(&update.node)
            .is_some_and(|entry| crate::session::superseded(entry, &self.texts[&update.node]))
        {
            self.texts.remove(&update.node);
        }
        self.render_node(update.node);
    }

    fn render_node(&mut self, node: NodeId) {
        let Some(entry) = self.entries.get(&node) else {
            return;
        };
        let ended = entry_turn(entry).is_some_and(|turn| self.ended.contains(&turn));
        let blocks = crate::session::entry_blocks(entry, ended, self.texts.get(&node));
        if let Some(start) = self.blocks.node_start(node) {
            self.blocks.replace(self.blocks.group(start), blocks);
        }
    }

    /// 按当前会话头与条目重新判定各轮是否结束。由未结束变为结束的一轮重画它的各段：
    /// 槽位改按会话库给出的结论显示，最后一步带出结局；`fresh` 且不在初始条目中时结局是新到的消息。
    fn settle(&mut self, fresh: bool) {
        let turns: std::collections::BTreeSet<TurnId> =
            self.entries.values().filter_map(entry_turn).collect();
        for turn in turns {
            let ended = self.turn_ended(turn);
            if ended == self.ended.contains(&turn) {
                continue;
            }
            if ended {
                self.ended.insert(turn);
            } else {
                self.ended.remove(&turn);
            }
            let nodes: Vec<NodeId> = self
                .entries
                .iter()
                .filter(|(_, entry)| entry_turn(entry) == Some(turn))
                .map(|(node, _)| *node)
                .collect();
            for node in nodes {
                self.render_node(node);
            }
            if ended
                && fresh
                && !self.initial
                && let Some(start) = self.last_step_start(turn)
            {
                let range = self.blocks.group(start);
                self.offer_in(range, |status| {
                    status.closing.as_ref().is_some_and(|(id, _)| *id == turn)
                });
            }
        }
        // 当前位置那一轮停在未关闭的 Step 上而没有 Owner：执行者已消失，每个视图提示一次。
        if let Some(head) = &self.head
            && let Some(turn) = head.turn_id
            && !head.owner
            && self.orphan_warned != Some(turn)
            && self
                .last_summary(turn)
                .is_some_and(|summary| !summary.progress.is_closed())
        {
            self.orphan_warned = Some(turn);
            self.push_status(orphaned_turn_status(turn));
        }
    }

    fn last_step_start(&self, turn: TurnId) -> Option<usize> {
        let node = self
            .entries
            .iter()
            .rev()
            .find_map(|(node, entry)| match entry {
                ConversationEntry::Step {
                    step,
                    summary: Some(_),
                    ..
                } if step.key.turn_id == turn => Some(*node),
                _ => None,
            })?;
        self.blocks.node_start(node)
    }

    /// 视图翻到对话记录顶部且仍有更早历史时，以已装入的最早条目节点为 `before`
    /// 取更早条目。失败后不自动重查，等下一次翻到顶部。
    fn request_older_at_top(&mut self) {
        if self.viewport.scroll() != 0 || self.older_pending || !self.has_older {
            return;
        }
        let session_id = self.session_id;
        let Some((&before, _)) = self.entries.first_key_value() else {
            return;
        };
        self.older_pending = true;
        self.older_request = Some(OlderRequest {
            session_id,
            before,
            epoch: self.epoch,
        });
    }

    /// 取到的更早条目插在已装入的最早条目之前，视图停在原来看到的内容上。请求发出后路径被替换时，
    /// 结果属于旧路径，丢弃。
    pub(crate) fn apply_older(
        &mut self,
        request: OlderRequest,
        result: lattice_local::Result<LocalHistory>,
    ) {
        if request.epoch != self.epoch {
            return;
        }
        self.older_pending = false;
        let history = match result {
            Ok(history) => history,
            Err(error) => {
                self.push_status(older_turns_unavailable_status(&error));
                return;
            }
        };
        if history.entries.is_empty() {
            self.has_older = false;
            self.reconcile_confirmed_view();
            self.refresh_phase();
            return;
        }
        self.has_older = history.has_more;
        let Some(mut at) = self.blocks.node_start(request.before) else {
            return;
        };
        let width = self.term_width;
        // 横幅与插入点之前的状态消息不随插入移动；翻到顶部时第 0 行总是横幅，所以以屏幕上
        // 第一行插入点之后的内容为准。记下这一行、它所属的块，及它在这个块自己的各行中的倒数序号：
        // 块前面有了内容时开头会多出分隔空行，从末行数起不受影响；块内穿插的内嵌评论属于别的块，
        // 不计入。插入后按它恢复视图。
        let scroll = self.viewport.scroll();
        let transcript = render_transcript(self, width);
        let visible = scroll.saturating_add(self.transcript_height(transcript.lines.len()));
        let fixed: HashSet<BlockId> = self
            .banner
            .iter()
            .map(|(id, _)| *id)
            .chain(self.blocks.entries[..at].iter().map(|entry| entry.id))
            .collect();
        let top = (scroll..visible.min(transcript.owners.len()))
            .find(|row| !fixed.contains(&transcript.owners[*row].id))
            .and_then(|row| Some((row, block_rows_from_end(&transcript, row)?)));
        for entry in history.entries {
            let node = entry.node();
            if let ConversationEntry::Step { step, .. } = &entry
                && step.key.ordinal == 0
            {
                self.record_accepted_submission(step.key.turn_id, node);
            }
            if self.entries.contains_key(&node) {
                continue;
            }
            let turn = entry_turn(&entry);
            self.entries.insert(node, entry);
            // 更早的轮都已结束：当前位置所属的那一轮总在已装入的部分里。不按会话头判定，
            // 新会话头待确认时也一样；它们的提示与结局不是新消息，直接进对话记录。
            let ended = turn.is_some_and(|t| {
                let ended = self.last_summary(t).is_some();
                if ended {
                    self.ended.insert(t);
                }
                ended
            });
            let blocks = crate::session::entry_blocks(&self.entries[&node], ended, None);
            at = self.blocks.replace(at..at, blocks).end;
        }
        // 浏览光标由 prepare_transcript 按所在块重新定位并保持屏幕行；焦点在 Dock 时移动视图。
        // 屏幕上只有横幅与前置状态消息时，插入的行都在屏幕之下，视图不动。
        if matches!(self.focus, Focus::Dock)
            && !self.viewport.following()
            && let Some((old_row, (id, rest))) = top
        {
            let rows = block_rows(&render_transcript(self, width), id);
            if let Some(&row) = rows
                .len()
                .checked_sub(rest)
                .and_then(|index| rows.get(index))
            {
                self.viewport.shift(row.saturating_sub(old_row));
            }
        }
        self.settle(false);
        self.reconcile_confirmed_view();
        self.refresh_status_fields();
        self.refresh_phase();
    }

    /// 这次提交的身份：会话在启动时已存在或经 `configure(New)` 建立。
    #[cfg(test)]
    pub(crate) fn submission_identity(&self) -> Option<(TurnId, SessionId)> {
        let (turn_id, _) = self.submitted.as_ref()?;
        Some((*turn_id, self.session_id))
    }

    /// 这一轮已经结束：最后一步已关闭，或者会话头说明它不在执行（当前位置在别处，或没有 Owner）。
    /// 新会话头待确认时，已确认的那条早于本批条目，不据它作新的判定：本批中刚到的一轮会被它
    /// 当成已在别处结束。其间沿用确认时的结论，确认后再按新会话头判定，已结束的轮也不因此反复。
    /// 还没有步骤的一轮没有可显示的结局，留到步骤到达再判定。
    fn turn_ended(&self, turn: TurnId) -> bool {
        self.last_summary(turn).is_some_and(|summary| {
            summary.progress.is_closed()
                || match (&self.pending_head, &self.head) {
                    (Some(_), _) => self.ended.contains(&turn),
                    (None, Some(head)) => head.turn_id != Some(turn) || !head.owner,
                    (None, None) => false,
                }
        })
    }

    /// 退出报告所述那一轮已知的情况。对话记录已不在屏幕上，结局照常重述。
    fn exit_viewed(&self, turn: Option<TurnId>) -> Viewed {
        if turn.is_some_and(|id| self.turn_ended(id)) {
            Viewed::Ended
        } else {
            Viewed::Unknown
        }
    }

    fn apply_clipboard(&mut self, result: Result<Content, ClipboardError>) {
        match result {
            Ok(Content::Text(text)) => {
                self.apply_action(KeyAction::Paste(text));
            }
            Ok(Content::Image(bytes)) => {
                let existing = self.input.image_refs();
                let meta = match lattice_local::image_bytes_ref(&bytes, &existing) {
                    Ok(meta) => meta,
                    Err(error) => {
                        self.push_status(paste_image_failed_status(&error));
                        return;
                    }
                };
                let path = match self.stage_clipboard_image(&meta, &bytes) {
                    Ok(path) => path,
                    Err(error) => {
                        self.push_status(clipboard_stage_failed_status(&error));
                        return;
                    }
                };
                self.input.insert_image(PendingImage {
                    meta,
                    source: ImageSource::Temp(path),
                });
            }
            Err(error) => {
                self.push_status(clipboard_failed_status(&error));
            }
        }
    }

    fn stage_clipboard_image(
        &mut self,
        meta: &lattice_local::ImageRef,
        bytes: &[u8],
    ) -> anyhow::Result<PathBuf> {
        if self.staging.is_none() {
            self.staging = Some(Staging::create_in(Path::new("/tmp"))?);
        }
        self.staging
            .as_ref()
            .expect("已创建剪贴板暂存目录")
            .store(meta, bytes)
    }
}

/// 块自己的各行在对话记录中的位置，按行序；不含穿插其间、属于别的块的行。
fn block_rows(transcript: &Transcript, id: BlockId) -> Vec<usize> {
    transcript
        .owners
        .iter()
        .enumerate()
        .filter(|(_, owner)| owner.id == id)
        .map(|(row, _)| row)
        .collect()
}

/// `row` 所属的块，及这一行在该块自己的各行中的倒数序号（末行为 1）。
fn block_rows_from_end(transcript: &Transcript, row: usize) -> Option<(BlockId, usize)> {
    let id = transcript.owners.get(row)?.id;
    let rows = block_rows(transcript, id);
    let index = rows.iter().position(|known| *known == row)?;
    Some((id, rows.len().saturating_sub(index)))
}

fn entry_turn(entry: &ConversationEntry) -> Option<TurnId> {
    entry.turn_id()
}

/// 进入 raw 模式后并行接收视图推送与按键；启动不等待条目读取。
pub(crate) async fn run_app(
    options: TuiOptions,
    ctx: SessionContext,
    client: LocalClient,
) -> anyhow::Result<()> {
    // try_init 可能在 raw/备用屏幕已改变后失败，恢复 guard 必须先于初始化。
    let mut cleanup = TerminalCleanup { kitty: false };
    let mut terminal = ratatui::try_init()?;
    let kitty = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    install_cleanup_panic_hook(kitty);
    cleanup.kitty = kitty;
    enable_terminal_modes(&mut std::io::stdout(), kitty)?;

    let mut app = App::new(
        ctx,
        kitty,
        &options.session_dir,
        options.project_root.clone(),
    );
    app.show_banner();
    let mut exit = run_loop(&options, &mut app, &client, EventStream::new(), |app| {
        terminal.draw(|f| crate::view::draw(f, app))?;
        Ok(())
    })
    .await;
    // 先复原终端，再由共同句柄有界承接既有意图；普通退出不新增取消。
    drop(cleanup);
    if let Some(turn) = exit.turn {
        exit.handoffs.push(turn.detach().boxed());
    }
    while let Some(report) = exit.handoffs.next().await {
        let viewed = app.exit_viewed(report.turn_id);
        for line in detach_lines(report, viewed) {
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
/// 会话视图与本客户端的提交句柄分开持有：内容只取自视图，提交、取消与退出交接经句柄。
async fn run_loop<E, D>(
    options: &TuiOptions,
    app: &mut App,
    client: &LocalClient,
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
    let mut view: Option<LocalTurn> = None;
    let mut turn: Option<LocalTurn> = None;
    // 句柄交付完自己的更新后仍保留，退出或换用新句柄时交给 detach。
    let mut turn_open = false;
    let mut cancels = FuturesUnordered::new();
    let mut older = FuturesUnordered::new();
    let mut models = FuturesUnordered::new();
    let mut configures = FuturesUnordered::new();
    let mut dirty = true;
    let result = loop {
        if std::mem::take(&mut app.view_wanted) && !app.should_quit {
            // 重新开始 watch 用新的句柄，旧连接上尚未读出的推送随旧句柄丢弃。
            view = Some(client.observe(app.session_id));
            app.begin_view();
        }
        if let Some(target) = app.cancel_requested.take() {
            match target {
                CancelTarget::Own => {
                    if let Some(turn) = &turn {
                        turn.cancel();
                    }
                }
                CancelTarget::Other(turn_id) => {
                    let client = client.clone();
                    cancels.push(async move { (turn_id, client.cancel(turn_id).await) }.boxed());
                }
            }
            dirty = true;
        }
        if let Some(request) = app.older_request.take() {
            let client = client.clone();
            older.push(
                async move {
                    let result = client
                        .history_before(request.session_id, request.before)
                        .await;
                    (request, result)
                }
                .boxed(),
            );
        }
        if let Some(request) = app.model_query.take() {
            let client = client.clone();
            models.push(
                async move {
                    let result = client.models(request.query.clone()).await;
                    (request, result)
                }
                .boxed(),
            );
        }
        if let Some(request) = app.configure_request.take() {
            let client = client.clone();
            let session = ConfigureSession::Existing(app.session_id);
            let workspace = options.project_root.clone();
            configures.push(
                async move {
                    let result = client
                        .configure(ConfigureRequest {
                            session,
                            workspace,
                            agent: None,
                            model: request.model,
                            effort: request.effort,
                        })
                        .await;
                    (request.generation, result)
                }
                .boxed(),
            );
        }
        if std::mem::take(&mut app.clipboard_request) {
            let result = crate::clipboard::read_system().await;
            app.apply_clipboard(result);
            dirty = true;
        }
        if app.should_quit {
            break Ok(());
        }
        let mut request = None;
        tokio::select! {
            event = events.next() => {
                match event {
                    Some(Ok(event)) => request = handle_input_event(app, event),
                    Some(Err(_)) | None => app.should_quit = true,
                }
            }
            Some(report) = handoffs.next(), if !handoffs.is_empty() => app.apply_handoff(report),
            update = async { turn.as_mut().expect("已有句柄").next().await }, if turn_open => {
                match update {
                    Some(update) => app.apply_submission(update),
                    None => turn_open = false,
                }
            }
            update = async { view.as_mut().expect("已有视图").next().await }, if view.is_some() => {
                match update {
                    Some(update) => {
                        // 提交句柄不开视图，取消补发与结束判定用转入的同一份视图消息。
                        let forwarded = match &update {
                            LocalUpdate::Head(head) => Some(ViewUpdate::Head(head.clone())),
                            LocalUpdate::Entry { entry } => Some(ViewUpdate::Entry { entry: entry.clone() }),
                            LocalUpdate::Text(text) => Some(ViewUpdate::Text(text.clone())),
                            _ => None,
                        };
                        app.apply_view(update);
                        if let (Some(turn), Some(forwarded)) = (&turn, forwarded) {
                            turn.apply_view(forwarded).await;
                        }
                    }
                    None => {
                        view = None;
                        app.connection_lost(&ConnectionStatus::Eof);
                    }
                }
            }
            Some((turn_id, result)) = cancels.next(), if !cancels.is_empty() => app.apply_cancel_result(turn_id, result),
            Some((request, result)) = older.next(), if !older.is_empty() => app.apply_older(request, result),
            Some((request, result)) = models.next(), if !models.is_empty() => app.apply_models(request, result),
            Some((generation, result)) = configures.next(), if !configures.is_empty() => app.apply_configure(generation, result),
            _ = ticker.tick() => {
                dirty |= app.expire_ctrl_c_notice(Instant::now());
                if !dirty { continue; }
                if let Err(error) = draw(app) { break Err(error); }
                dirty = false;
                continue;
            }
        }
        dirty = true;
        if let Some(request) = request {
            if let Some(old) = turn.take() {
                handoffs.push(old.detach().boxed());
            }
            turn = Some(client.start_submission(request));
            turn_open = true;
        }
    };
    LoopExit {
        result,
        turn,
        handoffs,
    }
}

#[rustfmt::skip]
fn handle_input_event(app: &mut App, event: Event) -> Option<TurnRequest> {
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

#[rustfmt::skip]
fn help_lines(kitty: bool) -> Vec<String> {
    let nl = if kitty { "Shift+Enter  换行" } else { "Alt+Enter  换行" };
    vec![
        "/help  显示本帮助".into(),
        "/model  选择下一次提交的模型".into(),
        "/effort  选择下一次提交的推理档位".into(),
        "/reconnect  只重连观察，不恢复或重新提交".into(),
        "/quit  退出（输入为空时也可按 Ctrl+D）".into(),
        "Esc  清空输入；关闭本面板".into(),
        "Space  关闭警告或错误，移入对话记录".into(),
        "Ctrl+C  取消运行中 Turn；空闲清空输入；两秒内再按退出".into(),
        "Ctrl+↑/↓  输入历史".into(),
        "Ctrl+V  粘贴剪贴板中的图片或文字".into(),
        "Enter  提交".into(),
        nl.into(),
    ]
}

#[rustfmt::skip]
/// session 与 agent 由状态行显示并随接受更新，这里不重复。
fn startup_banner() -> Vec<String> {
    vec![
        "LATTICE".into(),
        "/help  显示命令".into(),
        "/model  选择模型".into(),
        "/effort  选择推理档位".into(),
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
#[path = "test_fixtures.rs"]
mod fixtures;

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    #[test]
    fn terminal_cleanup_restores_on_normal_error_and_panic_paths() {
        const PROBE: &str = "LATTICE_TUI_CLEANUP_PROBE";
        if let Ok(mode) = std::env::var(PROBE) {
            let run = || -> std::io::Result<()> {
                let _cleanup = TerminalCleanup { kitty: false };
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
        for mode in ["normal", "error", "panic"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "app::tests::terminal_cleanup_restores_on_normal_error_and_panic_paths",
                    "--nocapture",
                ])
                .env(PROBE, mode)
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
            assert!(enabled.unwrap() < restored.unwrap());
        }
    }

    #[test]
    fn model_command_opens_loading_panel_without_submitting() {
        let mut app = test_app(false);
        app.input.set_text("/model");
        assert!(app.submit().is_none(), "命令不能成为 Turn 输入");
        assert_eq!(dock_title(&mut app), "MODEL");
        assert_eq!(
            app.model_query.as_ref().map(|q| &q.query),
            Some(&ModelCandidateQuery::List {
                provider: None,
                ready_only: true,
            })
        );
        assert!(app.input.is_empty());
        assert_eq!(app.phase, TurnPhase::Idle);
    }

    #[test]
    fn selection_panel_filters_scrolls_and_preserves_hidden_input_under_messages() {
        use lattice_local::{Effort::Low, ModelCandidates};
        let mut app = test_app(true);
        let mut current = head(0, None, false);
        current.provider = Some(ProviderId::parse("p").unwrap());
        current.model = Some(ModelId::parse("m6").unwrap());
        app.apply_view(LocalUpdate::Head(current));
        app.input.set_text("hidden draft");
        let open_models = |app: &mut App| {
            app.apply_builtin("/model");
            let query = app.model_query.take().unwrap();
            assert_eq!(
                query.query,
                ModelCandidateQuery::List {
                    provider: None,
                    ready_only: true,
                }
            );
            app.apply_models(
                query,
                Ok(ModelCandidates {
                    models: (0..10)
                        .map(|n| model_candidate("p", &format!("m{n}"), &[Low]))
                        .collect(),
                    warnings: vec![],
                }),
            );
        };
        let confirm_chosen_model = |app: &mut App, expected: &str, reason: &str| {
            let pending = app
                .configure_request
                .take()
                .unwrap_or_else(|| panic!("{reason}: 选定模型应发出 configure 请求"));
            assert_eq!(
                pending.model.as_ref().map(ToString::to_string).as_deref(),
                Some(expected),
                "{reason}"
            );
            assert_eq!(pending.effort, None, "{reason}");
            let target = pending.model.unwrap();
            let mut updated = head(0, None, false);
            updated.provider = Some(target.provider().clone());
            updated.model = Some(target.model().clone());
            app.apply_configure(
                pending.generation,
                Ok(ConfigureReply {
                    head: updated,
                    adjustment: None,
                }),
            );
            assert_eq!(
                app.current_target().map(|t| t.to_string()).as_deref(),
                Some(expected),
                "{reason}"
            );
        };
        open_models(&mut app);
        let screen = |app: &mut App, width, height| {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| crate::view::draw(frame, app))
                .unwrap();
            (
                buffer_text(terminal.backend().buffer()),
                terminal.get_cursor_position().unwrap(),
            )
        };
        let (shown, cursor) = screen(&mut app, 30, 8);
        assert_eq!(shown.lines().nth(6).unwrap().trim(), "p/m6 · 当前");
        assert!(!shown.contains("p/m0"), "{shown}");
        assert_eq!((cursor.x, cursor.y), (0, 6), "光标位于当前候选行");
        app.apply_action(KeyAction::Down);
        let (down, cursor) = screen(&mut app, 30, 8);
        assert_eq!(down.lines().nth(6).unwrap().trim(), "p/m7");
        assert!(!down.contains("p/m3"), "Down 越过原可见区时内部滚动");
        assert_eq!((cursor.x, cursor.y), (0, 6));
        app.apply_action(KeyAction::Submit);
        confirm_chosen_model(&mut app, "p/m7", "选择 m7");
        open_models(&mut app);
        for _ in 0..5 {
            app.apply_action(KeyAction::Up);
        }
        let (up, cursor) = screen(&mut app, 30, 8);
        assert_eq!(up.lines().nth(5).unwrap().trim(), "p/m2");
        assert!(up.contains("p/m0") && !up.contains("p/m7"));
        assert_eq!((cursor.x, cursor.y), (0, 5));
        app.apply_action(KeyAction::Submit);
        confirm_chosen_model(&mut app, "p/m2", "选择 m2");
        open_models(&mut app);
        for _ in 0..20 {
            app.apply_action(KeyAction::Up);
        }
        let (first, cursor) = screen(&mut app, 30, 8);
        assert_eq!(first.lines().nth(3).unwrap().trim(), "p/m0");
        assert_eq!((cursor.x, cursor.y), (0, 3));
        app.apply_action(KeyAction::Submit);
        confirm_chosen_model(&mut app, "p/m0", "顶部夹紧");
        open_models(&mut app);
        for _ in 0..20 {
            app.apply_action(KeyAction::Down);
        }
        let (last, cursor) = screen(&mut app, 30, 8);
        assert_eq!(last.lines().nth(6).unwrap().trim(), "p/m9");
        assert_eq!((cursor.x, cursor.y), (0, 6));
        app.apply_action(KeyAction::Submit);
        confirm_chosen_model(&mut app, "p/m9", "底部夹紧");
        open_models(&mut app);
        let (small, cursor) = screen(&mut app, 7, 5);
        assert_eq!(small.lines().nth(2).unwrap().trim(), "过滤：");
        assert!(small.lines().nth(3).unwrap().starts_with("p/m9"));
        assert_eq!((cursor.x, cursor.y), (0, 3));
        let (filter_only, _) = screen(&mut app, 7, 3);
        assert_eq!(filter_only.lines().nth(1).unwrap().trim(), "过滤：");
        assert!(!filter_only.contains("p/m9"), "只有过滤行空间时不画候选");
        app.apply_action(KeyAction::Paste("P/M9".into()));
        let (filtered, _) = screen(&mut app, 30, 8);
        assert!(filtered.contains("p/m9") && !filtered.contains("p/m6"));
        app.apply_action(KeyAction::Insert('x'));
        assert!(app.apply_action(KeyAction::Submit).is_none());
        assert!(!app.dock.shows_input(), "空结果不可选定");
        app.apply_action(KeyAction::Backspace);
        app.push_status(status(NoticeLevel::Warn, "overlay"));
        app.apply_action(KeyAction::Paste("invisible".into()));
        app.apply_action(KeyAction::Submit);
        app.apply_action(KeyAction::ScrollPageUp);
        app.apply_action(KeyAction::ScrollToBottom);
        app.apply_action(KeyAction::Insert(' '));
        let (restored, _) = screen(&mut app, 30, 8);
        assert!(restored.contains("P/M9") && restored.contains("p/m9"));
        assert!(!restored.contains("invisible"));
        assert_eq!(app.input.text(), "hidden draft");
        app.apply_action(KeyAction::Submit);
        confirm_chosen_model(&mut app, "p/m9", "释放消息后选择 m9");
        app.apply_builtin("/model");
        let query = app.model_query.take().unwrap();
        app.apply_models(
            query,
            Ok(ModelCandidates {
                models: vec![model_candidate("p", "界e\u{301}尾", &[])],
                warnings: vec![],
            }),
        );
        app.apply_action(KeyAction::Down);
        app.apply_action(KeyAction::Up);
        let (wide, _) = screen(&mut app, 6, 8);
        assert!(
            wide.contains("p/界e\u{301}") && !wide.contains("尾"),
            "{wide}"
        );
        for (width, height) in [(0, 0), (0, 1), (1, 1), (1, 2), (7, 3), (176, 50)] {
            screen(&mut app, width, height);
        }
        app.apply_action(KeyAction::Clear);
        assert_eq!(app.configure_request, None, "Esc 不发出 configure 请求");
        assert_eq!(
            app.current_target().map(|target| target.to_string()),
            Some("p/m9".into()),
            "Esc 保留当前会话头模型"
        );
    }

    #[test]
    fn config_error_model_is_marked_and_offers_no_effort() {
        use lattice_local::ModelCandidates;
        let mut app = test_app(true);
        let broken = lattice_local::ModelCandidate {
            availability: lattice_local::Availability::Invalid {
                reason: "effort low".into(),
            },
            ..model_candidate("p", "broken", &[])
        };
        let candidates = ModelCandidates {
            models: vec![broken],
            warnings: vec![],
        };
        let screen = |app: &mut App| {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 8)).unwrap();
            terminal
                .draw(|frame| crate::view::draw(frame, app))
                .unwrap();
            buffer_text(terminal.backend().buffer())
        };
        app.apply_builtin("/model");
        let query = app.model_query.take().unwrap();
        assert_eq!(
            query.query,
            ModelCandidateQuery::List {
                provider: None,
                ready_only: true,
            }
        );
        app.apply_models(query, Ok(candidates.clone()));
        let shown = screen(&mut app);
        assert!(shown.contains("p/broken"), "{shown}");
        app.apply_action(KeyAction::Submit);
        let pending = app
            .configure_request
            .take()
            .expect("选定候选时应发出 configure 请求");
        assert_eq!(
            pending.model.as_ref().map(ToString::to_string),
            Some("p/broken".into())
        );
        // 会话头已持有后续配置失效的模型时，/effort 按会话头精确查询并显示失效原因且不可选定。
        let mut broken_head = head(0, None, false);
        broken_head.provider = Some(ProviderId::parse("p").unwrap());
        broken_head.model = Some(ModelId::parse("broken").unwrap());
        app.apply_view(LocalUpdate::Head(broken_head));
        app.apply_builtin("/effort");
        let query = app.model_query.take().unwrap();
        assert_eq!(
            query.query,
            ModelCandidateQuery::Target("p/broken".parse().unwrap())
        );
        app.apply_models(query, Ok(candidates));
        let shown = screen(&mut app);
        assert!(shown.contains("effort low"), "{shown}");
        app.apply_action(KeyAction::Submit);
        assert_eq!(
            app.configure_request, None,
            "没有可选档位，Enter 不发出 configure 请求"
        );
        assert_eq!(app.current_effort(), None);
    }

    #[test]
    fn ctrl_c_targets_running_turn_but_esc_only_clears_input() {
        let mut app = test_app(true);
        let id = lattice_local::new_turn_id();
        open_view(
            &mut app,
            head(2, Some(id), true),
            vec![user(1, id, 1, "prompt"), step(2, id, 0).entry()],
        );
        assert_eq!(app.phase, TurnPhase::Streaming);
        app.input.set_text("draft");
        handle_input_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );
        assert!(app.input.is_empty());
        assert_eq!(app.cancel_requested, None);
        assert_eq!(app.phase, TurnPhase::Streaming);
        handle_input_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        );
        // 其他客户端提交的一轮按会话头所示的 TurnId 直接取消。
        assert_eq!(app.cancel_requested, Some(CancelTarget::Other(id)));
        assert_eq!(app.phase, TurnPhase::Cancelling);
        handle_input_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        );
        assert!(app.should_quit);
    }
    #[test]
    fn double_ctrl_c_within_window_quits_single_does_not() {
        let mut app = test_app(false);
        assert!(!matches!(ctrl_c_action(&mut app), KeyAction::Quit));
        app.apply_action(KeyAction::CancelOrClear);
        assert!(!app.should_quit);
        assert!(matches!(ctrl_c_action(&mut app), KeyAction::Quit));
        app.apply_action(KeyAction::Quit);
        assert!(app.should_quit);

        let mut app = test_app(false);
        app.input.set_text("clear on first Ctrl+C");
        ctrl_c_action(&mut app);
        app.last_ctrl_c = Some(Instant::now() - Duration::from_secs(3));
        assert!(!matches!(ctrl_c_action(&mut app), KeyAction::Quit));
        app.apply_action(KeyAction::CancelOrClear);
        assert!(app.input.is_empty());
        assert!(!app.should_quit);
    }

    /// 消息之间不叠：替换（Warn→Warn、Warn→Error、Error→Warn）把旧消息落回对话记录的
    /// 原时间位置，释放后每条恰好出现一次；Info 不进 Dock；草稿不受影响。
    #[test]
    fn dock_replacement_and_release_keep_transcript_order_and_draft() {
        let mut app = test_app(false);
        app.input.set_text("draft");
        let docked_is = |app: &App, level: NoticeLevel, marker: &str| {
            app.dock
                .message()
                .is_some_and(|status| status.level == level && status.text.contains(marker))
        };
        app.push_status(status(NoticeLevel::Warn, "warn-0"));
        app.push_status(status(NoticeLevel::Warn, "warn-a"));
        assert!(docked_is(&app, NoticeLevel::Warn, "warn-a"));
        let text = transcript(&app);
        assert!(text.contains("warn-0") && !text.contains("warn-a"));
        app.push_status(status(NoticeLevel::Info, "info-i"));
        assert!(
            docked_is(&app, NoticeLevel::Warn, "warn-a"),
            "Info 不进 Dock"
        );
        assert!(transcript(&app).contains("info-i"));
        app.push_status(status(NoticeLevel::Error, "commit-b"));
        assert!(docked_is(&app, NoticeLevel::Error, "commit-b"));
        assert!(transcript(&app).contains("warn-a"));
        app.push_status(status(NoticeLevel::Warn, "warn-c"));
        assert!(docked_is(&app, NoticeLevel::Warn, "warn-c"));
        // Space：释放当前消息，Space 不进入输入，草稿不变。
        assert!(app.apply_action(KeyAction::Insert(' ')).is_none());
        assert!(app.dock.shows_input());
        assert_eq!(app.input.text(), "draft");
        let order: Vec<usize> = ["warn-0", "warn-a", "info-i", "commit-b", "warn-c"]
            .into_iter()
            .map(|marker| row(&app, marker))
            .collect();
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(order, sorted, "保持到达顺序");
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
        let shown = ["USER", "HELP", "MODEL", "EFFORT", "WARN", "ERROR"]
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
            let mut app = test_app(false);
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
            assert!(!transcript(&app).contains("/reconnect"), "{name}");
            // 回到输入区后恢复普通编辑。
            let draft = app.input.text();
            press(&mut app, KeyCode::Char('x'));
            assert_eq!(app.input.text(), format!("{draft}x"), "{name}");
        }
    }

    /// 启动横幅固定在对话记录最上方：新视图丢弃条目与插入更早的轮都不把它当作状态消息重新排位。
    #[test]
    fn startup_banner_stays_on_top_across_new_views_and_older_turns() {
        let mut app = test_app(true);
        app.show_banner();
        app.push_status(status(NoticeLevel::Info, "before-view"));
        let turn = |n: u64| TurnId::parse(&format!("33333333-3333-4333-8333-{n:012}")).unwrap();
        let entries = |turns: std::ops::RangeInclusive<u64>| {
            turns
                .flat_map(|n| {
                    [
                        user(2 * n - 1, turn(n), n, &format!("prompt-{n}")),
                        step(2 * n, turn(n), 0)
                            .turn_ordinal(n)
                            .text(&format!("answer-{n}"))
                            .finished()
                            .entry(),
                    ]
                })
                .collect::<Vec<_>>()
        };
        for (turns, has_older) in [(1..=1, false), (2..=3, true)] {
            let mut h = head(2 * turns.end(), Some(turn(*turns.end())), false);
            h.has_older = Some(has_older);
            open_view(&mut app, h, entries(turns));
        }
        app.term_height = 100;
        app.apply_action(KeyAction::ScrollPageUp);
        let request = app.older_request.take().expect("第 1 轮尚未装入");
        app.apply_older(
            request,
            Ok(LocalHistory {
                entries: entries(1..=1),
                has_more: false,
            }),
        );
        let lines = crate::view::transcript_lines(&app, 78)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(
            lines.iter().take(6).map(String::as_str).collect::<Vec<_>>(),
            [
                "LATTICE",
                "/help  显示命令",
                "/model  选择模型",
                "/effort  选择推理档位",
                "/quit  退出",
                "/reconnect  重新读取状态并观察"
            ]
        );
        assert_eq!(
            lines.iter().filter(|line| line.contains("LATTICE")).count(),
            1
        );
        // 状态消息出现后不移动：它在视图开始前出现，插入的更早的轮也排在它之后。
        assert!(row(&app, "before-view") < row(&app, "prompt-1"));
        assert!(row(&app, "prompt-1") < row(&app, "prompt-2"));
    }

    /// WARN / ERROR 下屏蔽不可见输入编辑，全局动作照常。
    #[test]
    fn dock_status_blocks_invisible_input_but_keeps_globals() {
        for level in [NoticeLevel::Warn, NoticeLevel::Error] {
            let mut app = test_app(true);
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
            let id = lattice_local::new_turn_id();
            app.apply_view(LocalUpdate::Head(head(2, Some(id), true)));
            app.apply_view(entry_update(user(1, id, 1, "prompt")));
            app.apply_view(entry_update(step(2, id, 0).entry()));
            handle_input_event(
                &mut app,
                Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            );
            assert_eq!(app.cancel_requested, Some(CancelTarget::Other(id)));
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
}
