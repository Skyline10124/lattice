use std::path::PathBuf;

const RUNTIME_RUST_FILES: &[&str] = &[
    "src/lib.rs",
    "src/assembly.rs",
    "src/session.rs",
    "src/turn.rs",
];

const REMOVED_EXTENSION_NEEDLES: &[&str] = &[
    "lattice_orchestrator",
    "lattice_plugin",
    "Pipeline",
    "MicroAgent",
    "EventBus",
    "PluginRegistry",
    "RuntimePluginOverlay",
    "RuntimeAgentTurnExecutor",
    "load_plugin_dir",
    "watch_plugin_dir",
    "AgentHandleId",
    "get_or_create_agent_handle",
    "latest_turn_completion",
];

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_runtime_source(path: &str) -> String {
    let full_path = manifest_dir().join(path);
    std::fs::read_to_string(&full_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", full_path.display()))
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

#[test]
fn runtime_active_source_excludes_removed_extension_concepts() {
    let mut failures = Vec::new();

    for path in RUNTIME_RUST_FILES {
        let active = active_source(&read_runtime_source(path));
        for needle in REMOVED_EXTENSION_NEEDLES {
            if active.contains(needle) {
                failures.push(format!(
                    "{path} contains forbidden active string {needle:?}"
                ));
            }
        }
    }

    let manifest = read_runtime_source("Cargo.toml");
    for needle in ["lattice-orchestrator", "lattice-plugin"] {
        if manifest.contains(needle) {
            failures.push(format!(
                "Cargo.toml contains forbidden dependency {needle:?}"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "runtime boundary failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn runtime_keeps_generic_session_turn_contract() {
    let source = RUNTIME_RUST_FILES
        .iter()
        .map(|path| active_source(&read_runtime_source(path)))
        .collect::<Vec<_>>()
        .join("\n");

    for expected in [
        "RuntimeInvocationContext",
        "RuntimeAgentSession",
        "submit_session_turn",
        "commit_turn(",
    ] {
        assert!(
            source.contains(expected),
            "runtime source should continue to expose {expected}"
        );
    }
}
