use std::path::PathBuf;

const CLI_UX_FILES: &[&str] = &[
    "src/main.rs",
    "src/commands/run.rs",
    "src/commands/debug.rs",
    "src/profile_runtime.rs",
    "src/runtime_turn.rs",
    "src/session_seed.rs",
    "src/model_inventory.rs",
    "src/frontend/tui/app.rs",
    "src/frontend/tui/agent_runner.rs",
    "src/frontend/tui/commands.rs",
    "src/frontend/tui/state.rs",
    "src/frontend/tui/view_model.rs",
    "src/frontend/tui/widgets/statusline.rs",
    "src/frontend/tui/widgets/welcome.rs",
    "src/frontend/tui/widgets/composer.rs",
    "src/test_support.rs",
];

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn frontend_sources() -> Vec<(&'static str, String)> {
    CLI_UX_FILES
        .iter()
        .map(|path| {
            let full_path = manifest_dir().join(path);
            let source = std::fs::read_to_string(&full_path)
                .unwrap_or_else(|err| panic!("failed to read {}: {err}", full_path.display()));
            (*path, source)
        })
        .collect()
}

fn active_source(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut in_block_comment = false;

    while let Some(ch) = chars.next() {
        if in_block_comment {
            if ch == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_block_comment = false;
            } else if ch == '\n' {
                out.push('\n');
            }
            continue;
        }

        if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            in_block_comment = true;
            continue;
        }

        if ch == '/' && chars.peek() == Some(&'/') {
            for next in chars.by_ref() {
                if next == '\n' {
                    out.push('\n');
                    break;
                }
            }
            continue;
        }

        out.push(ch);
    }

    out
}

fn assert_absent_in_active_source(needles: &[&str]) {
    let mut failures = Vec::new();
    for (path, source) in frontend_sources() {
        let active = active_source(&source);
        for needle in needles {
            if active.contains(needle) {
                failures.push(format!(
                    "{path} contains forbidden active-path string {needle:?}"
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "frontend runtime guard failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn frontend_no_live_agent_ownership() {
    assert_absent_in_active_source(&["use lattice_agent::Agent", "Arc<Mutex<Agent>>"]);
}

#[test]
fn frontend_no_active_loop_event_rendering() {
    assert_absent_in_active_source(&[
        ".run_streaming(",
        ".run(",
        "LoopEvent::Manifest",
        "render(&LoopEvent",
    ]);
}

#[test]
fn frontend_uses_runtime_turn_completion() {
    let sources = frontend_sources()
        .into_iter()
        .map(|(_, source)| active_source(&source))
        .collect::<Vec<_>>()
        .join("\n");

    for expected in [
        "RuntimeTurnEvent",
        "RuntimeAgentSession",
        "TurnCompletion",
        "submit_session_turn",
    ] {
        assert!(
            sources.contains(expected),
            "frontend runtime paths should continue to use {expected}"
        );
    }
}

#[test]
fn frontend_no_runtime_handle_owned_execution() {
    assert_absent_in_active_source(&[
        "AgentHandleId",
        "get_or_create_agent_handle",
        "latest_turn_completion",
        "RuntimePluginOverlay",
    ]);
}

#[test]
fn single_agent_ux_has_no_removed_cli_or_tui_surfaces() {
    let deny = [
        "Commands::Code",
        "coding_agent",
        "code_agent",
        "AgentPreset::Coding",
        "pipeline: Option",
        "orchestrator_status",
        "plugins_dir",
        concat!("\"", "/plugins", "\""),
        concat!("\"", "/plugin"),
        "PluginRegistry",
        "plugin_registry",
        "plugin_sessions",
        "plugin_label",
        "RuntimePluginOverlay",
        "PreparedSubmission",
        "RunPluginContext",
        "MenuKind::Plugin",
        "profile_sessions",
        "profile_rebind",
        "fork_profile",
        concat!("\"", "/agent", "\""),
    ];
    assert_absent_in_active_source(&deny);
}

#[test]
fn single_agent_ux_uses_profile_runtime_path() {
    let sources = frontend_sources()
        .into_iter()
        .map(|(_, source)| active_source(&source))
        .collect::<Vec<_>>()
        .join("\n");

    for expected in [
        "ProfileSource",
        "prepare_profile_runtime_session",
        "RuntimeAgentSession",
        "submit_session_turn",
        "agent_label",
        "profile_drift_warning",
    ] {
        assert!(
            sources.contains(expected),
            "single-agent UX should continue to use {expected}"
        );
    }
}
