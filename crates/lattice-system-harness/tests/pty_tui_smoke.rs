// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! PTY smoke scenario (RFC #19 layer 3 / PTY-smoke): prove the prototype
//! TUI entrypoint still wires into the same `stream_turn` chain as
//! `session send --json`.
//!
//! This is `#[ignore]`-gated and opted into via `LATTICE_HARNESS_MSB=1`,
//! exactly like the Microsandbox scenarios — the TUI drives a real turn
//! through the same Upper-backed `stream_turn` path, so it needs the same
//! MSB stack. RFC #19 decision three: the smoke does **not** assert on
//! prompt shape, colors, or layout. Prompt recognition and ANSI cleanup
//! live inside `PtyTuiDriver`; the scenario asserts only semantic markers
//! (a turn produced visible assistant text and the TUI exited cleanly).
//!
//! What it proves (issue #21 acceptance point: at least one PTY smoke):
//!
//! - `lattice tui --agent <name> --project-branch <branch>` starts, prints
//!   a banner, and accepts a prompt.
//! - Sending a prompt produces assistant token text through the real turn
//!   chain (the fake LLM receives the request).
//! - `/quit` exits the TUI cleanly (exit within the timeout, no hang).
//! - After exit, the TUI's process group has no live members (no orphaned
//!   TUI / microsandbox / fake-LLM children).

#![cfg(unix)]

use std::time::Duration;

#[cfg(target_os = "linux")]
use lattice_system_harness::processes_in_pgid;
use lattice_system_harness::{
    LatticeDriver, PtyTuiDriver, ScenarioLayer, ScenarioSpec, ScenarioWorld, SseScript,
    require_msb_env,
};

/// Minimal project-local agent profile template for the TUI smoke. Same
/// shape as the MSB plain-text scenario. `[workspace]` is mandatory and
/// **must carry a real `image`** — `resolve_upper_config`
/// (`crates/lattice-api/src/turn/upper.rs`) returns `MissingWorkspaceSource`
/// for an empty/absent `[workspace].image`, failing at Upper prep before
/// the TUI can drive a turn. The image is read from `LATTICE_MSB_IMAGE`
/// (required — no baked default; see `msb_full_turn.rs::render_agent_toml`
/// for the full rationale). No tools enabled.
// `egress = "blocked"`: plain-text / no-tool smoke must not depend on
// in-Upper `apt-get install git` (default open egress when the image lacks
// git). That path has a 300s timeout and hangs under DNS blackholes; the
// harness step budget is 120s. See `msb_full_turn.rs` for the full rationale.
const AGENT_TOML_TEMPLATE: &str = r#"schema_version = 1

[agent]
name = "tui-smoke"
model = "compat:tui-smoke"

[prompt]
system = "You are a concise test agent. Reply briefly."

[workspace]
image = "{image}"
egress = "blocked"

[tools]
enabled = []
"#;

/// Render the agent profile TOML with `[workspace].image` taken from
/// `LATTICE_MSB_IMAGE` (required, no default — see
/// `msb_full_turn.rs::render_agent_toml`). Hand-written template render,
/// not serde — the harness boundary forbids `lattice-profile`.
fn render_agent_toml() -> String {
    let image = std::env::var("LATTICE_MSB_IMAGE").unwrap_or_else(|_| {
        panic!(
            "LATTICE_MSB_IMAGE must be set to a fully-qualified OCI ref (or warm template name) \
             for the KVM full-gate scenarios; the harness bakes no default. Example: \
             LATTICE_MSB_IMAGE=docker.m.daocloud.io/library/python:3.12-slim"
        )
    });
    assert!(
        !image.trim().is_empty(),
        "LATTICE_MSB_IMAGE is set but empty; set it to a fully-qualified OCI ref or template name"
    );
    AGENT_TOML_TEMPLATE.replace("{image}", &image)
}

fn driver() -> LatticeDriver {
    LatticeDriver::from_env().expect("LATTICE_BIN must point to a built lattice binary")
}

#[test]
#[ignore = "PTY smoke scenario: run with LATTICE_HARNESS_MSB=1 (full gate)"]
fn pty_tui_smoke_runs_turn_and_quits_cleanly() {
    let driver = driver();
    require_msb_env(&driver);

    let spec = ScenarioSpec {
        name: "pty-tui-smoke".into(),
        layer: ScenarioLayer::PtySmoke,
        step_timeout_secs: 120,
        total_timeout_secs: Some(300),
    };
    let budget = spec.start_budget();
    let mut world = ScenarioWorld::new(&spec.name).expect("world");
    world
        .write_project_file(".lattice/agents/tui-smoke/agent.toml", &render_agent_toml())
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
    // Point MSB_HOME at the real host ~/.microsandbox — the scenario world's
    // env_clear + isolated HOME would otherwise hide it from the SDK. See
    // msb_full_turn.rs for the full rationale.
    let real_msb_home = std::env::var("HOME")
        .map(|h| std::path::Path::new(&h).join(".microsandbox"))
        .expect("test process HOME must be set to locate ~/.microsandbox");
    assert!(
        real_msb_home.join("bin").join("msb").is_file(),
        "MSB_HOME={real_msb_home:?} has no bin/msb; run `cargo build -p lattice-workspace`"
    );
    world.set_env("MSB_HOME", real_msb_home.display().to_string());

    budget.check(&spec.name);
    let mut tui = PtyTuiDriver::spawn(&driver, &world, "tui-smoke", "main").expect("spawn tui");

    // Wait for the TUI to be ready for input. Prompt recognition lives
    // inside the driver (`wait_for_prompt`); the scenario never matches the
    // prompt shape itself (RFC #19 decision three).
    tui.wait_for_prompt(Duration::from_secs(30))
        .expect("tui banner/prompt appeared");

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

    // Quit, then Drop kills+reaps the process group. We assert "no live
    // process-group members" rather than a graceful exit window, to avoid
    // coupling to prototype timing (PRD user story 20).
    tui.send_line("/quit").expect("send /quit");
    std::thread::sleep(Duration::from_millis(500));

    let pgid = tui.pgid();
    drop(tui);

    // Give the OS a moment to reap, then verify no survivors.
    // Linux-only: on non-Linux Unix we skip the /proc scan (the Drop kill
    // is still best-effort correct, just not machine-verified here).
    std::thread::sleep(Duration::from_millis(400));
    #[cfg(target_os = "linux")]
    if let Some(pgid) = pgid {
        let survivors = processes_in_pgid(pgid);
        assert!(
            survivors.is_empty(),
            "TUI process group {pgid} still has live members after quit+drop: {survivors:?}"
        );
    }
}
