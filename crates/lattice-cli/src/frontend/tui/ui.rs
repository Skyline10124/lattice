use ratatui::{
    layout::{Constraint, Direction, Layout},
    widgets::Clear,
    Frame,
};

use super::app::App;
use super::bottom::{bottom_height, place_cursor, render_bottom};
use super::theme::Theme;
use super::view_model::TuiViewModel;
use super::widgets::transcript::render_transcript;

pub(super) fn draw(f: &mut Frame, app: &App) {
    let theme = Theme::catppuccin_mocha();
    let size = f.area();
    f.render_widget(Clear, size);
    app.activity.clear_render_state();
    let vm = TuiViewModel::from_app(app);

    let suggestions = app.slash_suggestions();
    let bottom_height = bottom_height(&vm, &suggestions, app.menu.as_ref(), size.height);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(bottom_height)])
        .split(size);

    render_transcript(f, chunks[0], app, &theme);
    let input_area = render_bottom(f, chunks[1], &vm, &theme, &suggestions, app.menu.as_ref());
    place_cursor(f, input_area, &vm);

    if let Some(ref overlay) = app.pager {
        overlay.render(f, size, &theme);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use lattice_core::types::Role;
    use lattice_runtime::RuntimeAgentSession;

    use super::*;
    use crate::frontend::tui::render_test::render_app;
    use crate::frontend::tui::state::{
        ChatMessage, MenuKind, MenuState, ToolStatus, SELECTION_LIST_PAGE_SIZE,
    };

    fn test_runtime_session(
        app: &App,
        profile_name: &str,
        session_id: &str,
    ) -> RuntimeAgentSession {
        crate::profile_runtime::prepare_generated_profile_runtime_session(
            &app.runtime,
            crate::profile_runtime::GeneratedProfileRuntimeSessionRequest {
                profile_name: profile_name.into(),
                model: app.current_model.clone(),
                system_prompt: lattice_runtime::coding_system_prompt(&app.workdir, &app.workdir),
                tool_bundles: vec!["workspace".into(), "filesystem".into()],
                thinking_effort: None,
                project_root: app.workdir.clone(),
                security: app.security.clone(),
                provider_override: app.provider_override.clone(),
                session_id: Some(session_id.into()),
                task_state_seed: None,
            },
        )
        .unwrap()
    }

    #[test]
    fn renders_empty_default_screen() {
        let app = App::new();

        let screen = render_app(&app, 100, 24);

        assert_screen_contains(
            &screen,
            &[
                "model  deepseek-v4-flash @ unresolved",
                "Tip: Type / to see all available commands.",
                "Type a message or / for commands",
                " prompt  deepseek-v4-flash@unresolved  ready",
            ],
        );
    }

    #[test]
    fn renders_transcript_above_bottom_area() {
        let mut app = App::new();
        app.messages.push(ChatMessage::new(
            Role::User,
            "Please inspect the TUI layout.".into(),
        ));
        app.messages.push(ChatMessage::new(
            Role::Assistant,
            "Here is a short answer.".into(),
        ));
        app.messages.push(done_tool_message());

        let screen = render_app(&app, 100, 18);

        assert_screen_contains(
            &screen,
            &[
                "Please inspect the TUI layout.",
                "Here is a short answer.",
                "Read(src/lib.rs)",
                "Type a message or / for commands",
            ],
        );
    }

    #[test]
    fn renders_slash_suggestions_with_composer_and_statusline() {
        let mut app = App::new();
        app.composer.set_input("/st");

        let screen = render_app(&app, 100, 24);

        assert_screen_contains(
            &screen,
            &[
                " commands ",
                "▶ /status",
                "show current session state",
                "❯ /st",
                " prompt  deepseek-v4-flash@unresolved  ready",
            ],
        );
    }

    #[test]
    fn renders_command_menu_as_bottom_selection_list() {
        let mut app = App::new();
        app.menu = Some(MenuState::new(
            MenuKind::Permissions,
            vec![
                "project".into(),
                "strict".into(),
                "permissive".into(),
                "off".into(),
            ],
            0,
        ));

        let screen = render_app(&app, 100, 24);

        assert_screen_contains(
            &screen,
            &[
                " permissions ",
                "▶ project",
                "  strict",
                "Type a message or / for commands",
                " prompt  deepseek-v4-flash@unresolved  ready",
            ],
        );
        assert!(!screen.contains("Select Permissions"));
    }

    #[test]
    fn renders_expanded_thinking_above_composer() {
        let mut app = App::new();
        let mut msg = ChatMessage::new(Role::Assistant, "Here is the answer.".into());
        msg.reasoning = Some("step one\nstep two".into());
        msg.reasoning_collapsed = false;
        app.messages.push(msg);

        let screen = render_app(&app, 100, 18);

        assert_screen_contains(
            &screen,
            &[
                "Here is the answer.",
                " thinking ",
                "step one",
                "step two",
                "Type a message or / for commands",
            ],
        );
        assert!(!screen.contains("Ctrl+O or /trace"));
        assert!(!screen.contains("thinking expanded"));
        assert!(!screen.contains("thought for 2 lines"));
    }

    #[test]
    fn hides_reasoning_only_assistant_from_transcript() {
        let mut app = App::new();
        let mut msg = ChatMessage::new(Role::Assistant, String::new());
        msg.reasoning = Some("thinking between tools".into());
        msg.reasoning_collapsed = false;
        app.messages.push(done_tool_message());
        app.messages.push(msg);
        app.messages.push(done_tool_message());

        let screen = render_app(&app, 100, 18);

        assert_screen_contains(&screen, &[" thinking ", "thinking between tools"]);
        assert!(!screen.contains("Thinking..."));
    }

    #[test]
    fn renders_slash_suggestions_at_bottom_before_scrolling_first_page() {
        let mut app = App::new();
        app.composer.set_input("/");
        for _ in 0..SELECTION_LIST_PAGE_SIZE - 1 {
            app.composer.suggestion_down();
        }

        let screen = render_app(&app, 100, 24);

        assert_screen_contains(&screen, &[" commands ", "  /approve", "▶ /find"]);
    }

    #[test]
    fn keeps_slash_suggestion_window_when_moving_up_inside_page() {
        let mut app = App::new();
        app.composer.set_input("/");
        for _ in 0..SELECTION_LIST_PAGE_SIZE {
            app.composer.suggestion_down();
        }
        app.composer.suggestion_up();

        let screen = render_app(&app, 100, 24);

        assert_screen_contains(&screen, &["▶ /find", "  /help"]);
    }

    #[test]
    fn renders_search_and_queue_statusline_segments() {
        let mut app = App::new();
        app.turn.queued_inputs.push_back("follow-up".into());
        app.messages
            .push(ChatMessage::new(Role::Assistant, "alpha beta".into()));
        app.start_search("alpha".into());

        let screen = render_app(&app, 160, 20);

        assert_screen_contains(&screen, &["queued:1", "find:1/1 'alpha'"]);
    }

    mod renders_queued_prompt {
        use super::*;

        #[test]
        fn renders_ready_queued_prompt_above_composer() {
            let mut app = App::new();
            app.turn.queued_inputs.push_back(
                "Summarize the runtime cancellation behavior before touching code.".into(),
            );

            let screen = render_app(&app, 160, 24);

            assert_screen_contains(
                &screen,
                &[
                    "queued prompt",
                    "Send queued prompt",
                    "Summarize the runtime cancellation behavior before touching code.",
                    "queued:1",
                    "Type a message or / for commands",
                ],
            );
        }

        #[test]
        fn slash_suggestions_take_priority_over_queue_prompt() {
            let mut app = App::new();
            app.turn.queued_inputs.push_back("queued follow-up".into());
            app.composer.set_input("/st");

            let screen = render_app(&app, 160, 24);

            assert_screen_contains(&screen, &[" commands ", "▶ /status", "queued:1"]);
            assert!(!screen.contains("queued prompt"));
            assert!(!screen.contains("Send queued prompt"));
        }
    }

    mod plugin_statusline {
        use super::*;

        #[test]
        fn plugin_prompt_transcript_row_keeps_at_plugin_prefix() {
            let mut app = App::new();
            app.messages.push(ChatMessage::new(
                Role::User,
                "@CodeReview inspect this diff".into(),
            ));

            let screen = render_app(&app, 120, 18);

            assert_screen_contains(&screen, &["@CodeReview inspect this diff"]);
        }

        #[test]
        fn plugin_prompt_running_shows_plugin_statusline() {
            let mut runtime = lattice_runtime::Runtime::builder().build();
            runtime
                .register_model(lattice_core::catalog::ModelCatalogEntry {
                    canonical_id: "plugin-status-render-model".into(),
                    context_length: 8192,
                    providers: vec![lattice_core::catalog::CatalogProviderEntry {
                        provider_id: "ollama".into(),
                        api_model_id: "plugin-status-render-model".into(),
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
            app.current_model = "plugin-status-render-model".into();
            app.status = crate::frontend::tui::state::AppStatus::Streaming;
            app.turn.stream_started = Some(std::time::Instant::now());
            let session = test_runtime_session(&app, "plugin:CodeReview", "plugin-render-session");
            app.plugin_sessions
                .insert("CodeReview".into(), session.clone());
            app.active_session_id = session.session_id.clone();
            app.messages.push(ChatMessage::new(
                Role::User,
                "@CodeReview inspect this diff".into(),
            ));

            let screen = render_app(&app, 160, 24);

            assert_screen_contains(
                &screen,
                &["@CodeReview inspect this diff", "plugin:CodeReview"],
            );
        }
    }

    mod approval_usage {
        use super::*;

        #[tokio::test]
        async fn approval_usage_renders_id_or_last() {
            let mut app = App::new();
            app.composer.set_input("/approve");

            app.submit().await.unwrap();
            let screen = render_app(&app, 120, 18);

            assert_screen_contains(&screen, &["Usage: /approve <id|last>"]);
        }
    }

    mod cancel_preserves {
        use super::*;

        #[test]
        fn cancel_preserves_partial_output_and_queue_count_in_render() {
            let mut app = App::new();
            app.messages.push(ChatMessage::new(
                Role::Assistant,
                "partial assistant output".into(),
            ));
            app.push_system_message("Cancelled. Partial output kept.");
            app.turn.queued_inputs.push_back("queued follow-up".into());

            let screen = render_app(&app, 160, 20);

            assert_screen_contains(
                &screen,
                &[
                    "partial assistant output",
                    "Cancelled. Partial output kept.",
                    "queued:1",
                ],
            );
        }
    }

    fn done_tool_message() -> ChatMessage {
        let mut msg = ChatMessage::tool_call(
            "call-1".into(),
            "read_file".into(),
            serde_json::json!({ "path": "src/lib.rs" }).to_string(),
        );
        let tool = msg.tool.as_mut().expect("tool message should have display");
        tool.status = ToolStatus::Done;
        tool.finished_at = Some(tool.started_at + Duration::from_millis(10));
        tool.result = Some("read ok".into());
        msg
    }

    fn assert_screen_contains(screen: &str, expected: &[&str]) {
        for needle in expected {
            assert!(
                screen.contains(needle),
                "expected rendered screen to contain {needle:?}\n\n{screen}"
            );
        }
    }
}
