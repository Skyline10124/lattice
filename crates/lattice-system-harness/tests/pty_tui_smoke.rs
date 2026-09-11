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
    LatticeDriver, PtyTuiDriver, ScenarioLayer, ScenarioSpec, ScenarioWorld, TuiMockScenario,
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

    let server = lattice_system_harness::FakeLlmServer::start(TuiMockScenario::LongText.script())
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
    // uses the shared long-text mock, so the final deterministic line must surface.
    budget.check(&spec.name);
    tui.send_line("ping").expect("send prompt");
    tui.wait_for(Duration::from_secs(60), |s| s.contains("mock line 32"))
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
        let survivors = processes_in_pgid(pgid).expect("inspect TUI process group");
        assert!(
            survivors.is_empty(),
            "TUI process group {pgid} still has live members after quit+drop: {survivors:?}"
        );
    }
}

mod support;

fn finish_pty(
    evidence: lattice_system_harness::ScenarioEvidence,
    result: std::thread::Result<()>,
) -> Result<(), lattice_system_harness::FailureReport> {
    let primary = result.err().map(|error| {
        error
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| error.downcast_ref::<&str>().map(|text| text.to_string()))
            .unwrap_or_else(|| "PTY scenario panicked with a non-text payload".into())
    });
    evidence.finish_report(primary)
}

#[test]
fn pty_failure_report_preserves_redacted_panic_reason() {
    for (index, payload) in [
        Box::new("specific PTY wait failure FAKE_SECRET") as Box<dyn std::any::Any + Send>,
        Box::new(String::from("specific PTY result failure FAKE_SECRET")),
    ]
    .into_iter()
    .enumerate()
    {
        let world = ScenarioWorld::new("pty-panic-report").unwrap();
        let evidence = lattice_system_harness::ScenarioEvidence::new(
            &world,
            &format!("pty-panic-{index}"),
            ["FAKE_SECRET".into()],
        )
        .unwrap();
        let report = finish_pty(evidence, Err(payload)).unwrap_err();
        let rendered = report.to_string();
        assert!(rendered.contains("specific PTY"), "{rendered}");
        assert!(!rendered.contains("FAKE_SECRET"), "{rendered}");
        assert!(rendered.contains("[REDACTED]"), "{rendered}");
        println!("{rendered}");
    }
}
#[test]
fn pty_detach_reloads_pending_and_result_without_cancelling() {
    use lattice_system_harness::{SessionSendDriver, SseScript};
    let driver = driver();
    let (world, mut spec) =
        support::direct_world("pty-background-reconnect", "tui-smoke", "[]", "");
    spec.step_timeout_secs = 120;
    spec.total_timeout_secs = Some(300);
    let budget = spec.start_budget();
    let mut evidence = support::evidence(&world, &spec);
    let (server, gate) = support::wire_gated_fake_llm(
        &world,
        "tui-smoke",
        SseScript::plain("background reconnect answer"),
    );
    let session = SessionSendDriver::new(&driver, &world, &spec, "tui-smoke", "main").unwrap();
    let mut tui = PtyTuiDriver::reconnect(&driver, &world, session.session_id()).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tui.wait_for_ready(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        tui.send_line("persistent pending input").unwrap();
        gate.wait_until_stalled(1, budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        tui.wait_for(budget.cap_timeout(spec.step_timeout()), |text| {
            text.contains("已确认接受")
        })
        .unwrap();
        let owners = world.daemon_pids().unwrap();
        assert_eq!(owners.len(), 1);
        assert_ne!(tui.pgid(), Some(owners[0]));
        tui.send_keys(b"\x04").unwrap();
        tui.wait_for_exit(budget.cap_timeout(Duration::from_secs(15)))
            .unwrap();
        assert!(processes_in_pgid(tui.pgid().unwrap()).unwrap().is_empty());
        assert_eq!(world.daemon_pids().unwrap(), owners);
        evidence.record_pty(tui.output());
        let mut reconnected =
            PtyTuiDriver::reconnect(&driver, &world, session.session_id()).unwrap();
        let result = (|| {
            reconnected.wait_for_ready(budget.cap_timeout(spec.step_timeout()))?;
            reconnected.wait_for(budget.cap_timeout(spec.step_timeout()), |text| {
                text.contains("persistent pending input")
            })?;
            gate.release(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
            reconnected.wait_for(budget.cap_timeout(spec.step_timeout()), |text| {
                text.contains("background reconnect answer")
            })?;
            reconnected.send_line("/quit")?;
            reconnected.wait_for_exit(budget.cap_timeout(Duration::from_secs(15)))
        })();
        evidence.record_pty(reconnected.output());
        result.unwrap();
        world.stop_daemon().unwrap();
        let mut reopened = PtyTuiDriver::reconnect(&driver, &world, session.session_id()).unwrap();
        let reopened_result = (|| {
            reopened.wait_for_ready(budget.cap_timeout(spec.step_timeout()))?;
            reopened.wait_for(budget.cap_timeout(spec.step_timeout()), |text| {
                text.contains("background reconnect answer")
                    && text.contains("confirmation unknown")
                    && !text.contains("✓ committed")
            })?;
            reopened.send_line("/quit")?;
            reopened.wait_for_exit(budget.cap_timeout(Duration::from_secs(15)))
        })();
        evidence.record_pty(reopened.output());
        reopened_result.unwrap();
        assert_eq!(server.captured_requests().len(), 1);
    }));
    evidence.record_pty(tui.output());
    evidence.record_captures(server.captured_requests());
    finish_pty(evidence, result).unwrap_or_else(|report| panic!("{report}"));
}

#[test]
fn pty_second_ctrl_c_preserves_cancel_acknowledgement() {
    use lattice_system_harness::{SessionSendDriver, SseScript};
    let driver = driver();
    let (world, mut spec) = support::direct_world("pty-background-cancel", "tui-smoke", "[]", "");
    spec.step_timeout_secs = 120;
    spec.total_timeout_secs = Some(300);
    let budget = spec.start_budget();
    let mut evidence = support::evidence(&world, &spec);
    let (server, gate) = support::wire_gated_fake_llm(
        &world,
        "tui-smoke",
        SseScript::plain("must not be delivered"),
    );
    let session = SessionSendDriver::new(&driver, &world, &spec, "tui-smoke", "main").unwrap();
    let mut tui = PtyTuiDriver::reconnect(&driver, &world, session.session_id()).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tui.wait_for_ready(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        tui.send_line("cancel before exit").unwrap();
        gate.wait_until_stalled(1, budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        tui.wait_for(budget.cap_timeout(spec.step_timeout()), |text| {
            text.contains("已确认接受")
        })
        .unwrap();
        tui.send_keys(b"\x03\x03").unwrap();
        tui.wait_for_exit(budget.cap_timeout(Duration::from_secs(15)))
            .unwrap();
        assert!(processes_in_pgid(tui.pgid().unwrap()).unwrap().is_empty());
        let deadline = std::time::Instant::now() + budget.cap_timeout(Duration::from_secs(30));
        loop {
            let log = session
                .run_command(&["--json", "session", "log", session.session_id()])
                .unwrap();
            evidence.record_command("query cancellation after TUI exit", &log);
            assert!(log.is_clean_exit(0));
            let value: serde_json::Value = serde_json::from_slice(&log.stdout).unwrap();
            if let Some(turn) = value["turns"].as_array().unwrap().first() {
                assert!(turn["outcome"].get("cancelled").is_some());
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "cancelled Turn did not close"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(server.captured_requests().len(), 1);
    }));
    evidence.record_pty(tui.output());
    evidence.record_captures(server.captured_requests());
    finish_pty(evidence, result).unwrap_or_else(|report| panic!("{report}"));
}
