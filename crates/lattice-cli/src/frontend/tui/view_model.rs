use super::app::summarize_prompt;
use super::app::App;
use super::state::{AppStatus, SearchState};

/// Small render-facing snapshot used by widgets instead of peeking at all of `App`.
pub(super) struct TuiViewModel<'a> {
    pub(super) composer: ComposerView<'a>,
    pub(super) statusline: StatuslineView<'a>,
    pub(super) search: &'a SearchState,
    pub(super) thinking: Option<&'a str>,
    pub(super) help_open: bool,
    pub(super) queued_count: usize,
    pub(super) queue_head_summary: Option<String>,
    pub(super) queue_prompt_active: bool,
}

pub(super) struct ComposerView<'a> {
    pub(super) input: &'a str,
    pub(super) input_cursor: usize,
    pub(super) suggestion_index: usize,
    pub(super) suggestion_window_start: usize,
}

pub(super) struct StatuslineView<'a> {
    pub(super) status: &'a AppStatus,
    pub(super) current_model: &'a str,
    pub(super) current_provider: &'a str,
    pub(super) token_count: usize,
    pub(super) context_limit: u32,
    pub(super) transcript_mode: bool,
    pub(super) thinking_effort: Option<&'a str>,
    pub(super) sandbox: &'a str,
    pub(super) plugin_label: String,
    pub(super) queued_count: usize,
    pub(super) mode_label: &'static str,
    pub(super) spinner: &'static str,
    pub(super) stream_started: Option<std::time::Instant>,
    pub(super) reasoning_started: Option<std::time::Instant>,
    pub(super) reasoning_duration: Option<std::time::Duration>,
}

impl<'a> TuiViewModel<'a> {
    pub(super) fn from_app(app: &'a App) -> Self {
        let queued_count = app.turn.queued_inputs.len();
        let queue_prompt_active = app.queue_prompt_active();
        Self {
            composer: ComposerView {
                input: &app.composer.input,
                input_cursor: app.composer.input_cursor,
                suggestion_index: app.composer.suggestion_index,
                suggestion_window_start: app.composer.suggestion_window_start,
            },
            statusline: StatuslineView {
                status: &app.status,
                current_model: &app.current_model,
                current_provider: &app.current_provider,
                token_count: app.token_count,
                context_limit: app.context_limit,
                transcript_mode: app.transcript_mode,
                thinking_effort: app.thinking_effort.as_deref(),
                sandbox: app.security.mode_label.as_str(),
                plugin_label: app
                    .active_plugin_name()
                    .map(|name| format!("plugin:{name}"))
                    .unwrap_or_else(|| format!("plugins:{}", app.plugin_count())),
                queued_count: app.turn.queued_inputs.len(),
                mode_label: app.mode_label(),
                spinner: app.spinner(),
                stream_started: app.turn.stream_started,
                reasoning_started: app.turn.reasoning_started,
                reasoning_duration: app.turn.reasoning_duration,
            },
            search: &app.activity.search,
            thinking: active_thinking(app),
            help_open: app.help_open,
            queued_count,
            queue_head_summary: app
                .turn
                .queued_inputs
                .front()
                .map(|prompt| summarize_prompt(prompt, 80)),
            queue_prompt_active,
        }
    }
}

fn active_thinking(app: &App) -> Option<&str> {
    app.messages.iter().rev().find_map(|msg| {
        if msg.role != lattice_core::types::Role::Assistant
            || (!app.transcript_mode && msg.reasoning_collapsed)
        {
            return None;
        }
        msg.reasoning
            .as_deref()
            .map(str::trim)
            .filter(|reasoning| !reasoning.is_empty())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::tui::state::ChatMessage;
    use lattice_core::types::Role;
    use lattice_runtime::RuntimePluginOverlay;

    #[test]
    fn view_model_tracks_composer_and_queue_state() {
        let mut app = App::new();
        app.composer.set_input("hello");
        app.turn.queue_input("queued".into());

        let vm = TuiViewModel::from_app(&app);

        assert_eq!(vm.composer.input, "hello");
        assert_eq!(vm.statusline.queued_count, 1);
        assert_eq!(vm.queued_count, 1);
        assert!(!vm.queue_prompt_active);
        assert_eq!(vm.queue_head_summary, Some("queued".into()));
    }

    #[test]
    fn view_model_exposes_uncollapsed_thinking() {
        let mut app = App::new();
        let mut msg = ChatMessage::new(Role::Assistant, "answer".into());
        msg.reasoning = Some("step one".into());
        msg.reasoning_collapsed = false;
        app.messages.push(msg);

        let vm = TuiViewModel::from_app(&app);

        assert_eq!(vm.thinking, Some("step one"));
    }

    #[test]
    fn view_model_exposes_collapsed_thinking_in_trace_mode() {
        let mut app = App::new();
        app.transcript_mode = true;
        let mut msg = ChatMessage::new(Role::Assistant, "answer".into());
        msg.reasoning = Some("step one".into());
        msg.reasoning_collapsed = true;
        app.messages.push(msg);

        let vm = TuiViewModel::from_app(&app);

        assert_eq!(vm.thinking, Some("step one"));
    }

    #[test]
    fn plugin_statusline_normal_handle_shows_plugins_count() {
        let app = App::new();

        let vm = TuiViewModel::from_app(&app);

        assert_eq!(
            vm.statusline.plugin_label,
            format!("plugins:{}", app.plugin_count())
        );
    }

    #[test]
    fn plugin_statusline_plugin_prompt_running_shows_plugin_statusline() {
        let mut runtime = lattice_runtime::Runtime::builder().build();
        runtime
            .register_model(lattice_core::catalog::ModelCatalogEntry {
                canonical_id: "plugin-status-model".into(),
                context_length: 8192,
                providers: vec![lattice_core::catalog::CatalogProviderEntry {
                    provider_id: "ollama".into(),
                    api_model_id: "plugin-status-model".into(),
                    priority: 1,
                    credential_keys: std::collections::HashMap::new(),
                    base_url: Some("http://127.0.0.1:9".into()),
                    api_protocol: lattice_core::catalog::ApiProtocol::OpenAiChat,
                    provider_specific: std::collections::HashMap::new(),
                }],
                aliases: Vec::new(),
            })
            .unwrap();
        let mut app = App::new();
        app.runtime = runtime;
        app.current_model = "plugin-status-model".into();
        let workdir = app.workdir.to_string_lossy().to_string();
        let handle_id = app
            .runtime
            .get_or_create_agent_handle_for_session(
                lattice_orchestrator::AgentTurnSpec {
                    profile_name: "plugin:CodeReview".into(),
                    model: app.current_model.clone(),
                    provider_override: None,
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
                    preset: Some(lattice_orchestrator::AgentPreset::Coding),
                },
                app.security.clone(),
                None,
            )
            .unwrap();
        app.runtime
            .set_agent_plugin_overlay(
                &handle_id,
                Some(RuntimePluginOverlay {
                    name: "CodeReview".into(),
                    system_prompt_delta: "Review code carefully.".into(),
                    output_contract_delta: None,
                    tools: Vec::new(),
                    authorized_tool_names: Vec::new(),
                }),
            )
            .unwrap();
        app.active_handle = Some(handle_id);

        let vm = TuiViewModel::from_app(&app);

        assert_eq!(vm.statusline.plugin_label, "plugin:CodeReview");
    }
}
