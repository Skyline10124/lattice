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
fn archive_state(
    world: &ScenarioWorld,
    budget: &ScenarioBudget,
    evidence: &mut ScenarioEvidence,
    turns: &[&str],
) {
    let list = read(world, budget, evidence, &["--json", "session", "list"]);
    let mut records = Vec::new();
    for turn in turns {
        let record = detail(world, budget, evidence, turn);
        let sid = record["session_id"].as_str().unwrap();
        read(world, budget, evidence, &["--json", "session", "log", sid]);
        let tree = run(world, budget, evidence, &["session", "log", sid]);
        assert!(tree.is_clean_exit(0));
        read(
            world,
            budget,
            evidence,
            &[
                "--json",
                "audit",
                "replay",
                "--session",
                sid,
                "--turn",
                turn,
            ],
        );
        records.push(record);
    }
    read(
        world,
        budget,
        evidence,
        &["--json", "audit", "--tail", "--since", "0"],
    );
    read(world, budget, evidence, &["--json", "lancet", "status"]);
    evidence
        .record_json("state-summary", &json!({"list":list,"turns":records}))
        .unwrap();
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
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
        assert!(
            lines(&observed).iter().any(
                |r| r["type"] == "history" && r.to_string().contains("detached durable answer")
            )
        );
        assert_eq!(server.captured_requests().len(), 1);
        evidence.record_captures(server.captured_requests());
        archive_state(&world, &budget, &mut evidence, &[FIRST]);
        evidence.record_json("parameters", &json!({"turn":FIRST,"script":"gated plain detached durable answer","frontend":"SIGKILL after existing receipt","backend_pids":owners})).unwrap();
        world.stop_daemon().unwrap();
        for pid in owners {
            assert!(
                lattice_system_harness::processes_in_pgid(pid)
                    .unwrap()
                    .is_empty()
            );
        }
    }));
    support::finish_recorded(
        evidence,
        result,
        "direct_background",
        "detached_cli_finishes_and_world_owns_daemon_cleanup",
    );
}
#[test]
fn unread_acceptance_retry_and_restart_keep_one_turn() {
    for existing_session in [false, true] {
        for restart in [false, true] {
            let (world, spec) = direct_world(
                &format!("background-unread-{existing_session}-{restart}"),
                "background",
                "[]",
                "",
            );
            let budget = world.budget(&spec);
            let mut evidence = support::evidence(&world, &spec);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let existing = existing_session.then(|| {
                    let mut seed =
                        wire_fake_llm(&world, "background", SseScript::plain("seed reply"));
                    let out = run(
                        &world,
                        &budget,
                        &mut evidence,
                        &[
                            "--json",
                            "run",
                            "seed input",
                            "--agent",
                            "background",
                            "--turn-id",
                            THIRD,
                        ],
                    );
                    assert!(out.is_clean_exit(0));
                    let seed_turn = detail(&world, &budget, &mut evidence, THIRD);
                    evidence
                        .record_json("seed-provider", &json!(seed.captured_requests()))
                        .unwrap();
                    seed.shutdown(budget.cap_timeout(Duration::from_secs(30)))
                        .unwrap();
                    seed_turn["session_id"].as_str().unwrap().to_owned()
                });
                let (mut server, gate) = wire_gated_fake_llm(
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
                assert!(bootstrap.is_clean_exit(if existing_session { 0 } else { 1 }));
                let config = json!({"profile_toml":std::fs::read_to_string(world.project_dir().join(".lattice/agents/background/agent.toml")).unwrap(),"system_prompt":"You are a concise test agent.","connection":{"alias":"compat:background","provider":"openai_compatible","provider_model":"background","base_url":server.base_url(),"api_key":FAKE_API_KEY,"roots_der":[]}});
                let unread = Wire::new(
                    &world,
                    &budget,
                    submit(
                        &world,
                        existing.as_deref(),
                        FIRST,
                        "identical input",
                        config,
                    ),
                );
                gate.wait_until_stalled(1, budget.cap_timeout(Duration::from_secs(30)))
                    .unwrap();
                let pending = detail(&world, &budget, &mut evidence, FIRST);
                assert!(pending["summary"].is_null());
                let sid = pending["session_id"].as_str().unwrap();
                if let Some(existing) = &existing {
                    assert_eq!(sid, existing);
                }
                // 此连接只写请求，从未调用 next/read；独立查询证明接受已持久。
                drop(unread);
                let epoch =
                    ipc(&world, &budget, json!({"operation":"status"}))["value"]["epoch"].clone();
                evidence.record_json("unread-receipt-window", &json!({"existing_session":existing_session,
                    "restart_before_first_retry":restart,"pending":pending,"epoch":epoch,"request_id":FIRST,
                    "input":"identical input","receipt_read":false,"provider_arrivals":1})).unwrap();
                if !restart {
                    let retry = ipc(
                        &world,
                        &budget,
                        submit(
                            &world,
                            existing.as_deref(),
                            FIRST,
                            "identical input",
                            Value::Null,
                        ),
                    );
                    assert_eq!(retry["value"]["acceptance"], "existing");
                    assert_eq!(retry["value"]["turn"]["session_id"], sid);
                    evidence.record_json("running-retry", &retry).unwrap();
                }
                gate.release(budget.cap_timeout(Duration::from_secs(30)))
                    .unwrap();
                let original = closed(&world, &budget, &mut evidence, FIRST);
                if restart {
                    world.stop_daemon().unwrap();
                }
                world.unset_env("OPENAI_COMPATIBLE_API_KEY");
                let profile = world
                    .project_dir()
                    .join(".lattice/agents/background/agent.toml");
                let saved = std::fs::read_to_string(&profile).unwrap();
                std::fs::remove_file(&profile).unwrap();
                let mut args = vec!["--json", "run", "identical input", "--turn-id", FIRST];
                if existing_session {
                    args.extend(["--session", sid]);
                }
                let repeat = run(&world, &budget, &mut evidence, &args);
                assert!(repeat.is_clean_exit(0), "{}", repeat.summary());
                let current_epoch =
                    ipc(&world, &budget, json!({"operation":"status"}))["value"]["epoch"].clone();
                assert_eq!(epoch != current_epoch, restart);
                assert_eq!(detail(&world, &budget, &mut evidence, FIRST), original);
                assert_eq!(server.captured_requests().len(), 1);
                let conflict = ipc(
                    &world,
                    &budget,
                    submit(
                        &world,
                        existing.as_deref(),
                        FIRST,
                        "different input",
                        Value::Null,
                    ),
                );
                assert_eq!(conflict["value"]["kind"], "conflict");
                let list = read(
                    &world,
                    &budget,
                    &mut evidence,
                    &["--json", "session", "list"],
                );
                assert_eq!(list.as_array().unwrap().len(), 1);
                assert_eq!(list[0]["node_count"], if existing_session { 5 } else { 3 });
                evidence.record_json("identity-retry", &json!({"original":original,"epoch_after":current_epoch,"conflict":conflict,"list":list})).unwrap();
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
                archive_state(&world, &budget, &mut evidence, &[FIRST, SECOND]);
                evidence
                    .record_json(
                        "parameters",
                        &json!({"existing_session":existing_session,"restart":restart,
                    "script":"one request one Turn","state_root":world.session_store_dir().parent(),
                    "provider_requests":2,"backend":world.daemon_pids().unwrap()}),
                    )
                    .unwrap();
                evidence.record_captures(server.captured_requests());
                server
                    .shutdown(budget.cap_timeout(Duration::from_secs(30)))
                    .unwrap();
            }));
            support::finish_recorded(
                evidence,
                result,
                "direct_background",
                "unread_acceptance_retry_and_restart_keep_one_turn",
            );
        }
    }
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
        let second_detail = detail(&world, &budget, &mut evidence, SECOND);
        evidence
            .record_json(
                "concurrent-window",
                &json!({"provider_arrivals":server.captured_requests().len(),
            "first":first_detail,"second":second_detail,"busy":busy,
            "backend":world.daemon_pids().unwrap(),"gate_released":false}),
            )
            .unwrap();
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
        archive_state(&world, &budget, &mut evidence, &[FIRST, SECOND]);
        evidence.record_json("parameters", &json!({"script":"parallel answer","requests":["parallel a","parallel b"],
            "state_root":world.session_store_dir().parent(),"provider_requests":2,"stop":lines(&drained)})).unwrap();
    }));
    support::finish_recorded(
        evidence,
        result,
        "direct_background",
        "busy_cancel_and_stop_do_not_serialize_distinct_sessions",
    );
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
        archive_state(&world, &budget, &mut evidence, &[FIRST, SECOND]);
        evidence.record_json("parameters", &json!({"script":"gated 24000 Chinese characters","turns":[FIRST,SECOND],"snapshot":snapshot})).unwrap();
        evidence.record_captures(server.captured_requests());
    }));
    support::finish_recorded(
        evidence,
        result,
        "direct_background",
        "concurrent_autostart_has_one_owner_and_snapshot_reloads_result",
    );
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
        archive_state(&world, &budget, &mut evidence, &[FIRST]);
        evidence
            .record_json(
                "parameters",
                &json!({"script":"slow-reader- repeated 160000 times","turn":FIRST}),
            )
            .unwrap();
        evidence.record_captures(server.captured_requests());
    }));
    support::finish_recorded(
        evidence,
        result,
        "direct_background",
        "slow_subscriber_cannot_block_fixed_result_or_other_queries",
    );
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
        archive_state(&world, &budget, &mut evidence, &[FIRST]);
        let beta = detail(&world, &budget, &mut evidence, SECOND);
        let sid = beta["session_id"].as_str().unwrap();
        for args in [
            vec!["--json", "session", "log", sid],
            vec!["session", "log", sid],
            vec!["--json", "session", "turn", SECOND],
            vec!["--json", "audit", "--tail"],
            vec![
                "--json",
                "audit",
                "replay",
                "--session",
                sid,
                "--turn",
                SECOND,
            ],
            vec!["--json", "lancet", "status"],
        ] {
            let mut cmd = world.command(driver().binary_path());
            cmd.current_dir(&project).args(&args);
            let out = RunningCommand::spawn(cmd)
                .unwrap()
                .wait(Some(budget.cap_timeout(Duration::from_secs(30))))
                .unwrap();
            evidence.record_command(format!("beta {}", args.join(" ")), &out);
            assert!(out.is_clean_exit(0), "{}", out.summary());
        }
        evidence.record_json("beta-state", &beta).unwrap();
        evidence.record_json("parameters", &json!({"turns":[FIRST,SECOND],"prompts":["alpha","beta"],"methods":["session.todo_write"],"final_reply":"frozen result","provider_requests":[2,2],"source_changed_after_both_gates":true})).unwrap();
    }));
    support::finish_recorded(
        evidence,
        result,
        "direct_background",
        "shared_daemon_keeps_project_endpoints_keys_and_profiles_frozen",
    );
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
        archive_state(&world, &budget, &mut evidence, &[tid]);
        evidence
            .record_json(
                "parameters",
                &json!({"script":"todo durable then restart answer","turn":tid,"restart":true}),
            )
            .unwrap();
        evidence.record_captures(server.captured_requests());
    }));
    support::finish_recorded(
        evidence,
        result,
        "direct_background",
        "daemon_restart_reads_closed_result_without_resubmitting",
    );
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
        archive_state(
            &world,
            &budget,
            &mut evidence,
            &[tid, second.accepted_turn_id().unwrap()],
        );
        evidence.record_json("parameters", &json!({"script":"two gated answer Turns","old_turn":tid,"during_live":before,"after_observation":still})).unwrap();
        evidence.record_captures(server.captured_requests());
    }));
    support::finish_recorded(
        evidence,
        result,
        "direct_background",
        "a_live_new_turn_does_not_block_old_closed_observation",
    );
}
#[test]
fn frozen_tls_roots_and_policies_are_isolated_between_concurrent_turns() {
    use lattice_system_harness::FakeLlmServer;
    let (world, spec) = direct_world(
        "background-real-tls",
        "background",
        "[\"fetch.web\"]",
        "\n[llm.retry]\nmax_retries = 0\n[lancet.capabilities]\nmax_boundary = \"ex\"\n[lancet.network]\nallow_localhost = true\nallowed_hosts = [\"localhost\"]\n",
    );
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // 直接启动带专属环境的前端前，先经标准驱动登记产物与后台清理归属。
        assert!(run(&world, &budget, &mut evidence, &["--help"]).is_clean_exit(0));
        let fixture =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tls_proxy.py");
        let mut proxies = Vec::new();
        let mut roots = Vec::new();
        let mut urls = Vec::new();
        for name in ["a", "b"] {
            let root = world.root().join(format!("tls-{name}"));
            std::fs::create_dir(&root).unwrap();
            std::fs::create_dir(root.join("empty-certs")).unwrap();
            std::fs::write(root.join("leaf.ext"), "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost\n").unwrap();
            for args in [
                vec![
                    "req",
                    "-x509",
                    "-newkey",
                    "rsa:2048",
                    "-nodes",
                    "-keyout",
                    "ca-key.pem",
                    "-out",
                    "ca.pem",
                    "-days",
                    "2",
                    "-config",
                    "/dev/null",
                    "-subj",
                    "/CN=Harness CA",
                    "-addext",
                    "basicConstraints=critical,CA:TRUE",
                    "-addext",
                    "keyUsage=critical,keyCertSign,cRLSign",
                ],
                vec![
                    "req",
                    "-new",
                    "-newkey",
                    "rsa:2048",
                    "-nodes",
                    "-keyout",
                    "key.pem",
                    "-out",
                    "leaf.csr",
                    "-config",
                    "/dev/null",
                    "-subj",
                    "/CN=localhost",
                ],
                vec![
                    "x509",
                    "-req",
                    "-in",
                    "leaf.csr",
                    "-CA",
                    "ca.pem",
                    "-CAkey",
                    "ca-key.pem",
                    "-CAcreateserial",
                    "-out",
                    "cert.pem",
                    "-days",
                    "2",
                    "-extfile",
                    "leaf.ext",
                ],
                vec!["x509", "-in", "ca.pem", "-outform", "DER", "-out", "ca.der"],
            ] {
                let mut cmd = world.command("openssl");
                cmd.current_dir(&root).args(&args);
                let out = RunningCommand::spawn(cmd)
                    .unwrap()
                    .wait(Some(budget.cap_timeout(spec.step_timeout())))
                    .unwrap();
                evidence.record_command(
                    format!("generate ephemeral TLS {name}: {}", args.join(" ")),
                    &out,
                );
                assert!(out.is_clean_exit(0), "{}", out.summary());
            }
            let mut cmd = world.command("python3");
            cmd.arg(&fixture).arg(&root);
            let proxy = RunningCommand::spawn(cmd).unwrap();
            let deadline = Instant::now() + budget.cap_timeout(Duration::from_secs(10));
            let port = loop {
                if let Ok(bytes) = std::fs::read(root.join("ready.json"))
                    && let Ok(ready) = serde_json::from_slice::<Value>(&bytes)
                {
                    break ready["port"].as_u64().unwrap();
                }
                assert!(Instant::now() < deadline, "TLS fixture not ready");
                std::thread::sleep(Duration::from_millis(5));
            };
            urls.push(format!("https://localhost:{port}"));
            roots.push(root);
            proxies.push(proxy);
        }
        let (a, ga) = FakeLlmServer::start_gated(support::sequence(vec![
            SseScript::ToolCalls {
                calls: vec![
                    (
                        "own".into(),
                        "fetch.web".into(),
                        json!({"url":format!("{}/own-a",urls[0])}).to_string(),
                    ),
                    (
                        "cross".into(),
                        "fetch.web".into(),
                        json!({"url":format!("{}/cross-from-a",urls[1])}).to_string(),
                    ),
                ],
            },
            SseScript::plain("TLS alpha complete"),
        ]))
        .unwrap();
        let (b, gb) = FakeLlmServer::start_gated(support::sequence(vec![
            tool_call(
                "denied",
                "fetch.web",
                &json!({"url":format!("{}/denied-b",urls[1])}).to_string(),
            ),
            SseScript::plain("TLS beta complete"),
        ]))
        .unwrap();
        for (root, provider) in roots.iter().zip([&a, &b]) {
            std::fs::write(
                root.join("upstream-port"),
                provider.base_url().rsplit(':').next().unwrap(),
            )
            .unwrap();
        }
        let profile = world
            .project_dir()
            .join(".lattice/agents/background/agent.toml");
        let allowed = std::fs::read_to_string(&profile).unwrap();
        let denied = allowed.replace("allowed_hosts = [\"localhost\"]", "allowed_hosts = []");
        let beta = world.root().join("beta");
        std::fs::create_dir_all(beta.join(".lattice/agents/background")).unwrap();
        let beta_profile = beta.join(".lattice/agents/background/agent.toml");
        std::fs::write(&beta_profile, &denied).unwrap();
        std::fs::write(beta.join(".env"), "").unwrap();
        gix::init(&beta).unwrap();
        world.set_env("OPENAI_COMPATIBLE_MODEL", "background");
        world.set_env("OPENAI_COMPATIBLE_API_KEY", FAKE_API_KEY);
        let start = |index: usize, project: &std::path::Path, turn: &str| {
            let mut cmd = world.command(driver().binary_path());
            cmd.current_dir(project)
                .env("OPENAI_COMPATIBLE_BASE_URL", &urls[index])
                .env("SSL_CERT_FILE", roots[index].join("ca.pem"))
                .env("SSL_CERT_DIR", roots[index].join("empty-certs"))
                .args([
                    "--json",
                    "session",
                    "send",
                    "frozen TLS",
                    "--agent",
                    "background",
                    "--turn-id",
                    turn,
                ]);
            RunningCommand::spawn(cmd).unwrap()
        };
        let first = start(0, world.project_dir(), FIRST);
        ga.wait_until_stalled(1, budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        let second = start(1, &beta, SECOND);
        gb.wait_until_stalled(1, budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        assert_eq!(world.daemon_pids().unwrap().len(), 1);
        let original_a = std::fs::read(roots[0].join("ca.pem")).unwrap();
        let original_b = std::fs::read(roots[1].join("ca.pem")).unwrap();
        std::fs::write(roots[0].join("ca.pem"), &original_b).unwrap();
        std::fs::write(roots[1].join("ca.pem"), &original_a).unwrap();
        std::fs::write(&profile, &denied).unwrap();
        std::fs::write(&beta_profile, &allowed).unwrap();
        evidence.record_json("frozen-window", &json!({"both_providers_arrived":true,"daemon_pids":world.daemon_pids().unwrap(),
            "endpoint_a":urls[0],"endpoint_b":urls[1],"root_a_blake3":blake3::hash(&original_a).to_hex().to_string(),
            "root_b_blake3":blake3::hash(&original_b).to_hex().to_string(),"sources_swapped_after_acceptance":true})).unwrap();
        ga.release(budget.cap_timeout(spec.step_timeout())).unwrap();
        gb.release(budget.cap_timeout(spec.step_timeout())).unwrap();
        for (label, front) in [("alpha frozen TLS", first), ("beta frozen TLS", second)] {
            let out = front
                .wait(Some(budget.cap_timeout(spec.step_timeout())))
                .unwrap();
            evidence.record_command(label, &out);
            assert!(out.is_clean_exit(0), "{}", out.summary());
        }
        let ta = detail(&world, &budget, &mut evidence, FIRST);
        let tb = detail(&world, &budget, &mut evidence, SECOND);
        assert_eq!(
            ta["steps"][0]["state"]["slots"][0]["state"]["outcome"],
            "succeeded"
        );
        assert_eq!(
            ta["steps"][0]["state"]["slots"][1]["state"]["outcome"],
            "failed"
        );
        // 跨根请求被策略拒绝：结局是 Failed，错误码属于 Validation/Permission/Guard 错误族。
        let refused = &tb["steps"][0]["state"]["slots"][0]["state"];
        assert_eq!(refused["outcome"], "failed");
        assert!(
            matches!(
                refused["reason"]["value"]["code"].as_str(),
                Some(
                    "schema_mismatch"
                        | "path_containment"
                        | "type_error"
                        | "capability_not_granted"
                        | "project_root_violation"
                        | "ambient_authority_denied"
                        | "guard_denied"
                        | "replan_required"
                )
            ),
            "{refused}"
        );
        let own = &ta["steps"][0]["state"]["slots"][0]["state"]["evidence"]["value"]["effects"]["effects"]
            [0]["network"];
        assert_eq!(own["transport"]["tls"]["state"], "established");
        assert_eq!(own["http_status"], 200);
        let cross = &ta["steps"][0]["state"]["slots"][1]["state"]["evidence"]["value"]["effects"]["effects"]
            [0]["network"];
        assert_eq!(cross["transport"]["tls"]["state"], "failed");
        assert_eq!(cross["failure_phase"], "tls");
        let policy = &tb["steps"][0]["state"]["slots"][0]["state"]["evidence"]["value"]["denied"]["denied"]
            [0]["network"];
        assert_eq!(policy["policy_verdict"], "denied");
        assert_eq!(policy["resolution"], "not_attempted");
        assert_eq!(policy["transport"]["tls"]["state"], "not_attempted");
        assert_eq!(a.captured_requests().len(), 2);
        assert_eq!(b.captured_requests().len(), 2);
        // 换根后的新请求不能借用同后台另一个 Turn 的信任。
        let crossed = start(0, world.project_dir(), THIRD)
            .wait(Some(budget.cap_timeout(spec.step_timeout())))
            .unwrap();
        evidence.record_command("cross-root provider rejected", &crossed);
        assert!(crossed.is_clean_exit(1));
        let failed = closed(&world, &budget, &mut evidence, THIRD);
        assert_eq!(failed["summary"]["outcome"], "failed");
        assert_eq!(
            failed["steps"][0]["attempts"][0]["end"]["cause"]["cause"],
            "provider_error"
        );
        assert_eq!(a.captured_requests().len(), 2);
        let bad_id = "00000000-0000-4000-8000-000000000004";
        let bad = ipc(
            &world,
            &budget,
            submit(
                &world,
                None,
                bad_id,
                "invalid root",
                json!({"profile_toml":allowed,"system_prompt":"You are a concise test agent.","connection":{
                "alias":"compat:background","provider":"openai_compatible","provider_model":"background",
                "base_url":urls[0],"api_key":FAKE_API_KEY,"roots_der":[[1,2,3]]}}),
            ),
        );
        assert_eq!(bad["value"]["kind"], "bad_request");
        let missing = ipc(
            &world,
            &budget,
            json!({"operation":"query","session_id":null,"turn_id":bad_id}),
        );
        assert_eq!(missing["value"]["kind"], "not_found");
        evidence
            .record_json(
                "invalid-root-before-acceptance",
                &json!({"submit":bad,"query":missing}),
            )
            .unwrap();
        archive_state(&world, &budget, &mut evidence, &[FIRST, THIRD]);
        let beta_sid = tb["session_id"].as_str().unwrap();
        for args in [
            vec!["--json", "audit", "--tail"],
            vec!["session", "log", beta_sid],
            vec!["--json", "session", "log", beta_sid],
            vec!["--json", "session", "turn", SECOND],
            vec![
                "--json",
                "audit",
                "replay",
                "--session",
                beta_sid,
                "--turn",
                SECOND,
            ],
            vec!["--json", "lancet", "status"],
        ] {
            let mut cmd = world.command(driver().binary_path());
            cmd.current_dir(&beta).args(&args);
            let out = RunningCommand::spawn(cmd)
                .unwrap()
                .wait(Some(budget.cap_timeout(spec.step_timeout())))
                .unwrap();
            evidence.record_command(format!("beta {}", args.join(" ")), &out);
            assert!(out.is_clean_exit(0));
        }
        evidence.record_json("parameters", &json!({"test":"frozen_tls_roots_and_policies_are_isolated_between_concurrent_turns",
            "tls":"two ephemeral CAs; localhost leaves; source roots/policy swapped after both provider gates",
            "tools_a":["own-a","cross-from-a"],"tools_b":["denied-b"],"turns":[ta,tb,failed]})).unwrap();
        for root in &roots {
            std::fs::write(root.join("stop"), b"").unwrap();
        }
        for (index, proxy) in proxies.into_iter().enumerate() {
            let out = proxy
                .wait(Some(budget.cap_timeout(spec.step_timeout())))
                .unwrap();
            evidence.record_command(format!("TLS proxy {index} shutdown"), &out);
            assert!(out.is_clean_exit(0));
            let stats: Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(stats["errors"], json!([]));
            assert_eq!(stats["posts"], 2);
            assert_eq!(
                stats["get_paths"],
                if index == 0 {
                    json!(["/own-a"])
                } else {
                    json!([])
                }
            );
            assert!(stats["rejected_handshakes"].as_u64().unwrap() >= 1);
            evidence
                .record_json(&format!("tls-stats-{index}"), &stats)
                .unwrap();
        }
        evidence.record_captures(a.captured_requests());
        evidence.record_captures(b.captured_requests());
    }));
    support::finish_recorded(
        evidence,
        result,
        "direct_background",
        "frozen_tls_roots_and_policies_are_isolated_between_concurrent_turns",
    );
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
        let project = world.root().join("different-project");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(project.join(".env"), "").unwrap();
        gix::init(&project).unwrap();
        let mut project_conflicts = Vec::new();
        for sid in [None, a["session_id"].as_str()] {
            let mut request = submit(&world, sid, FIRST, "alpha", Value::Null);
            request["request"]["workspace"] = json!(project);
            let response = ipc(&world, &budget, request);
            assert_eq!(response["value"]["kind"], "conflict");
            project_conflicts.push(response);
        }
        evidence
            .record_json(
                "identity-conflicts",
                &json!({"session":conflict,"project":project_conflicts,"configuration_sent":false}),
            )
            .unwrap();
        archive_state(&world, &budget, &mut evidence, &[FIRST, SECOND]);
        evidence.record_json("parameters", &json!({"turns":[FIRST,SECOND],"projects":2,"script":"two todo/final Turns; no-profile cross-project retries"})).unwrap();
        assert_eq!(detail(&world, &budget, &mut evidence, FIRST), a);
        assert_eq!(detail(&world, &budget, &mut evidence, SECOND), b);
        assert_eq!(server.captured_requests().len(), 4);
        let inv_a = &a["steps"][0]["state"]["slots"][0]["invocation_id"];
        let inv_b = &b["steps"][0]["state"]["slots"][0]["invocation_id"];
        assert_ne!(inv_a, inv_b);
        evidence.record_captures(server.captured_requests());
    }));
    support::finish_recorded(
        evidence,
        result,
        "direct_background",
        "global_turn_identity_rejects_a_different_session_without_dispatch",
    );
}
