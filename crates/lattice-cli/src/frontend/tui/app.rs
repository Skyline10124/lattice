use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use base64::Engine as _;
use crossterm::event::{KeyEvent, MouseEvent};
use lattice_core::types::Role;
use lattice_plugin::registry::PluginRegistry;
use lattice_runtime::{AgentHandleId, Runtime, TurnCompletion, TurnId, TurnStatus};
use tokio::sync::mpsc::UnboundedSender;

use super::activity::{ActivityContext, ActivityState, ToolOutputUpdate};
use super::agent_runner::{AgentBuildRequest, AgentBuildStatus, AgentStreamRequest, RebuiltAgent};
use super::commands::CommandRegistry;
use super::composer::ComposerState;
use super::event::Event;
use super::keymap::RuntimeKeymap;
use super::pager::PagerOverlay;
use super::plugin_mode::{runtime_plugin_overlay, PreparedSubmission, RunPluginContext};
use super::state::{AppStatus, ChatMessage, FileSnapshot, MenuState, SlashSuggestion};
use super::theme::Theme;
use super::turn::{StreamOutcome, StreamTokenUpdate, TurnState};
use super::widgets::message::message_lines;
use super::widgets::transcript::transcript_lines;

/// Send text to the terminal clipboard via OSC 52.
/// Works in most modern terminals (kitty, wezterm, foot, iterm2, windows terminal).
fn write_osc52_clipboard(text: &str) {
    use std::io::Write;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let osc = format!("\x1b]52;c;{}\x07", encoded);
    let _ = std::io::stdout().write_all(osc.as_bytes());
    let _ = std::io::stdout().flush();
}

type ClipboardSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Application state.
pub(super) struct App {
    pub(super) should_quit: bool,
    pub(super) messages: Vec<ChatMessage>,
    pub(super) composer: ComposerState,
    pub(super) status: AppStatus,
    pub(super) current_model: String,
    pub(super) previous_model: String,
    pub(super) current_provider: String,
    pub(super) token_count: usize,
    pub(super) scroll_offset: usize,
    pub(super) help_open: bool,
    pub(super) transcript_mode: bool,
    pub(super) spinner_index: usize,
    pub(super) provider_override: Option<String>,
    pub(super) credentials: HashMap<String, String>,
    pub(super) workdir: std::path::PathBuf,
    pub(super) save_sessions: bool,
    pub(super) security: crate::security::RuntimeSecurity,
    pub(super) security_config: crate::config::SecurityConfig,
    pub(super) plugin_registry: Option<Arc<PluginRegistry>>,
    pub(super) session: Option<crate::session::Session>,
    pub(super) event_tx: Option<UnboundedSender<Event>>,
    pub(super) runtime: Runtime,
    pub(super) normal_handle: Option<AgentHandleId>,
    pub(super) plugin_handles: HashMap<String, AgentHandleId>,
    pub(super) active_handle: Option<AgentHandleId>,
    last_approval_handle: Option<AgentHandleId>,
    pub(super) active_runtime_turn: Option<TurnId>,
    pending_cancelled_turn_id: Option<u64>,
    pending_cancelled_handle: Option<AgentHandleId>,
    pending_cancelled_runtime_turn: Option<TurnId>,
    pub(super) turn: TurnState,
    prepared_submission: Option<PreparedSubmission>,
    pub(super) activity: ActivityState,
    pub(super) thinking_effort: Option<String>,
    pub(super) context_limit: u32,
    pub(super) menu: Option<MenuState>,
    pub(super) pager: Option<PagerOverlay>,
    pub(super) keymap: RuntimeKeymap,
    clipboard: ClipboardSink,
}

impl App {
    pub(super) fn new() -> Self {
        Self {
            should_quit: false,
            messages: vec![],
            composer: ComposerState::default(),
            status: AppStatus::Ready,
            current_model: "deepseek-v4-flash".into(),
            previous_model: String::new(),
            current_provider: "".into(),
            token_count: 0,
            scroll_offset: 0,
            help_open: false,
            transcript_mode: false,
            spinner_index: 0,
            provider_override: None,
            credentials: HashMap::new(),
            workdir: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            save_sessions: false,
            security_config: crate::config::SecurityConfig::default(),
            security: crate::security::default_runtime_security(
                &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            )
            .expect("default runtime security should build"),
            plugin_registry: crate::plugins::build_plugin_registry().ok(),
            session: None,
            event_tx: None,
            runtime: crate::runtime::model_runtime(HashMap::new()),
            normal_handle: None,
            plugin_handles: HashMap::new(),
            active_handle: None,
            last_approval_handle: None,
            active_runtime_turn: None,
            pending_cancelled_turn_id: None,
            pending_cancelled_handle: None,
            pending_cancelled_runtime_turn: None,
            turn: TurnState::default(),
            prepared_submission: None,
            activity: ActivityState::default(),
            menu: None,
            pager: None,
            keymap: RuntimeKeymap::new(),
            thinking_effort: None,
            context_limit: 128000,
            clipboard: Arc::new(write_osc52_clipboard),
        }
    }

    pub(super) fn load_session(&mut self, session: crate::session::Session) {
        self.cancel_active_turn();
        self.messages = super::session_state::messages_from_session(&session);
        self.current_provider = session.provider.clone();
        self.session = Some(session);
        self.clear_runtime_handles();
        self.previous_model.clear();
        self.scroll_offset = 0;
        self.turn.reset_runtime();
        self.clear_search();
        self.prepared_submission = None;
    }

    fn begin_turn(&mut self, user: String) -> u64 {
        self.turn.begin(user, self.messages.len())
    }

    pub(super) fn cancel_active_turn(&mut self) {
        if let Some(handle_id) = self.active_handle.clone() {
            let _ = self.runtime.cancel_turn(&handle_id);
            if let Some(turn_id) = self.turn.active_turn_id {
                self.pending_cancelled_turn_id = Some(turn_id);
                self.pending_cancelled_handle = Some(handle_id);
                self.pending_cancelled_runtime_turn = self.active_runtime_turn.clone();
            }
        }
        self.turn.cancel(&mut self.status);
        self.active_handle = None;
        self.active_runtime_turn = None;
    }

    pub(super) fn accepts_turn(&self, turn_id: u64) -> bool {
        self.turn.accepts(turn_id)
    }

    pub(super) fn accepts_runtime_turn(
        &self,
        turn_id: u64,
        handle_id: &AgentHandleId,
        runtime_turn_id: &TurnId,
    ) -> bool {
        let active_matches = self.active_handle.as_ref() == Some(handle_id)
            && self.active_runtime_turn.as_ref() == Some(runtime_turn_id)
            && (self.turn.accepts(turn_id) || self.turn.active_turn_id.is_none());
        let pending_cancel_matches =
            self.accepts_pending_cancelled_runtime_turn(turn_id, handle_id, runtime_turn_id)
                && (self.turn.accepts(turn_id) || self.turn.active_turn_id.is_none());
        active_matches || pending_cancel_matches
    }

    pub(super) fn mark_runtime_turn_started(
        &mut self,
        turn_id: u64,
        handle_id: AgentHandleId,
        runtime_turn_id: TurnId,
    ) {
        if self.pending_cancelled_turn_id == Some(turn_id)
            && self.pending_cancelled_handle.as_ref() == Some(&handle_id)
            && self.pending_cancelled_runtime_turn.is_none()
        {
            self.pending_cancelled_runtime_turn = Some(runtime_turn_id);
            return;
        }
        if self.turn.accepts(turn_id) && self.active_handle.as_ref() == Some(&handle_id) {
            self.active_runtime_turn = Some(runtime_turn_id);
        }
    }

    pub(super) fn clear_runtime_handles(&mut self) {
        self.clear_plugin_runtime_overlays();
        self.normal_handle = None;
        self.plugin_handles.clear();
        self.active_handle = None;
        self.last_approval_handle = None;
        self.active_runtime_turn = None;
        self.clear_pending_cancelled_turn();
    }

    fn clear_plugin_runtime_overlays(&self) {
        for handle_id in self.plugin_handles.values() {
            if let Err(err) = self.runtime.set_agent_plugin_overlay(handle_id, None) {
                tracing::debug!("failed to clear plugin overlay for handle {handle_id}: {err}");
            }
        }
    }

    fn accepts_pending_cancelled_runtime_turn(
        &self,
        turn_id: u64,
        handle_id: &AgentHandleId,
        runtime_turn_id: &TurnId,
    ) -> bool {
        self.pending_cancelled_turn_id == Some(turn_id)
            && self.pending_cancelled_handle.as_ref() == Some(handle_id)
            && self.pending_cancelled_runtime_turn.as_ref() == Some(runtime_turn_id)
    }

    fn clear_pending_cancelled_turn(&mut self) {
        self.pending_cancelled_turn_id = None;
        self.pending_cancelled_handle = None;
        self.pending_cancelled_runtime_turn = None;
    }

    #[cfg(test)]
    fn set_clipboard_sink(&mut self, sink: ClipboardSink) {
        self.clipboard = sink;
    }

    pub(super) fn copy_to_clipboard(&self, text: &str) {
        (self.clipboard)(text);
    }

    pub(super) fn toggle_recent_reasoning(&mut self) {
        if let Some(msg) = self.messages.iter_mut().rev().find(|msg| {
            msg.role == Role::Assistant && msg.reasoning.as_deref().is_some_and(|r| !r.is_empty())
        }) {
            msg.reasoning_collapsed = !msg.reasoning_collapsed;
            msg.invalidate_cache();
        } else {
            self.transcript_mode = !self.transcript_mode;
        }
    }

    pub(super) fn tick(&mut self) {
        self.spinner_index = self.spinner_index.wrapping_add(1);
    }

    pub(super) async fn reap_audit(&self) {
        crate::security::reap_audit(&self.security).await;
    }

    pub(super) async fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        super::interaction::handle_key(self, key).await
    }

    /// Insert text at the cursor (e.g. from paste or IME commit).
    pub(super) fn insert_text(&mut self, text: &str) {
        self.composer.insert_text(text);
    }

    pub(super) async fn handle_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        super::interaction::handle_mouse(self, mouse).await
    }

    pub(super) async fn submit(&mut self) -> Result<()> {
        let text = self.composer.input.trim().to_string();
        if text.is_empty() {
            return Ok(());
        }
        self.composer.add_to_history(text.clone());
        if self.handle_slash_command(&text) {
            if let Some(prepared) = self.prepared_submission.take() {
                return self.submit_prepared(prepared).await;
            }
            self.composer.clear();
            self.scroll_offset = 0;
            return Ok(());
        }
        if self.turn.is_active(&self.status) {
            self.queue_input(text);
            self.composer.clear();
            return Ok(());
        }

        if let Some(cmd) = text.strip_prefix('!') {
            self.messages
                .push(ChatMessage::new(Role::User, text.clone()));
            self.composer.clear();
            self.scroll_offset = 0;
            self.status = AppStatus::Streaming;
            self.turn.stream_started = Some(std::time::Instant::now());

            self.messages
                .push(super::bash_command::run(&self.security, &self.workdir, cmd).await);
            self.status = AppStatus::Ready;
            self.turn.stream_started = None;
            return Ok(());
        }

        self.submit_model_turn(text.clone(), text, None).await
    }

    async fn submit_prepared(&mut self, prepared: PreparedSubmission) -> Result<()> {
        if self.turn.is_active(&self.status) {
            let queued = match prepared.raw_prompt.as_deref() {
                Some(raw) => prepared
                    .plugin
                    .as_ref()
                    .map(|plugin| format!("/plugin {} {}", plugin.name, raw))
                    .unwrap_or_else(|| prepared.display.clone()),
                None => prepared.display.clone(),
            };
            self.queue_input(queued);
            self.composer.clear();
            return Ok(());
        }
        self.submit_model_turn(prepared.display, prepared.prompt, prepared.plugin)
            .await
    }

    async fn submit_model_turn(
        &mut self,
        display_text: String,
        prompt_text: String,
        plugin: Option<RunPluginContext>,
    ) -> Result<()> {
        self.messages
            .push(ChatMessage::new(Role::User, display_text.clone()));
        self.token_count +=
            lattice_core::tokens::TokenEstimator::estimate_text(&display_text) as usize;
        self.composer.clear();
        self.scroll_offset = 0;
        self.status = AppStatus::Streaming;
        self.turn.stream_started = Some(std::time::Instant::now());
        self.turn.reasoning_started = None;
        self.turn.reasoning_duration = None;
        let turn_id = self.begin_turn(display_text);

        self.messages
            .push(ChatMessage::new(Role::Assistant, String::new()));

        let tx = match self.event_tx.clone() {
            Some(tx) => tx,
            None => {
                self.status = AppStatus::Error("Event channel not initialized".into());
                self.cancel_active_turn();
                return Ok(());
            }
        };

        let profile_name = plugin
            .as_ref()
            .map(|plugin| format!("plugin:{}", plugin.name))
            .unwrap_or_else(|| "coding".into());
        let existing_handle = self.existing_handle_for_submission(plugin.as_ref());

        let handle_id = match super::agent_runner::ensure_agent(AgentBuildRequest {
            turn_id,
            tx: tx.clone(),
            runtime: self.runtime.clone(),
            existing_handle,
            profile_name,
            current_model: self.current_model.clone(),
            previous_model: self.previous_model.clone(),
            provider_override: self.provider_override.clone(),
            workdir: self.workdir.clone(),
            previous_session: self.session.clone(),
            thinking_effort: self.thinking_effort.clone(),
            security: self.security.clone(),
        })
        .await
        {
            AgentBuildStatus::Ready(outcome) => {
                if let Some(rebuilt) = outcome.rebuilt {
                    self.apply_rebuilt_agent(rebuilt);
                }
                if let Some(plugin) = plugin.as_ref() {
                    self.plugin_handles
                        .insert(plugin.name.clone(), outcome.handle_id.clone());
                } else {
                    self.normal_handle = Some(outcome.handle_id.clone());
                }
                self.active_handle = Some(outcome.handle_id.clone());
                outcome.handle_id
            }
            AgentBuildStatus::Failed => {
                self.status = AppStatus::Ready;
                return Ok(());
            }
        };

        self.apply_runtime_plugin_context(plugin.as_ref(), &handle_id)?;

        super::agent_runner::spawn_stream(AgentStreamRequest {
            runtime: self.runtime.clone(),
            handle_id,
            prompt_text,
            turn_id,
            tx,
            workdir: self.workdir.clone(),
            security: self.security.clone(),
            persist_session: self.save_sessions,
            session_id: self.session.as_ref().map(|session| session.id.clone()),
        });

        Ok(())
    }

    fn existing_handle_for_submission(
        &self,
        plugin: Option<&RunPluginContext>,
    ) -> Option<AgentHandleId> {
        match plugin {
            Some(plugin) => self.plugin_handles.get(&plugin.name).cloned(),
            None => self.normal_handle.clone(),
        }
    }

    fn apply_rebuilt_agent(&mut self, rebuilt: RebuiltAgent) {
        self.context_limit = rebuilt.context_limit;
        self.current_model = rebuilt.model;
        self.current_provider = rebuilt.provider;
        self.previous_model = self.current_model.clone();
    }

    fn apply_runtime_plugin_context(
        &mut self,
        plugin: Option<&RunPluginContext>,
        handle_id: &AgentHandleId,
    ) -> Result<()> {
        match plugin {
            Some(plugin) => {
                self.runtime
                    .set_agent_plugin_overlay(handle_id, Some(runtime_plugin_overlay(plugin)))?;
            }
            None => {
                self.clear_plugin_runtime_overlays();
                let _ = self.runtime.set_agent_plugin_overlay(handle_id, None);
            }
        }
        Ok(())
    }

    fn handle_slash_command(&mut self, text: &str) -> bool {
        CommandRegistry::execute(self, text)
    }

    pub(super) fn reset_conversation(&mut self) {
        self.messages.clear();
        self.composer.clear();
        self.scroll_offset = 0;
        self.help_open = false;
        self.token_count = 0;
        self.turn.reset_runtime();
        self.clear_search();
        self.clear_runtime_handles();
        self.previous_model.clear();
        self.session = None;
        self.prepared_submission = None;
        self.status = AppStatus::Ready;
    }

    pub(super) fn push_system_message(&mut self, content: &str) {
        self.messages
            .push(ChatMessage::new(Role::System, content.to_string()));
    }

    pub(super) fn switch_permissions(&mut self, mode: &str) {
        let normalized = mode.trim().to_ascii_lowercase();
        if !permission_modes()
            .iter()
            .any(|candidate| candidate == &normalized)
        {
            self.push_system_message("Usage: /permissions <project|strict|permissive>");
            return;
        }

        let mut next_config = self.security_config.clone();
        next_config.sandbox_mode = normalized.clone();
        match crate::security::build_runtime_security(&next_config, &self.workdir) {
            Ok(security) => {
                self.cancel_active_turn();
                self.security_config = next_config;
                self.security = security;
                self.clear_runtime_handles();
                self.previous_model.clear();
                self.push_system_message(&format!(
                    "Permissions switched to '{}'. Agent will rebuild next turn.",
                    self.security.mode_label
                ));
            }
            Err(err) => {
                self.push_system_message(&format!("Permission switch failed: {err}"));
            }
        }
    }

    pub(super) fn approve_pending_proposal(&mut self, selector: &str) {
        let Some(handle_id) = self.approval_handle_for_selector(selector) else {
            self.push_system_message("Approval failed: no active agent state yet.");
            return;
        };
        let result = self
            .runtime
            .approve_handle_pending_proposal(&handle_id, selector);
        self.push_approval_result(result);
    }

    fn approval_handle_for_selector(&self, _selector: &str) -> Option<AgentHandleId> {
        self.active_handle
            .clone()
            .or_else(|| self.last_approval_handle.clone())
            .or_else(|| self.normal_handle.clone())
    }

    fn push_approval_result(
        &mut self,
        result: std::result::Result<String, lattice_runtime::RuntimeError>,
    ) {
        match result {
            Ok(message) => self.push_system_message(&format!(
                "{message}. Continue or retry the blocked request when ready."
            )),
            Err(lattice_runtime::RuntimeError::HandleBusy(_)) => self.push_system_message(
                "Approval failed: agent is busy; try again after the current turn finishes.",
            ),
            Err(err) => self.push_system_message(&format!("Approval failed: {err}")),
        }
    }

    pub(super) fn plugin_count(&self) -> usize {
        super::plugin_mode::plugin_count(self.plugin_registry.as_ref())
    }

    pub(super) fn active_plugin_name(&self) -> Option<String> {
        let handle_id = self.active_handle.as_ref()?;
        self.runtime
            .agent_handle_snapshot(handle_id)
            .ok()
            .and_then(|snapshot| snapshot.plugin_overlay_name)
    }

    pub(super) fn plugin_names(&self) -> Vec<String> {
        super::plugin_mode::plugin_names(self.plugin_registry.as_ref())
    }

    pub(super) fn has_plugin(&self, name: &str) -> bool {
        super::plugin_mode::has_plugin(self.plugin_registry.as_ref(), name)
    }

    pub(super) fn plugin_summary(&self, name: &str) -> Option<(String, String)> {
        super::plugin_mode::plugin_summary(self.plugin_registry.as_ref(), name)
    }

    pub(super) fn prepare_plugin_prompt(&mut self, name: &str, prompt: String) -> Result<()> {
        self.prepared_submission = Some(super::plugin_mode::prepare_submission(
            self.plugin_registry.as_ref(),
            &self.workdir,
            name,
            prompt,
        )?);
        Ok(())
    }

    pub(super) fn slash_suggestions(&self) -> Vec<SlashSuggestion> {
        self.composer.slash_suggestions()
    }

    pub(super) fn accept_selected_suggestion(&mut self) {
        self.composer.accept_selected_suggestion();
    }

    pub(super) fn queue_input(&mut self, text: String) {
        let count = self.turn.queue_input(text);
        self.push_system_message(&format!(
            "Queued prompt #{count}. Press Enter on \"Send queued prompt\" when ready."
        ));
    }

    pub(super) fn queue_runtime_busy_prompt(&mut self, turn_id: u64, prompt_text: String) {
        if !self.turn.accepts(turn_id) {
            return;
        }
        self.turn.finish();
        self.status = AppStatus::Ready;
        self.active_handle = None;
        self.active_runtime_turn = None;
        self.queue_input(prompt_text);
        self.push_system_message("Agent is still working on this conversation. Prompt queued.");
    }

    pub(super) fn queue_prompt_active(&self) -> bool {
        self.status == AppStatus::Ready
            && self.composer.is_empty()
            && !self.turn.queued_inputs.is_empty()
    }

    pub(super) async fn submit_next_queued(&mut self) -> Result<()> {
        if self.status != AppStatus::Ready || self.turn.active_turn_id.is_some() {
            return Ok(());
        }
        let Some(next) = self.turn.queued_inputs.pop_front() else {
            return Ok(());
        };
        self.composer.set_input(next);
        self.submit().await
    }

    pub(super) fn mode_label(&self) -> &'static str {
        if self.composer.input.starts_with('!') {
            "bash"
        } else {
            "prompt"
        }
    }

    pub(super) fn spinner(&self) -> &'static str {
        const FRAMES: &[&str] = &["-", "\\", "|", "/"];
        FRAMES[self.spinner_index % FRAMES.len()]
    }

    /// Apply a streaming token to the last assistant message.
    pub(super) fn apply_stream_token(
        &mut self,
        turn_id: u64,
        content: String,
        reasoning: Option<String>,
        done: bool,
        error: Option<String>,
    ) {
        let outcome = self.turn.apply_stream_token(
            &mut self.messages,
            &mut self.status,
            &mut self.token_count,
            StreamTokenUpdate {
                turn_id,
                content,
                reasoning,
                done,
                error,
            },
        );

        if matches!(
            outcome,
            StreamOutcome::Finished { .. } | StreamOutcome::Errored
        ) {
            // Recalculate total token count from all messages for accuracy.
            self.token_count = super::session_state::recount_tokens(&self.messages);
            self.refresh_search_matches();
            // Cache is populated lazily on next render at message_lines().
        }
    }

    pub(super) fn apply_runtime_completion(
        &mut self,
        turn_id: u64,
        handle_id: &AgentHandleId,
        runtime_turn_id: &TurnId,
        status: TurnStatus,
        completion: Option<&TurnCompletion>,
        error: Option<String>,
    ) {
        if !self.accepts_runtime_turn(turn_id, handle_id, runtime_turn_id) {
            return;
        }
        let pending_cancelled =
            self.accepts_pending_cancelled_runtime_turn(turn_id, handle_id, runtime_turn_id);

        if let Some(completion) = completion {
            self.current_model = completion.model.clone();
            self.current_provider = completion.provider.clone();
            if completion
                .task_state_snapshot
                .latest_pending_approval()
                .is_some()
            {
                self.last_approval_handle = Some(handle_id.clone());
            }
            if let Some(usage) = completion.token_usage.as_ref() {
                self.token_count = usage.total_tokens as usize;
            } else {
                self.token_count = super::session_state::recount_tokens(&self.messages);
            }
        }

        let effective_status = completion.map(|c| c.status).unwrap_or(status);
        match effective_status {
            TurnStatus::Completed => {
                self.status = AppStatus::Ready;
                self.turn.finish();
            }
            TurnStatus::Cancelled => {
                self.status = AppStatus::Ready;
                self.turn.finish();
                self.push_system_message("Cancelled. Partial output kept.");
            }
            TurnStatus::Error => {
                let message = completion
                    .and_then(|completion| completion.error.clone())
                    .or(error)
                    .unwrap_or_else(|| "runtime turn failed".into());
                if self.turn.accepts(turn_id) {
                    self.apply_stream_token(turn_id, String::new(), None, false, Some(message));
                } else {
                    self.status = AppStatus::Error(message);
                }
            }
        }
        self.active_runtime_turn = None;
        self.active_handle = None;
        if pending_cancelled {
            self.clear_pending_cancelled_turn();
        }
        self.refresh_search_matches();
    }

    pub(super) fn apply_tool_output(
        &mut self,
        turn_id: u64,
        call_id: String,
        name: String,
        arguments: String,
        result: Option<String>,
        file_before: Option<FileSnapshot>,
    ) {
        let mut context = ActivityContext {
            turn: &mut self.turn,
            messages: &mut self.messages,
            workdir: &self.workdir,
            security: &self.security,
        };
        let changed = self.activity.apply_tool_output(
            &mut context,
            ToolOutputUpdate {
                turn_id,
                call_id,
                name,
                arguments,
                result,
                file_before,
            },
        );
        if !changed {
            return;
        }
        self.scroll_offset = 0;
        self.refresh_search_matches();
    }

    pub(super) fn toggle_tool_expand(&mut self) {
        if let Some(tool) = self
            .messages
            .iter_mut()
            .rev()
            .find(|msg| msg.role == Role::Tool)
        {
            tool.collapsed = !tool.collapsed;
            tool.invalidate_cache();
        }
    }

    pub(super) fn open_transcript_pager(&mut self) {
        let theme = Theme::catppuccin_mocha();
        let lines = transcript_lines(self, &theme, 88);
        self.pager = Some(PagerOverlay::new("Transcript", lines));
    }

    pub(super) fn open_recent_tool_pager(&mut self) {
        let theme = Theme::catppuccin_mocha();
        let Some(tool) = self.messages.iter().rev().find(|msg| msg.tool.is_some()) else {
            self.push_system_message("No tool output to show.");
            return;
        };
        let lines = message_lines(tool, &theme, true, 88);
        self.pager = Some(PagerOverlay::new("Tool detail", lines));
    }

    pub(super) fn copy_last_assistant(&mut self) {
        let text = self
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant && !m.content.is_empty())
            .map(|m| m.content.clone())
            .unwrap_or_default();
        if !text.is_empty() {
            self.copy_to_clipboard(&text);
            self.push_system_message("Copied to clipboard");
        }
    }
}

pub(super) fn permission_modes() -> Vec<String> {
    ["project", "strict", "permissive"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

pub(super) fn summarize_prompt(text: &str, max_chars: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    for (idx, ch) in one_line.chars().enumerate() {
        if idx >= max_chars {
            out.push_str("...");
            return out;
        }
        out.push(ch);
    }
    out
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::super::state::{MenuKind, TextSelection, ToolStatus};
    use super::*;
    use crossterm::event::KeyCode;
    use lattice_core::catalog::{ApiProtocol, CatalogProviderEntry, ModelCatalogEntry};
    use lattice_core::streaming::TokenUsage;
    use lattice_core::types::ToolDefinition;
    use lattice_orchestrator::{AgentPreset, AgentTurnSpec};
    use lattice_sandbox::{ApprovalKind, ApprovedRule, RiskClass};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn test_session() -> crate::session::Session {
        crate::session::Session {
            id: "session-1".into(),
            model: "old-model".into(),
            provider: "old-provider".into(),
            title: None,
            parent_id: None,
            branch: None,
            messages: vec![
                crate::session::SessionMessage {
                    role: "user".into(),
                    content: "old question".into(),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                    artifact_ids: Vec::new(),
                    compact: None,
                },
                crate::session::SessionMessage {
                    role: "assistant".into(),
                    content: "old answer".into(),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                    artifact_ids: Vec::new(),
                    compact: None,
                },
            ],
            created_at: "2026-05-05T00:00:00+08:00".into(),
            updated_at: "2026-05-05T00:00:00+08:00".into(),
        }
    }

    fn test_completion(
        handle_id: AgentHandleId,
        runtime_turn_id: TurnId,
        status: TurnStatus,
    ) -> TurnCompletion {
        TurnCompletion {
            handle_id,
            turn_id: runtime_turn_id,
            status,
            assistant_text: "answer".into(),
            token_usage: Some(TokenUsage {
                prompt_tokens: 2,
                completion_tokens: 3,
                total_tokens: 5,
            }),
            task_state_snapshot: lattice_agent::TaskState::new(),
            final_report: None,
            turn_manifests: Vec::new(),
            model: "runtime-model".into(),
            provider: "runtime-provider".into(),
            saved_session_id: Some("runtime-session".into()),
            error: (status == TurnStatus::Error).then(|| "runtime failed".into()),
        }
    }

    fn register_test_model(runtime: &mut Runtime, name: &str) {
        runtime
            .register_model(ModelCatalogEntry {
                canonical_id: name.to_string(),
                context_length: 8192,
                providers: vec![CatalogProviderEntry {
                    provider_id: "ollama".to_string(),
                    api_model_id: name.to_string(),
                    priority: 1,
                    credential_keys: HashMap::new(),
                    base_url: Some("http://127.0.0.1:9".into()),
                    api_protocol: ApiProtocol::OpenAiChat,
                    provider_specific: HashMap::new(),
                }],
                aliases: Vec::new(),
            })
            .unwrap();
    }

    fn app_with_test_runtime(model: &str) -> App {
        let mut runtime = Runtime::builder().build();
        register_test_model(&mut runtime, model);
        let mut app = App::new();
        app.runtime = runtime;
        app.current_model = model.into();
        app.previous_model = model.into();
        app
    }

    fn test_turn_spec(app: &App, profile_name: &str) -> AgentTurnSpec {
        let workdir = app.workdir.to_string_lossy().to_string();
        AgentTurnSpec {
            profile_name: profile_name.into(),
            model: app.current_model.clone(),
            provider_override: app.provider_override.clone(),
            system_prompt: Some(lattice_runtime::coding_system_prompt(
                &app.workdir,
                &app.workdir,
            )),
            prompt_spec: None,
            capabilities: lattice_orchestrator::CapabilitiesConfig {
                presets: vec!["coding".into()],
                bundles: Vec::new(),
            },
            tools: Default::default(),
            behavior: Default::default(),
            handoff: Default::default(),
            memory: Default::default(),
            project_root: Some(workdir.clone()),
            cwd: Some(workdir),
            thinking_effort: None,
            preset: Some(AgentPreset::Coding),
        }
    }

    fn create_test_handle(app: &App, profile_name: &str) -> AgentHandleId {
        app.runtime
            .get_or_create_agent_handle_for_session(
                test_turn_spec(app, profile_name),
                app.security.clone(),
                None,
            )
            .unwrap()
    }

    fn test_plugin_context(name: &str) -> RunPluginContext {
        RunPluginContext {
            name: name.into(),
            system_prompt: "Review code carefully.".into(),
            output_contract_delta: Some("Return JSON.".into()),
            tools: vec![ToolDefinition::new(
                "plugin_read".into(),
                "Read plugin data".into(),
                serde_json::json!({"type": "object"}),
            )],
        }
    }

    mod cancel_preserves {
        use super::*;

        fn active_cancel_app() -> (App, AgentHandleId, TurnId) {
            let mut app = App::new();
            let handle_id = AgentHandleId::new();
            let runtime_turn_id = TurnId::new();
            app.active_handle = Some(handle_id.clone());
            app.active_runtime_turn = Some(runtime_turn_id.clone());
            app.status = AppStatus::Streaming;
            app.turn.active_turn_id = Some(11);
            app.turn.active_assistant_index = Some(0);
            app.messages.push(ChatMessage::new(
                Role::Assistant,
                "partial assistant output".into(),
            ));
            (app, handle_id, runtime_turn_id)
        }

        #[test]
        fn cancel_preserves_queued_inputs() {
            let (mut app, _handle_id, _runtime_turn_id) = active_cancel_app();
            app.turn.queued_inputs.push_back("queued follow-up".into());

            app.cancel_active_turn();

            assert_eq!(app.turn.queued_inputs.len(), 1);
            assert_eq!(
                app.turn.queued_inputs.front().map(String::as_str),
                Some("queued follow-up")
            );
        }

        #[test]
        fn cancel_preserves_partial_assistant_output() {
            let (mut app, handle_id, runtime_turn_id) = active_cancel_app();
            app.turn.queued_inputs.push_back("queued follow-up".into());

            app.cancel_active_turn();
            let completion = test_completion(
                handle_id.clone(),
                runtime_turn_id.clone(),
                TurnStatus::Cancelled,
            );
            app.apply_runtime_completion(
                11,
                &handle_id,
                &runtime_turn_id,
                TurnStatus::Cancelled,
                Some(&completion),
                None,
            );

            assert_eq!(app.messages[0].content, "partial assistant output");
            assert_eq!(app.turn.queued_inputs.len(), 1);
            assert_eq!(
                app.messages.last().unwrap().content,
                "Cancelled. Partial output kept."
            );
        }

        #[test]
        fn cancelled_completion_is_non_success() {
            let (mut app, handle_id, runtime_turn_id) = active_cancel_app();

            app.cancel_active_turn();
            let mut completion = test_completion(
                handle_id.clone(),
                runtime_turn_id.clone(),
                TurnStatus::Cancelled,
            );
            completion.final_report = Some("successful final report".into());
            app.apply_runtime_completion(
                11,
                &handle_id,
                &runtime_turn_id,
                TurnStatus::Cancelled,
                Some(&completion),
                None,
            );

            assert_eq!(app.status, AppStatus::Ready);
            assert!(app
                .messages
                .iter()
                .all(|message| !message.content.contains("successful final report")));
            assert_eq!(
                app.messages.last().unwrap().content,
                "Cancelled. Partial output kept."
            );
        }
    }

    #[test]
    fn stream_error_sets_status_and_appends_visible_error() {
        let mut app = App::new();
        app.status = AppStatus::Streaming;
        app.turn.active_turn_id = Some(7);
        app.turn.active_assistant_index = Some(0);
        app.messages
            .push(ChatMessage::new(Role::Assistant, "partial response".into()));

        app.apply_stream_token(
            7,
            String::new(),
            None,
            false,
            Some("provider failed".into()),
        );

        assert_eq!(app.status, AppStatus::Error("provider failed".into()));
        assert_eq!(
            app.messages.last().unwrap().content,
            "partial response\nError: provider failed"
        );
    }

    #[test]
    fn stale_turn_tokens_are_ignored() {
        let mut app = App::new();
        app.turn.active_turn_id = Some(9);
        app.turn.active_assistant_index = Some(0);
        app.messages
            .push(ChatMessage::new(Role::Assistant, String::new()));

        app.apply_stream_token(8, "ignored".into(), None, false, None);

        assert!(app.messages[0].content.is_empty());
    }

    #[test]
    fn slash_model_switch_updates_state() {
        let mut app = App::new();
        app.current_provider = "mock".into();
        app.previous_model = "old".into();

        let handled = app.handle_slash_command("/model llama-3.1").then_some(());

        assert!(handled.is_some());
        assert_eq!(app.current_model, "llama-3.1");
        assert!(app.current_provider.is_empty());
        assert_eq!(app.status, AppStatus::Ready);
        assert!(app.normal_handle.is_none());
        assert_eq!(
            app.messages.last().unwrap().content,
            "Model switched to llama-3.1."
        );
    }

    #[test]
    fn slash_suggestions_filter_prefixes() {
        let mut app = App::new();
        app.composer.set_input("/st");

        let suggestions = app.slash_suggestions();

        assert!(suggestions.iter().any(|s| s.command == "/status"));
        assert!(!suggestions.iter().any(|s| s.command == "/clear"));
    }

    #[test]
    fn slash_suggestions_fuzzy_match_command_name() {
        let mut app = App::new();
        app.composer.set_input("/trn");

        let suggestions = app.slash_suggestions();

        assert!(suggestions.iter().any(|s| s.command == "/transcript"));
    }

    #[test]
    fn accept_selected_suggestion_completes_command_name() {
        let mut app = App::new();
        app.composer.set_input("/f");

        app.accept_selected_suggestion();

        assert_eq!(app.composer.input, "/find ");
        assert_eq!(app.composer.input_cursor, app.composer.input.len());
    }

    #[tokio::test]
    async fn transcript_key_opens_pager_and_pager_close_consumes_key() {
        let mut app = App::new();
        app.messages
            .push(ChatMessage::new(Role::Assistant, "answer".into()));

        app.handle_key(KeyEvent::new(
            KeyCode::Char('t'),
            crossterm::event::KeyModifiers::CONTROL,
        ))
        .await
        .unwrap();

        assert!(app.pager.is_some());

        app.handle_key(KeyEvent::new(
            KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        ))
        .await
        .unwrap();

        assert!(app.pager.is_none());
    }

    #[test]
    fn slash_detail_without_tool_reports_no_output() {
        let mut app = App::new();

        assert!(app.handle_slash_command("/detail"));

        assert!(app.pager.is_none());
        assert_eq!(
            app.messages.last().unwrap().content,
            "No tool output to show."
        );
    }

    #[test]
    fn queue_input_records_prompt_for_later() {
        let mut app = App::new();

        app.queue_input("next task".into());

        assert_eq!(app.turn.queued_inputs.len(), 1);
        assert_eq!(app.turn.queued_inputs.front().unwrap(), "next task");
        assert_eq!(
            app.messages.last().unwrap().content,
            "Queued prompt #1. Press Enter on \"Send queued prompt\" when ready."
        );
    }

    #[tokio::test]
    async fn submit_next_queued_waits_until_ready() {
        let mut app = App::new();
        app.status = AppStatus::Streaming;
        app.turn.queued_inputs.push_back("/status".into());

        app.submit_next_queued().await.unwrap();

        assert_eq!(app.turn.queued_inputs.len(), 1);
        assert!(app.messages.is_empty());
    }

    #[tokio::test]
    async fn submit_next_queued_runs_slash_command() {
        let mut app = App::new();
        app.turn.queued_inputs.push_back("/status".into());

        app.submit_next_queued().await.unwrap();

        assert!(app.turn.queued_inputs.is_empty());
        assert!(app.messages.last().unwrap().content.contains("model="));
    }

    #[test]
    fn slash_queue_clear_drops_pending_prompts() {
        let mut app = App::new();
        app.turn.queued_inputs.push_back("one".into());
        app.turn.queued_inputs.push_back("two".into());

        assert!(app.handle_slash_command("/queue clear"));

        assert!(app.turn.queued_inputs.is_empty());
        assert_eq!(
            app.messages.last().unwrap().content,
            "Cleared 2 queued prompt(s)."
        );
    }

    #[test]
    fn slash_permissions_switches_runtime_security() {
        let mut app = App::new();

        assert!(app.handle_slash_command("/permissions strict"));

        assert_eq!(app.security.mode_label, "strict");
        assert_eq!(app.security_config.sandbox_mode, "strict");
        assert!(app.normal_handle.is_none());
        assert!(app
            .messages
            .last()
            .unwrap()
            .content
            .contains("Permissions switched"));
    }

    #[test]
    fn slash_permissions_without_mode_opens_menu() {
        let mut app = App::new();

        assert!(app.handle_slash_command("/permissions"));

        let menu = app.menu.expect("permissions should open menu");
        assert!(matches!(menu.kind, MenuKind::Permissions));
        assert!(menu.options.iter().any(|mode| mode == "project"));
        assert!(!menu.options.iter().any(|mode| mode == "off"));
    }

    #[test]
    fn slash_permissions_off_is_not_advertised_or_accepted() {
        let mut app = App::new();

        assert!(!permission_modes().iter().any(|mode| mode == "off"));
        assert!(app.handle_slash_command("/permissions off"));

        assert_eq!(
            app.messages.last().unwrap().content,
            "Usage: /permissions <project|strict|permissive>"
        );
    }

    #[test]
    fn slash_approve_appears_in_suggestions() {
        let mut app = App::new();
        app.composer.set_input("/app");

        let suggestions = app.slash_suggestions();

        assert!(suggestions
            .iter()
            .any(|s| s.command == "/approve <id|last>"));
    }

    #[test]
    fn slash_approve_last_reports_missing_runtime_handle_without_submit() {
        let mut app = App::new();

        assert!(app.handle_slash_command("/approve last"));

        assert_eq!(app.security.approved_rules.lock().unwrap().len(), 0);
        assert!(app.turn.active_turn_id.is_none());
        assert!(app.turn.queued_inputs.is_empty());
        let msg = app.messages.last().unwrap().content.as_str();
        assert_eq!(msg, "Approval failed: no active agent state yet.");
    }

    #[test]
    fn slash_approve_unknown_id_reports_failure() {
        let mut app = App::new();

        assert!(app.handle_slash_command("/approve ap-404"));

        let msg = app.messages.last().unwrap().content.as_str();
        assert!(msg.starts_with("Approval failed:"));
        assert_eq!(app.security.approved_rules.lock().unwrap().len(), 0);
    }

    #[test]
    fn slash_approve_last_uses_runtime_handle_state() {
        let mut app = app_with_test_runtime("slash-approve-runtime-model");
        let handle = create_test_handle(&app, "coding");
        app.normal_handle = Some(handle);

        assert!(app.handle_slash_command("/approve last"));

        let msg = app.messages.last().unwrap().content.as_str();
        assert!(msg.contains("no pending approval proposal matches 'last'"));
        assert!(!msg.contains("no active agent state"));
    }

    #[test]
    fn slash_approve_last_prefers_latest_approval_handle() {
        let mut app = App::new();
        let normal_handle = AgentHandleId::new();
        let plugin_handle = AgentHandleId::new();
        app.normal_handle = Some(normal_handle);
        app.last_approval_handle = Some(plugin_handle.clone());

        assert_eq!(
            app.approval_handle_for_selector("last"),
            Some(plugin_handle)
        );
    }

    #[test]
    fn runtime_completion_tracks_pending_approval_handle() {
        let mut app = App::new();
        let handle_id = AgentHandleId::new();
        let runtime_turn_id = TurnId::new();
        let mut completion = test_completion(
            handle_id.clone(),
            runtime_turn_id.clone(),
            TurnStatus::Completed,
        );
        completion.task_state_snapshot.record_approval_proposal(
            "Allow cargo test".into(),
            "approve command_prefix cargo test".into(),
            "mutate".into(),
        );
        app.active_handle = Some(handle_id.clone());
        app.active_runtime_turn = Some(runtime_turn_id.clone());
        app.status = AppStatus::Streaming;
        app.turn.active_turn_id = Some(9);
        app.turn.active_assistant_index = Some(0);
        app.messages
            .push(ChatMessage::new(Role::Assistant, "blocked".into()));

        app.apply_runtime_completion(
            9,
            &handle_id,
            &runtime_turn_id,
            TurnStatus::Completed,
            Some(&completion),
            None,
        );

        assert_eq!(app.last_approval_handle, Some(handle_id.clone()));
        assert_eq!(app.approval_handle_for_selector("last"), Some(handle_id));
    }

    #[test]
    fn slash_approve_busy_reports_runtime_busy() {
        let mut app = App::new();

        app.push_approval_result(Err(lattice_runtime::RuntimeError::HandleBusy(
            "handle-1".into(),
        )));

        assert_eq!(
            app.messages.last().unwrap().content,
            "Approval failed: agent is busy; try again after the current turn finishes."
        );
    }

    #[test]
    fn slash_approve_success_does_not_replay_blocked_operation() {
        let mut app = App::new();

        app.push_approval_result(Ok("Approved proposal ap-1".into()));

        assert_eq!(
            app.messages.last().unwrap().content,
            "Approved proposal ap-1. Continue or retry the blocked request when ready."
        );
        assert!(app.turn.active_turn_id.is_none());
        assert!(app.turn.queued_inputs.is_empty());
    }

    #[test]
    fn slash_approve_without_selector_shows_usage() {
        let mut app = App::new();

        assert!(app.handle_slash_command("/approve"));

        assert_eq!(
            app.messages.last().unwrap().content,
            "Usage: /approve <id|last>"
        );
    }

    #[test]
    fn runtime_plugin_plugin_submission_sets_runtime_overlay() {
        let mut app = app_with_test_runtime("runtime-plugin-overlay-model");
        let plugin = test_plugin_context("CodeReview");
        let plugin_handle = create_test_handle(&app, "plugin:CodeReview");

        app.apply_runtime_plugin_context(Some(&plugin), &plugin_handle)
            .unwrap();

        let snapshot = app.runtime.agent_handle_snapshot(&plugin_handle).unwrap();
        assert_eq!(snapshot.plugin_overlay_name.as_deref(), Some("CodeReview"));
    }

    #[test]
    fn runtime_plugin_plugin_and_normal_handles_share_security_by_default() {
        let app = app_with_test_runtime("runtime-plugin-security-model");
        app.security
            .approve_rule(ApprovedRule {
                kind: ApprovalKind::CommandPrefix {
                    prefix: "cargo test".into(),
                },
                risk_class: RiskClass::Mutate,
            })
            .unwrap();

        let normal_handle = create_test_handle(&app, "coding");
        let plugin_handle = create_test_handle(&app, "plugin:CodeReview");

        let normal = app.runtime.agent_handle_snapshot(&normal_handle).unwrap();
        let plugin = app.runtime.agent_handle_snapshot(&plugin_handle).unwrap();
        assert_eq!(normal.approved_rule_count, 1);
        assert_eq!(plugin.approved_rule_count, 1);
    }

    #[test]
    fn runtime_plugin_switching_to_normal_clears_plugin_overlay() {
        let mut app = app_with_test_runtime("runtime-plugin-clear-model");
        let plugin = test_plugin_context("CodeReview");
        let normal_handle = create_test_handle(&app, "coding");
        let plugin_handle = create_test_handle(&app, "plugin:CodeReview");
        app.plugin_handles
            .insert(plugin.name.clone(), plugin_handle.clone());
        app.active_handle = Some(plugin_handle.clone());
        app.apply_runtime_plugin_context(Some(&plugin), &plugin_handle)
            .unwrap();

        app.apply_runtime_plugin_context(None, &normal_handle)
            .unwrap();

        let plugin_snapshot = app.runtime.agent_handle_snapshot(&plugin_handle).unwrap();
        let normal_snapshot = app.runtime.agent_handle_snapshot(&normal_handle).unwrap();
        assert!(plugin_snapshot.plugin_overlay_name.is_none());
        assert!(normal_snapshot.plugin_overlay_name.is_none());
        assert!(app.active_plugin_name().is_none());
    }

    #[test]
    fn permissions_switch_permission_switch_clears_plugin_runtime_context() {
        let mut app = app_with_test_runtime("permissions-plugin-clear-model");
        let plugin = test_plugin_context("CodeReview");
        let plugin_handle = create_test_handle(&app, "plugin:CodeReview");
        app.plugin_handles
            .insert(plugin.name.clone(), plugin_handle.clone());
        app.active_handle = Some(plugin_handle.clone());
        app.apply_runtime_plugin_context(Some(&plugin), &plugin_handle)
            .unwrap();

        app.switch_permissions("strict");

        assert_eq!(app.security.mode_label, "strict");
        assert!(app.normal_handle.is_none());
        assert!(app.plugin_handles.is_empty());
        assert!(app.active_handle.is_none());
        assert!(app.active_plugin_name().is_none());
        let snapshot = app.runtime.agent_handle_snapshot(&plugin_handle).unwrap();
        assert!(snapshot.plugin_overlay_name.is_none());
    }

    #[test]
    fn slash_plugins_lists_registry() {
        let mut app = App::new();

        assert!(app.handle_slash_command("/plugins"));

        let msg = app.messages.last().unwrap().content.as_str();
        assert!(msg.contains("plugin(s) loaded"));
        assert!(msg.contains("/plugin <name> <prompt>"));
    }

    #[test]
    fn slash_plugin_prepares_plugin_submission() {
        let mut app = App::new();

        assert!(app.handle_slash_command("/plugin CodeReview inspect this diff"));

        let prepared = app
            .prepared_submission
            .as_ref()
            .expect("plugin command should prepare a submission");
        assert!(prepared.display.starts_with("@CodeReview "));
        assert!(prepared.prompt.contains("Review the following code"));
        assert_eq!(prepared.plugin.as_ref().unwrap().name, "CodeReview");
    }

    #[test]
    fn plugin_submission_does_not_fall_back_to_normal_handle() {
        let mut app = App::new();
        let normal_handle = AgentHandleId::new();
        let plugin_handle = AgentHandleId::new();
        let plugin = test_plugin_context("CodeReview");
        app.normal_handle = Some(normal_handle.clone());

        assert_eq!(
            app.existing_handle_for_submission(Some(&plugin)),
            None,
            "first plugin turn must not reuse the normal coding handle"
        );

        app.plugin_handles
            .insert(plugin.name.clone(), plugin_handle.clone());
        assert_eq!(
            app.existing_handle_for_submission(Some(&plugin)),
            Some(plugin_handle)
        );
        assert_eq!(
            app.existing_handle_for_submission(None),
            Some(normal_handle)
        );
    }

    #[test]
    fn slash_find_tracks_matches_and_target() {
        let mut app = App::new();
        app.messages
            .push(ChatMessage::new(Role::User, "alpha".into()));
        app.messages
            .push(ChatMessage::new(Role::Assistant, "beta alpha".into()));

        assert!(app.handle_slash_command("/find alpha"));

        assert_eq!(app.activity.search.query, "alpha");
        assert_eq!(app.activity.search.matches, vec![0, 1]);
        assert_eq!(app.activity.search.position(), Some((1, 2)));
        assert_eq!(app.activity.search.target_message, Some(0));
    }

    #[test]
    fn search_navigation_wraps() {
        let mut app = App::new();
        app.messages
            .push(ChatMessage::new(Role::User, "alpha".into()));
        app.messages
            .push(ChatMessage::new(Role::Assistant, "beta alpha".into()));
        app.start_search("alpha".into());

        app.jump_search_next();
        assert_eq!(app.activity.search.position(), Some((2, 2)));
        assert_eq!(app.activity.search.target_message, Some(1));

        app.jump_search_next();
        assert_eq!(app.activity.search.position(), Some((1, 2)));
        assert_eq!(app.activity.search.target_message, Some(0));
    }

    #[test]
    fn tool_output_adds_visible_tool_message() {
        let mut app = App::new();
        app.turn.active_turn_id = Some(1);

        app.apply_tool_output(
            1,
            "call-1".into(),
            "list_directory".into(),
            r#"{"path":"."}"#.into(),
            Some("FILE Cargo.toml".into()),
            None,
        );

        let msg = app.messages.last().unwrap();
        assert_eq!(msg.role, Role::Tool);
        assert!(msg.content.contains("list_directory"));
        assert!(msg.content.contains("FILE Cargo.toml"));
        assert_eq!(msg.tool.as_ref().unwrap().status, ToolStatus::Done);
    }

    #[test]
    fn tool_output_updates_existing_call() {
        let mut app = App::new();
        app.turn.active_turn_id = Some(1);

        app.apply_tool_output(
            1,
            "call-1".into(),
            "read_file".into(),
            r#"{"path":"Cargo.toml"}"#.into(),
            None,
            None,
        );
        app.apply_tool_output(
            1,
            "call-1".into(),
            "read_file".into(),
            r#"{"path":"Cargo.toml"}"#.into(),
            Some("package data".into()),
            None,
        );

        assert_eq!(app.messages.len(), 1);
        let msg = app.messages.last().unwrap();
        assert!(msg.content.contains("package data"));
        assert_eq!(msg.tool.as_ref().unwrap().status, ToolStatus::Done);
    }

    #[test]
    fn consecutive_same_tool_calls_share_one_card() {
        let mut app = App::new();
        app.turn.active_turn_id = Some(1);

        app.apply_tool_output(
            1,
            "call-1".into(),
            "read_file".into(),
            r#"{"path":"Cargo.toml"}"#.into(),
            None,
            None,
        );
        app.apply_tool_output(
            1,
            "call-2".into(),
            "read_file".into(),
            r#"{"path":"Cargo.lock"}"#.into(),
            None,
            None,
        );
        app.apply_tool_output(
            1,
            "call-2".into(),
            "read_file".into(),
            r#"{"path":"Cargo.lock"}"#.into(),
            Some("lock data".into()),
            None,
        );

        assert_eq!(app.messages.len(), 1);
        let tool = app.messages[0].tool.as_ref().unwrap();
        assert_eq!(tool.call_id, "call-1");
        assert_eq!(tool.grouped_calls.len(), 1);
        assert_eq!(tool.grouped_calls[0].call_id, "call-2");
        assert_eq!(tool.grouped_calls[0].result.as_deref(), Some("lock data"));
    }

    #[test]
    fn structured_patch_arguments_use_first_header_path_for_snapshot() {
        let patch = "*** Begin Patch\n\
*** Update File: src/lib.rs\n\
*** Old Hash: blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\
@@\n\
 old\n\
-value\n\
+new value\n\
*** Delete File: src/old.rs\n\
*** Old Hash: blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n\
*** End Patch";
        let arguments = serde_json::json!({ "patch": patch }).to_string();

        assert_eq!(
            crate::frontend::tui::activity::editable_file_path("patch", &arguments),
            Some("src/lib.rs".into())
        );
    }

    #[test]
    fn structured_patch_arguments_without_header_do_not_invent_path() {
        let arguments =
            serde_json::json!({ "patch": "*** Begin Patch\n*** End Patch" }).to_string();

        assert_eq!(
            crate::frontend::tui::activity::editable_file_path("patch", &arguments),
            None
        );
    }

    #[test]
    fn write_file_output_attaches_file_diff() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, "old\nsame\n").unwrap();

        let mut app = App::new();
        app.workdir = dir.path().to_path_buf();
        app.security = crate::security::default_runtime_security(&app.workdir).unwrap();
        app.turn.active_turn_id = Some(1);

        app.apply_tool_output(
            1,
            "call-1".into(),
            "write_file".into(),
            r#"{"path":"notes.txt","content":"new\nsame\n"}"#.into(),
            None,
            Some(FileSnapshot {
                path: "notes.txt".into(),
                content: Some("old\nsame\n".into()),
            }),
        );
        std::fs::write(&file, "new\nsame\n").unwrap();
        app.apply_tool_output(
            1,
            "call-1".into(),
            "write_file".into(),
            r#"{"path":"notes.txt","content":"new\nsame\n"}"#.into(),
            Some("Wrote 9 bytes to notes.txt".into()),
            None,
        );

        let diff = &app
            .messages
            .last()
            .unwrap()
            .tool
            .as_ref()
            .unwrap()
            .file_diff
            .as_ref()
            .unwrap()
            .text;
        assert!(diff.contains("--- a/notes.txt"));
        assert!(diff.contains("+++ b/notes.txt"));
        assert!(diff.contains("-old"));
        assert!(diff.contains("+new"));
    }

    #[test]
    fn toggle_recent_reasoning_flips_latest_assistant_block() {
        let mut app = App::new();
        let mut msg = ChatMessage::new(Role::Assistant, "answer".into());
        msg.reasoning = Some("step one".into());
        app.messages.push(msg);

        app.toggle_recent_reasoning();

        assert!(!app.messages.last().unwrap().reasoning_collapsed);
    }

    #[test]
    fn stream_after_tool_output_creates_visible_assistant_message() {
        let mut app = App::new();
        app.messages
            .push(ChatMessage::new(Role::Assistant, String::new()));
        app.turn.active_turn_id = Some(1);
        app.turn.active_assistant_index = Some(0);
        app.apply_tool_output(
            1,
            "call-1".into(),
            "read_file".into(),
            r#"{"path":"Cargo.toml"}"#.into(),
            None,
            None,
        );

        app.apply_stream_token(1, "final answer".into(), None, false, None);

        let msg = app.messages.last().unwrap();
        assert_eq!(msg.role, Role::Assistant);
        assert_eq!(msg.content, "final answer");
    }

    #[test]
    fn tool_output_splits_assistant_stream_at_call_site() {
        let mut app = App::new();
        app.status = AppStatus::Streaming;
        app.turn.active_turn_id = Some(1);
        app.turn.active_assistant_index = Some(0);
        app.messages
            .push(ChatMessage::new(Role::Assistant, "before tool".into()));

        app.apply_tool_output(
            1,
            "call-1".into(),
            "grep".into(),
            r#"{"pattern":"Runtime","path":"src"}"#.into(),
            None,
            None,
        );
        app.apply_stream_token(1, "after tool".into(), None, false, None);

        assert_eq!(app.messages.len(), 3);
        assert_eq!(app.messages[0].role, Role::Assistant);
        assert_eq!(app.messages[0].content, "before tool");
        assert_eq!(app.messages[1].role, Role::Tool);
        assert_eq!(app.messages[2].role, Role::Assistant);
        assert_eq!(app.messages[2].content, "after tool");
    }

    #[test]
    fn selection_copy_uses_viewport_origin() {
        let mut app = App::new();
        app.set_clipboard_sink(Arc::new(|_| {}));
        app.activity
            .visible_rows
            .replace(vec!["line two".into(), "line three".into()]);
        app.activity.visible_rows_origin.set(10);
        let sel = TextSelection {
            start_row: 10,
            start_col: 0,
            end_row: 11,
            end_col: 0,
            active: false,
        };

        super::super::interaction::copy_selection(&app, &sel);
    }

    #[test]
    fn loading_session_does_not_override_explicit_model() {
        let mut app = App::new();
        app.current_model = "explicit-model".into();
        app.previous_model = "old-agent-model".into();

        app.load_session(test_session());

        assert_eq!(app.current_model, "explicit-model");
        assert_eq!(app.current_provider, "old-provider");
        assert_eq!(app.messages.len(), 2);
        assert!(app.previous_model.is_empty());
        assert!(app.normal_handle.is_none());
    }

    #[test]
    fn tui_completion_uses_runtime_completion_metadata() {
        let mut app = App::new();
        let handle_id = AgentHandleId::new();
        let runtime_turn_id = TurnId::new();
        app.active_handle = Some(handle_id.clone());
        app.active_runtime_turn = Some(runtime_turn_id.clone());
        app.status = AppStatus::Streaming;
        app.turn.active_turn_id = Some(3);
        app.turn.active_assistant_index = Some(0);
        app.messages
            .push(ChatMessage::new(Role::Assistant, "answer".into()));
        let completion = test_completion(
            handle_id.clone(),
            runtime_turn_id.clone(),
            TurnStatus::Completed,
        );

        app.apply_runtime_completion(
            3,
            &handle_id,
            &runtime_turn_id,
            TurnStatus::Completed,
            Some(&completion),
            None,
        );

        assert_eq!(app.current_model, "runtime-model");
        assert_eq!(app.current_provider, "runtime-provider");
        assert_eq!(app.token_count, 5);
        assert_eq!(app.status, AppStatus::Ready);
        assert!(app.active_handle.is_none());
        assert!(app.active_runtime_turn.is_none());
    }

    #[test]
    fn runtime_persistence_no_frontend_save_on_runtime_completion() {
        let mut app = App::new();
        app.save_sessions = true;
        app.session = Some(test_session());
        let original_session_id = app.session.as_ref().unwrap().id.clone();
        let handle_id = AgentHandleId::new();
        let runtime_turn_id = TurnId::new();
        app.active_handle = Some(handle_id.clone());
        app.active_runtime_turn = Some(runtime_turn_id.clone());
        app.status = AppStatus::Streaming;
        app.turn.active_turn_id = Some(4);
        app.turn.active_assistant_index = Some(0);
        app.turn.pending_user = Some("new question".into());
        app.messages
            .push(ChatMessage::new(Role::Assistant, "runtime answer".into()));
        let completion = test_completion(
            handle_id.clone(),
            runtime_turn_id.clone(),
            TurnStatus::Completed,
        );

        app.apply_runtime_completion(
            4,
            &handle_id,
            &runtime_turn_id,
            TurnStatus::Completed,
            Some(&completion),
            None,
        );

        assert_eq!(app.session.as_ref().unwrap().id, original_session_id);
        assert_eq!(app.session.as_ref().unwrap().messages.len(), 2);
    }

    #[test]
    fn tui_completion_cancelled_keeps_partial_output_and_copy() {
        let mut app = App::new();
        let handle_id = AgentHandleId::new();
        let runtime_turn_id = TurnId::new();
        app.active_handle = Some(handle_id.clone());
        app.active_runtime_turn = Some(runtime_turn_id.clone());
        app.status = AppStatus::Streaming;
        app.turn.active_turn_id = Some(5);
        app.turn.active_assistant_index = Some(0);
        app.messages
            .push(ChatMessage::new(Role::Assistant, "partial answer".into()));
        let completion = test_completion(
            handle_id.clone(),
            runtime_turn_id.clone(),
            TurnStatus::Cancelled,
        );

        app.apply_runtime_completion(
            5,
            &handle_id,
            &runtime_turn_id,
            TurnStatus::Cancelled,
            Some(&completion),
            None,
        );

        assert_eq!(app.status, AppStatus::Ready);
        assert_eq!(app.messages[0].content, "partial answer");
        assert_eq!(
            app.messages.last().unwrap().content,
            "Cancelled. Partial output kept."
        );
        assert!(app.active_handle.is_none());
        assert!(app.active_runtime_turn.is_none());
    }

    #[test]
    fn load_session_clears_runtime_handles_and_transient_state() {
        let mut app = App::new();
        app.normal_handle = Some(AgentHandleId::new());
        app.plugin_handles
            .insert("CodeReview".into(), AgentHandleId::new());
        app.active_handle = Some(AgentHandleId::new());
        app.active_runtime_turn = Some(TurnId::new());
        app.prepared_submission = Some(PreparedSubmission {
            display: "display".into(),
            prompt: "prompt".into(),
            raw_prompt: None,
            plugin: None,
        });
        app.turn.queued_inputs.push_back("queued".into());
        app.start_search("old".into());

        app.load_session(test_session());

        assert!(app.normal_handle.is_none());
        assert!(app.plugin_handles.is_empty());
        assert!(app.active_handle.is_none());
        assert!(app.active_runtime_turn.is_none());
        assert!(app.prepared_submission.is_none());
        assert!(app.turn.queued_inputs.is_empty());
        assert!(!app.activity.search.is_active());
    }
}
