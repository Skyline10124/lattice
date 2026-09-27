// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
mod support;
use lattice_system_harness::{
    CommandOutcome, FAKE_API_KEY, LatticeCommand, RunningCommand, ScenarioBudget, ScenarioEvidence,
    ScenarioWorld, SessionSendDriver, SseScript,
};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};
use support::{
    assert_clean_send, assert_forced_kill, direct_world, driver, tool_call, wire_fake_llm,
    wire_gated_fake_llm,
};
const FIRST: &str = "00000000-0000-4000-8000-000000000001";
const SECOND: &str = "00000000-0000-4000-8000-000000000002";
const THIRD: &str = "00000000-0000-4000-8000-000000000003";
fn command(world: &ScenarioWorld, budget: &ScenarioBudget, args: &[&str]) -> RunningCommand {
    driver()
        .start(
            world,
            &LatticeCommand {
                args: args.iter().map(|s| (*s).into()).collect(),
                timeout: Some(budget.cap_timeout(Duration::from_secs(30))),
            },
        )
        .unwrap()
}
fn run(
    world: &ScenarioWorld,
    budget: &ScenarioBudget,
    evidence: &mut ScenarioEvidence,
    args: &[&str],
) -> CommandOutcome {
    let out = command(world, budget, args).wait(None).unwrap();
    evidence.record_command(args.join(" "), &out);
    out
}
fn lines(out: &CommandOutcome) -> Vec<Value> {
    assert!(out.stderr.is_empty());
    std::str::from_utf8(&out.stdout)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}
fn read(
    world: &ScenarioWorld,
    budget: &ScenarioBudget,
    evidence: &mut ScenarioEvidence,
    args: &[&str],
) -> Value {
    let out = run(world, budget, evidence, args);
    assert!(out.is_clean_exit(0), "{}", out.summary());
    serde_json::from_slice(&out.stdout).unwrap()
}
fn detail(
    world: &ScenarioWorld,
    budget: &ScenarioBudget,
    evidence: &mut ScenarioEvidence,
    turn: &str,
) -> Value {
    read(
        world,
        budget,
        evidence,
        &["--json", "session", "turn", turn],
    )
}
fn closed(
    world: &ScenarioWorld,
    budget: &ScenarioBudget,
    evidence: &mut ScenarioEvidence,
    turn: &str,
) -> Value {
    let deadline = Instant::now() + budget.cap_timeout(Duration::from_secs(30));
    loop {
        let t = detail(world, budget, evidence, turn);
        if !t["summary"].is_null() {
            return t;
        }
        assert!(Instant::now() < deadline, "Turn did not close");
        std::thread::yield_now();
    }
}
struct Wire {
    stream: UnixStream,
    message: u64,
}
impl Wire {
    fn new(world: &ScenarioWorld, budget: &ScenarioBudget, value: Value) -> Self {
        let mut stream =
            UnixStream::connect(world.session_store_dir().join("local/api.sock")).unwrap();
        let timeout = budget.cap_timeout(Duration::from_secs(30));
        stream.set_read_timeout(Some(timeout)).unwrap();
        stream.set_write_timeout(Some(timeout)).unwrap();
        let bytes = serde_json::to_vec(&value).unwrap();
        stream.write_all(b"LTLOCAL3").unwrap();
        stream
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .unwrap();
        stream.write_all(&bytes).unwrap();
        Self { stream, message: 0 }
    }
    fn next(&mut self) -> Value {
        let mut logical = String::new();
        loop {
            let mut header = [0; 12];
            self.stream.read_exact(&mut header).unwrap();
            assert_eq!(&header[..8], b"LTLOCAL3");
            let length = u32::from_be_bytes(header[8..].try_into().unwrap()) as usize;
            assert!((1..=16 * 1024 * 1024).contains(&length));
            let mut bytes = vec![0; length];
            self.stream.read_exact(&mut bytes).unwrap();
            let frame: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(frame["message_id"], self.message);
            assert_eq!(frame["offset"], logical.len());
            logical.push_str(frame["json_fragment"].as_str().unwrap());
            if frame["complete"] == true {
                self.message += 1;
                return serde_json::from_str(&logical).unwrap();
            }
        }
    }
}
fn ipc(world: &ScenarioWorld, budget: &ScenarioBudget, request: Value) -> Value {
    Wire::new(world, budget, request).next()
}
fn submit(
    world: &ScenarioWorld,
    sid: Option<&str>,
    turn: &str,
    input: &str,
    config: Value,
) -> Value {
    json!({"operation":"submit","request":{"session_id":sid,"turn_id":turn,"input":input,"workspace":world.project_dir(),"agent":"background"},"configuration":config})
}
fn view(world: &ScenarioWorld, budget: &ScenarioBudget, turn: &str) -> Value {
    let response = ipc(
        world,
        budget,
        json!({"operation":"query","session_id":null,"turn_id":turn}),
    );
    assert_eq!(response["response"], "view", "{response}");
    response["value"].clone()
}
fn subscribe(world: &ScenarioWorld, budget: &ScenarioBudget, turn: &str, cursor: Value) -> Wire {
    Wire::new(
        world,
        budget,
        json!({"operation":"subscribe","target":{"session_id":null,"turn_id":turn},"cursor":cursor}),
    )
}
fn finish(evidence: ScenarioEvidence, result: std::thread::Result<()>) {
    let primary = result.err().map(|e| {
        e.downcast_ref::<String>()
            .cloned()
            .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "non-text panic".into())
    });
    evidence.finish_report(primary).unwrap();
}
#[test]
fn detached_cli_finishes_and_world_owns_daemon_cleanup() {
    let (world, spec) = direct_world("background-cli-detach", "background", "[]", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (server, gate) = wire_gated_fake_llm(
                &world,
                "background",
                SseScript::plain("detached durable answer"),
            );
            let front = command(
                &world,
                &budget,
                &[
                    "--json",
                    "run",
                    "keep running",
                    "--agent",
                    "background",
                    "--turn-id",
                    FIRST,
                ],
            );
            gate.wait_until_stalled(1, budget.cap_timeout(Duration::from_secs(30)))
                .unwrap();
            let accepted = ipc(
                &world,
                &budget,
                submit(&world, None, FIRST, "keep running", Value::Null),
            );
            assert_eq!(accepted["value"]["acceptance"], "existing");
            assert_eq!(accepted["value"]["turn"]["turn_id"], FIRST);
            let owners = world.daemon_pids().unwrap();
            assert_eq!(owners.len(), 1);
            assert_ne!(owners[0], front.pgid());
            let killed = front.kill_and_wait().unwrap();
            evidence.record_command("detach CLI", &killed);
            assert_forced_kill(&killed);
            assert_eq!(world.daemon_pids().unwrap(), owners);
            assert!(detail(&world, &budget, &mut evidence, FIRST)["summary"].is_null());
            gate.release(budget.cap_timeout(Duration::from_secs(30)))
                .unwrap();
            assert_eq!(
                closed(&world, &budget, &mut evidence, FIRST)["summary"]["outcome"],
                "completed"
            );
            let observed = run(
                &world,
                &budget,
                &mut evidence,
                &["--json", "session", "turn", FIRST, "--follow"],
            );
            assert!(observed.is_clean_exit(0));
            assert!(lines(&observed).iter().any(
                |r| r["type"] == "history" && r.to_string().contains("detached durable answer")
            ));
            assert_eq!(server.captured_requests().len(), 1);
            evidence.record_captures(server.captured_requests());
            world.stop_daemon().unwrap();
            for pid in owners {
                assert!(
                    lattice_system_harness::processes_in_pgid(pid)
                        .unwrap()
                        .is_empty()
                );
            }
        }));
    finish(evidence, result);
}
#[test]
fn unread_acceptance_retry_and_restart_keep_one_turn() {
    let (world, spec) = direct_world("background-unread-acceptance", "background", "[]", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (server, gate) = wire_gated_fake_llm(
            &world,
            "background",
            SseScript::plain("one request one Turn"),
        );
        let bootstrap = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "list"],
        );
        assert!(bootstrap.is_clean_exit(1)); // 只自启后台，不创建库。
        let config = json!({"profile_toml":std::fs::read_to_string(world.project_dir().join(".lattice/agents/background/agent.toml")).unwrap(),"system_prompt":"You are a concise test agent.","connection":{"alias":"compat:background","provider":"openai_compatible","provider_model":"background","base_url":server.base_url(),"api_key":FAKE_API_KEY,"roots_der":[]}});
        let unread = Wire::new(
            &world,
            &budget,
            submit(&world, None, FIRST, "identical input", config),
        );
        gate.wait_until_stalled(1, budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        let pending = detail(&world, &budget, &mut evidence, FIRST);
        assert!(pending["summary"].is_null());
        let sid = pending["session_id"].as_str().unwrap();
        drop(unread);
        let retry = ipc(
            &world,
            &budget,
            submit(&world, None, FIRST, "identical input", Value::Null),
        );
        assert_eq!(retry["value"]["acceptance"], "existing");
        assert_eq!(retry["value"]["turn"]["session_id"], sid);
        let conflict = ipc(
            &world,
            &budget,
            submit(&world, None, FIRST, "different input", Value::Null),
        );
        assert_eq!(conflict["value"]["kind"], "conflict");
        gate.release(budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        let original = closed(&world, &budget, &mut evidence, FIRST);
        world.stop_daemon().unwrap();
        world.unset_env("OPENAI_COMPATIBLE_API_KEY");
        let profile = world
            .project_dir()
            .join(".lattice/agents/background/agent.toml");
        let saved = std::fs::read_to_string(&profile).unwrap();
        std::fs::remove_file(&profile).unwrap();
        let repeat = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "run", "identical input", "--turn-id", FIRST],
        );
        assert!(repeat.is_clean_exit(0));
        assert_eq!(detail(&world, &budget, &mut evidence, FIRST), original);
        assert_eq!(server.captured_requests().len(), 1);
        let list = read(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "list"],
        );
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(list[0]["node_count"], 3);
        std::fs::write(profile, saved).unwrap();
        world.set_env("OPENAI_COMPATIBLE_API_KEY", FAKE_API_KEY);
        let independent = run(
            &world,
            &budget,
            &mut evidence,
            &[
                "--json",
                "run",
                "identical input",
                "--session",
                sid,
                "--turn-id",
                SECOND,
            ],
        );
        assert!(independent.is_clean_exit(0));
        assert_eq!(server.captured_requests().len(), 2);
        evidence.record_captures(server.captured_requests());
    }));
    finish(evidence, result);
}
#[test]
fn busy_cancel_and_stop_do_not_serialize_distinct_sessions() {
    let (world, spec) = direct_world("background-concurrent-stop", "background", "[]", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (server, gate) =
            wire_gated_fake_llm(&world, "background", SseScript::plain("parallel answer"));
        let first = command(
            &world,
            &budget,
            &[
                "--json",
                "run",
                "parallel a",
                "--agent",
                "background",
                "--turn-id",
                FIRST,
            ],
        );
        gate.wait_until_stalled(1, budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        let first_detail = detail(&world, &budget, &mut evidence, FIRST);
        let a = first_detail["session_id"].as_str().unwrap();
        let second = command(
            &world,
            &budget,
            &[
                "--json",
                "run",
                "parallel b",
                "--agent",
                "background",
                "--turn-id",
                SECOND,
            ],
        );
        gate.wait_until_stalled(2, budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        let busy = ipc(
            &world,
            &budget,
            submit(&world, Some(a), THIRD, "must not queue", Value::Null),
        );
        assert_eq!(busy["value"]["kind"], "busy");
        let cancel = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "cancel", a, FIRST],
        );
        assert!(cancel.is_clean_exit(0));
        assert_eq!(lines(&cancel)[0]["owned"], true);
        let detached = first.kill_and_wait().unwrap();
        evidence.record_command("detach after Taken", &detached);
        assert_forced_kill(&detached);
        assert_eq!(
            closed(&world, &budget, &mut evidence, FIRST)["summary"]["outcome"],
            "cancelled"
        );
        let stop = command(&world, &budget, &["--json", "daemon", "stop"]);
        let deadline = Instant::now() + budget.cap_timeout(Duration::from_secs(30));
        loop {
            if ipc(&world, &budget, json!({"operation":"status"}))["value"]["stopping"] == true {
                break;
            }
            assert!(Instant::now() < deadline);
        }
        assert_eq!(
            ipc(
                &world,
                &budget,
                submit(&world, Some(a), THIRD, "after stop", Value::Null)
            )["value"]["kind"],
            "stopping"
        );
        assert!(detail(&world, &budget, &mut evidence, SECOND)["summary"].is_null());
        gate.release(budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        let done = second.wait(None).unwrap();
        evidence.record_command("parallel completion", &done);
        if done.is_clean_exit(0) {
            assert!(
                lines(&done)
                    .iter()
                    .any(|r| r["type"] == "history" && r.to_string().contains("parallel answer"))
            );
        } else {
            assert!(done.is_clean_exit(1));
            assert!(lines(&done).iter().any(|r| r["type"] == "error"));
        }
        let drained = stop.wait(None).unwrap();
        evidence.record_command("stop drains", &drained);
        assert!(drained.is_clean_exit(0));
        assert_eq!(lines(&drained)[0]["owners"], 0);
        world.stop_daemon().unwrap();
        assert_eq!(
            detail(&world, &budget, &mut evidence, SECOND)["summary"]["outcome"],
            "completed"
        );
        assert_eq!(server.captured_requests().len(), 2);
        evidence.record_captures(server.captured_requests());
    }));
    finish(evidence, result);
}
#[test]
fn concurrent_autostart_has_one_owner_and_snapshot_reloads_result() {
    let (world, spec) = direct_world("background-start-snapshot", "background", "[]", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (server, gate) =
            wire_gated_fake_llm(&world, "background", SseScript::plain("大".repeat(24000)));
        let a = command(
            &world,
            &budget,
            &[
                "--json",
                "run",
                "large a",
                "--agent",
                "background",
                "--turn-id",
                FIRST,
            ],
        );
        let b = command(
            &world,
            &budget,
            &[
                "--json",
                "run",
                "large b",
                "--agent",
                "background",
                "--turn-id",
                SECOND,
            ],
        );
        gate.wait_until_stalled(2, budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        assert_eq!(world.daemon_pids().unwrap().len(), 1);
        let mut observer = subscribe(
            &world,
            &budget,
            FIRST,
            json!({"epoch":"previous-backend","sequence":1}),
        );
        let snapshot = observer.next();
        assert_eq!(snapshot["value"]["type"], "snapshot");
        assert!(snapshot["value"]["view"]["selected_turn"]["summary"].is_null());
        drop(observer);
        gate.release(budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        for front in [a, b] {
            let done = front.wait(None).unwrap();
            evidence.record_command("concurrent first send", &done);
            assert!(done.is_clean_exit(0));
            assert!(
                lines(&done)
                    .iter()
                    .any(|r| r["type"] == "history" && r.to_string().contains(&"大".repeat(24000)))
            );
        }
        assert_eq!(server.captured_requests().len(), 2);
        evidence.record_captures(server.captured_requests());
    }));
    finish(evidence, result);
}
#[test]
fn slow_subscriber_cannot_block_fixed_result_or_other_queries() {
    let (world, spec) = direct_world("background-slow-subscriber", "background", "[]", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let chunk = "slow-reader-".repeat(100);
        let text = chunk.repeat(1600);
        let (server, gate) = wire_gated_fake_llm(
            &world,
            "background",
            SseScript::PlainText {
                chunks: vec![chunk; 1600],
                finish_reason: "stop".into(),
            },
        );
        let front = command(
            &world,
            &budget,
            &[
                "--json",
                "run",
                "slow observer",
                "--agent",
                "background",
                "--turn-id",
                FIRST,
            ],
        );
        gate.wait_until_stalled(1, budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        let mut slow = subscribe(&world, &budget, FIRST, Value::Null);
        assert_eq!(slow.next()["value"]["type"], "snapshot");
        gate.release(budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        let done = front.wait(None).unwrap();
        evidence.record_command("normal beside slow", &done);
        assert!(done.is_clean_exit(0), "{}", done.summary());
        assert!(
            lines(&done)
                .iter()
                .any(|r| r["type"] == "history" && r.to_string().contains(&text))
        );
        assert_eq!(
            closed(&world, &budget, &mut evidence, FIRST)["summary"]["outcome"],
            "completed"
        );
        drop(slow);
        assert_eq!(server.captured_requests().len(), 1);
        evidence.record_captures(server.captured_requests());
    }));
    finish(evidence, result);
}
#[test]
fn shared_daemon_keeps_project_endpoints_keys_and_profiles_frozen() {
    use lattice_system_harness::{FakeLlmServer, assert_no_secret};
    let (world, spec) = direct_world(
        "background-project-config",
        "background",
        "[\"session.todo_write\"]",
        "",
    );
    let budget = world.budget(&spec);
    let key_a = "project-alpha-private-fixture-key";
    let key_b = "project-beta-private-fixture-key";
    let mut evidence =
        ScenarioEvidence::new(&world, &spec.name, [key_a.into(), key_b.into()]).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let script = SseScript::Sequence {
            scripts: vec![
                tool_call(
                    "todo",
                    "session.todo_write",
                    r#"{"todos":[{"content":"isolation","status":"pending"}]}"#,
                ),
                SseScript::plain("frozen result"),
            ],
        };
        let (a, ga) = FakeLlmServer::start_gated(script.clone()).unwrap();
        let (b, gb) = FakeLlmServer::start_gated(script).unwrap();
        a.expect_api_key(key_a);
        b.expect_api_key(key_b);
        world.set_env("OPENAI_COMPATIBLE_BASE_URL", a.base_url());
        world.unset_env("OPENAI_COMPATIBLE_API_KEY");
        world
            .write_project_file(
                ".env",
                &format!(
                    "OPENAI_COMPATIBLE_API_KEY={key_a}\nOPENAI_COMPATIBLE_BASE_URL={}\n",
                    b.base_url()
                ),
            )
            .unwrap();
        let project = world.root().join("second-project");
        std::fs::create_dir_all(project.join(".lattice/agents/background")).unwrap();
        let profile = world
            .project_dir()
            .join(".lattice/agents/background/agent.toml");
        std::fs::write(
            project.join(".lattice/agents/background/agent.toml"),
            std::fs::read_to_string(&profile)
                .unwrap()
                .replace("concise test agent", "beta test agent"),
        )
        .unwrap();
        std::fs::write(
            project.join(".env"),
            format!(
                "OPENAI_COMPATIBLE_API_KEY={key_b}\nOPENAI_COMPATIBLE_BASE_URL={}\n",
                a.base_url()
            ),
        )
        .unwrap();
        gix::init(&project).unwrap();
        let first = command(
            &world,
            &budget,
            &[
                "--json",
                "run",
                "alpha",
                "--agent",
                "background",
                "--turn-id",
                FIRST,
            ],
        );
        ga.wait_until_stalled(1, budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        let mut cmd = world.command(driver().binary_path());
        cmd.current_dir(&project)
            .env("OPENAI_COMPATIBLE_BASE_URL", b.base_url())
            .args([
                "--json",
                "run",
                "beta",
                "--agent",
                "background",
                "--turn-id",
                SECOND,
            ]);
        let second = RunningCommand::spawn(cmd).unwrap();
        gb.wait_until_stalled(1, budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        assert_eq!(world.daemon_pids().unwrap().len(), 1);
        std::fs::write(profile, "invalid changed profile").unwrap();
        world
            .write_project_file(".env", &format!("OPENAI_COMPATIBLE_API_KEY={key_b}\n"))
            .unwrap();
        ga.release(budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        gb.release(budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        let first = first.wait(None).unwrap();
        let second = second
            .wait(Some(budget.cap_timeout(Duration::from_secs(30))))
            .unwrap();
        for (name, out) in [("alpha", &first), ("beta", &second)] {
            evidence.record_command(name, out);
            assert!(out.is_clean_exit(0), "{name}: {}", out.summary());
            assert_no_secret(out, key_a).unwrap();
            assert_no_secret(out, key_b).unwrap();
        }
        for (server, prompt) in [
            (&a, "You are a concise test agent."),
            (&b, "You are a beta test agent."),
        ] {
            let captures = server.captured_requests();
            assert_eq!(captures.len(), 2);
            for c in &captures {
                assert_eq!(c.raw["messages"][0]["content"], prompt);
            }
            evidence.record_captures(captures);
        }
    }));
    finish(evidence, result);
}
#[test]
fn later_session_driver_keeps_the_scenario_budget_origin() {
    let (world, spec) = direct_world("background-budget-origin", "background", "[]", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert!(run(&world, &budget, &mut evidence, &["--help"]).is_clean_exit(0));
        let consumed = budget.elapsed();
        let creating = Instant::now();
        let driver = driver();
        let send = SessionSendDriver::new(&driver, &world, &spec, "background");
        let elapsed = creating.elapsed();
        assert!(send.budget().elapsed() >= consumed + elapsed);
        assert_eq!(
            send.budget().remaining().is_some(),
            budget.remaining().is_some()
        );
    }));
    finish(evidence, result);
}
#[test]
fn exhausted_budget_records_normal_stop_failure_without_fixed_wait() {
    let (world, mut spec) = direct_world("background-budget-exhausted", "background", "[]", "");
    spec.total_timeout_secs = Some(0);
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_eq!(budget.remaining(), Some(Duration::ZERO));
        let root = world
            .session_store_dir()
            .parent()
            .unwrap()
            .display()
            .to_string();
        let daemon = driver()
            .start(
                &world,
                &LatticeCommand {
                    args: ["daemon", "serve", "--state-root", &root]
                        .iter()
                        .map(|s| (*s).into())
                        .collect(),
                    timeout: Some(spec.step_timeout()),
                },
            )
            .unwrap();
        let deadline = Instant::now() + spec.step_timeout();
        while world.daemon_pids().unwrap() != vec![daemon.pgid()] {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        let start = Instant::now();
        let errors = world
            .stop_daemon()
            .expect_err("normal stop must exhaust budget");
        assert!(errors.iter().any(|s| s.contains("budget")));
        assert!(start.elapsed() < Duration::from_secs(10));
        assert!(world.daemon_pids().unwrap().is_empty());
        let killed = daemon.wait(None).unwrap();
        evidence.record_command("exhausted budget cleanup", &killed);
        assert!(killed.cleanup.surviving_pids.is_empty());
    }));
    finish(evidence, result);
}
#[test]
fn daemon_restart_reads_closed_result_without_resubmitting() {
    let (world, spec) = direct_world(
        "background-restart-read",
        "background",
        "[\"session.todo_write\"]",
        "",
    );
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let server = wire_fake_llm(
            &world,
            "background",
            SseScript::Sequence {
                scripts: vec![
                    tool_call(
                        "todo",
                        "session.todo_write",
                        r#"{"todos":[{"content":"durable","status":"pending"}]}"#,
                    ),
                    SseScript::plain("restart answer"),
                ],
            },
        );
        let driver = driver();
        let send = SessionSendDriver::new(&driver, &world, &spec, "background");
        let sent = send.send("restart prompt").unwrap();
        evidence.record_command("before restart", &sent.command);
        assert_clean_send(&sent).unwrap();
        let tid = sent.accepted_turn_id().unwrap();
        let original = detail(&world, &budget, &mut evidence, tid);
        world.stop_daemon().unwrap();
        world.unset_env("OPENAI_COMPATIBLE_API_KEY");
        assert_eq!(detail(&world, &budget, &mut evidence, tid), original);
        let mut observer = subscribe(&world, &budget, tid, Value::Null);
        let snapshot = observer.next();
        assert_eq!(snapshot["value"]["type"], "snapshot");
        assert_eq!(snapshot["value"]["view"]["selected_turn"], original);
        let done = observer.next();
        assert_eq!(done["value"]["type"], "finished");
        assert_eq!(
            done["value"]["history"]["entries"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert!(done.to_string().contains("restart answer"));
        assert_eq!(server.captured_requests().len(), 2);
        evidence
            .record_json("closed-subscription", &json!([snapshot, done]))
            .unwrap();
        evidence.record_captures(server.captured_requests());
    }));
    finish(evidence, result);
}
#[test]
fn a_live_new_turn_does_not_block_old_closed_observation() {
    let (world, spec) = direct_world("background-read-beside-live", "background", "[]", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (server, gate) = wire_gated_fake_llm(&world, "background", SseScript::plain("answer"));
        let driver = driver();
        let send = SessionSendDriver::new(&driver, &world, &spec, "background");
        let first = send.spawn("first").unwrap();
        gate.wait_until_stalled(1, budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        gate.release_next(budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        let first = send.wait_spawned(first).unwrap();
        evidence.record_command("first", &first.command);
        assert!(first.has_completed());
        let tid = first.accepted_turn_id().unwrap();
        let old = detail(&world, &budget, &mut evidence, tid);
        let second = send.spawn("second").unwrap();
        gate.wait_until_stalled(2, budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        let before = view(&world, &budget, tid);
        assert!(!before["session"]["active_turn"].is_null());
        let observed = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "turn", tid, "--follow"],
        );
        assert!(observed.is_clean_exit(0), "{}", observed.summary());
        assert!(
            lines(&observed)
                .iter()
                .any(|r| r["type"] == "history" && r.to_string().contains("second"))
        );
        let still = view(&world, &budget, tid);
        assert_eq!(
            still["session"]["active_turn"],
            before["session"]["active_turn"]
        );
        assert_eq!(still["selected_turn"], old);
        gate.release_next(budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        let second = send.wait_spawned(second).unwrap();
        evidence.record_command("second", &second.command);
        assert!(second.has_completed());
        assert_eq!(detail(&world, &budget, &mut evidence, tid), old);
        assert_eq!(server.captured_requests().len(), 2);
        evidence.record_captures(server.captured_requests());
    }));
    finish(evidence, result);
}
#[test]
fn global_turn_identity_rejects_a_different_session_without_dispatch() {
    let (world, spec) = direct_world(
        "background-global-turn-identity",
        "background",
        "[\"session.todo_write\"]",
        "",
    );
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let server = wire_fake_llm(
            &world,
            "background",
            SseScript::Cycle {
                scripts: vec![
                    tool_call(
                        "todo",
                        "session.todo_write",
                        r#"{"todos":[{"content":"shared","status":"pending"}]}"#,
                    ),
                    SseScript::plain("answer"),
                ],
            },
        );
        let first = run(
            &world,
            &budget,
            &mut evidence,
            &[
                "--json",
                "run",
                "alpha",
                "--agent",
                "background",
                "--turn-id",
                FIRST,
            ],
        );
        let second = run(
            &world,
            &budget,
            &mut evidence,
            &[
                "--json",
                "run",
                "beta",
                "--agent",
                "background",
                "--turn-id",
                SECOND,
            ],
        );
        assert!(first.is_clean_exit(0) && second.is_clean_exit(0));
        let a = detail(&world, &budget, &mut evidence, FIRST);
        let b = detail(&world, &budget, &mut evidence, SECOND);
        assert_ne!(a["session_id"], b["session_id"]);
        let conflict = ipc(
            &world,
            &budget,
            submit(
                &world,
                b["session_id"].as_str(),
                FIRST,
                "alpha",
                Value::Null,
            ),
        );
        assert_eq!(conflict["value"]["kind"], "conflict");
        assert_eq!(detail(&world, &budget, &mut evidence, FIRST), a);
        assert_eq!(detail(&world, &budget, &mut evidence, SECOND), b);
        assert_eq!(server.captured_requests().len(), 4);
        let inv_a = &a["steps"][0]["state"]["slots"][0]["invocation_id"];
        let inv_b = &b["steps"][0]["state"]["slots"][0]["invocation_id"];
        assert_ne!(inv_a, inv_b);
        evidence.record_captures(server.captured_requests());
    }));
    finish(evidence, result);
}
