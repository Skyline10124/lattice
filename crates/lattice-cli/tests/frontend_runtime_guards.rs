use std::path::PathBuf;

const FRONTEND_FILES: &[&str] = &[
    "src/coding_agent.rs",
    "src/commands/run.rs",
    "src/commands/debug.rs",
    "src/frontend/tui/app.rs",
    "src/frontend/tui/agent_runner.rs",
    "src/frontend/tui/plugin_mode.rs",
];

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn frontend_sources() -> Vec<(&'static str, String)> {
    FRONTEND_FILES
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
        .map(|(_, source)| source)
        .collect::<Vec<_>>()
        .join("\n");

    for expected in [
        "RuntimeTurnEvent",
        "TurnCompletion",
        "submit_turn(",
        "latest_turn_completion",
    ] {
        assert!(
            sources.contains(expected),
            "frontend runtime paths should continue to use {expected}"
        );
    }
}
