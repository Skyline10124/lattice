// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! PTY smoke scenario (RFC #41 (Codeberg #19) layer 3 / PTY-smoke): prove the
//! prototype TUI entrypoint still wires into the same `stream_turn` chain as
//! `session send --json`.
//!
//! Since #62 (Codeberg #40) the turn chain is Direct-Local: the TUI drives a
//! real turn against the canonical project root — no KVM, no MSB daemon, no
//! OCI image, no `[workspace]` profile section. This scenario therefore runs
//! in the ordinary lane (not `#[ignore]`-gated): a broken Direct-Local
//! environment is a real product failure, never something to skip. RFC #41
//! decision three: the smoke does **not** assert on prompt shape, colors, or
//! layout. Ready recognition and ANSI cleanup live inside `PtyTuiDriver`; the
//! scenario asserts only semantic markers (a turn produced visible assistant
//! text and the TUI exited cleanly).
//!
//! What it proves (issue #43 (Codeberg #21) acceptance point: at least one
//! PTY smoke):
//!
//! - `lattice tui --agent <name> --project-branch <branch>` starts, enters
//!   the alternate screen, and accepts a prompt.
//! - Sending a prompt produces assistant token text through the real turn
//!   chain (the fake LLM receives the request).
//! - `/quit` exits the TUI cleanly (exit within the timeout, no hang).
//! - After exit, the TUI's process group has no live members (no orphaned
//!   TUI / fake-LLM children).

use std::time::Duration;

use lattice_system_harness::processes_in_pgid;
use lattice_system_harness::{
    LatticeDriver, PtyTuiDriver, ScenarioLayer, ScenarioSpec, ScenarioWorld, SseScript,
};

/// Minimal project-local agent profile for the TUI smoke: the Direct-Local
/// end-state shape — no `[workspace]` section, no enabled tools (this is a
/// plain-text turn, so no `tool_calls` should appear). Hand-written TOML,
/// not serde — the harness boundary forbids `lattice-profile`.
const AGENT_TOML: &str = r#"schema_version = 1

[agent]
name = "tui-smoke"
model = "compat:tui-smoke"

[prompt]
system = "You are a concise test agent. Reply briefly."

[tools]
enabled = []
"#;

fn driver() -> LatticeDriver {
    LatticeDriver::from_env().expect("LATTICE_BIN must point to a built lattice binary")
}

#[test]
fn pty_tui_smoke_runs_turn_and_quits_cleanly() {
    let driver = driver();

    let spec = ScenarioSpec {
        name: "pty-tui-smoke".into(),
        layer: ScenarioLayer::PtySmoke,
        step_timeout_secs: 120,
        total_timeout_secs: Some(300),
    };
    let budget = spec.start_budget();
    let world = ScenarioWorld::new(&spec.name).expect("world");
    world
        .write_project_file(".lattice/agents/tui-smoke/agent.toml", AGENT_TOML)
        .expect("write agent.toml");
    world.init_git_project().expect("init git project");
    budget.check(&spec.name);

    let server = lattice_system_harness::FakeLlmServer::start(SseScript::plain("pong"))
        .expect("start fake llm");
    world.set_env("OPENAI_COMPATIBLE_BASE_URL", server.base_url());
    world.set_env("OPENAI_COMPATIBLE_MODEL", "tui-smoke");
    world.set_env(
        "OPENAI_COMPATIBLE_API_KEY",
        lattice_system_harness::FAKE_API_KEY,
    );

    budget.check(&spec.name);
    let mut tui = PtyTuiDriver::spawn(&driver, &world, "tui-smoke", "main").expect("spawn tui");

    // Wait for the TUI to enter the alternate screen. Ready recognition
    // lives inside the driver (`wait_for_ready`); the scenario never matches
    // display bytes itself (RFC #41 decision three).
    tui.wait_for_ready(Duration::from_secs(30))
        .expect("tui entered alternate screen");

    // Send a prompt and wait for the assistant token text. The fake LLM
    // returns "pong", so the TUI's token rendering must surface it.
    budget.check(&spec.name);
    tui.send_line("ping").expect("send prompt");
    tui.wait_for(Duration::from_secs(60), |s| s.contains("pong"))
        .expect("assistant token text appeared");

    let caps = server.captured_requests();
    assert!(
        caps.iter()
            .any(|c| c.model == "tui-smoke" && c.message_roles.iter().any(|r| r == "user")),
        "fake LLM did not receive the expected request; captures: {caps:?}"
    );

    // `/quit` must exit the TUI **by itself**. Observe the child's own exit
    // before `Drop` runs: Drop kills the process group, so a survivor check
    // alone would pass even if `/quit` did nothing at all — it would only
    // prove SIGKILL works.
    tui.send_line("/quit").expect("send /quit");
    tui.wait_for_exit(Duration::from_secs(15))
        .expect("/quit must exit the TUI without being killed");

    let pgid = tui.pgid();
    drop(tui);

    // Then verify the whole group is gone (no orphaned lattice/fake-LLM children).
    std::thread::sleep(Duration::from_millis(400));
    if let Some(pgid) = pgid {
        let survivors = processes_in_pgid(pgid);
        assert!(
            survivors.is_empty(),
            "TUI process group {pgid} still has live members after quit+drop: {survivors:?}"
        );
    }
}
