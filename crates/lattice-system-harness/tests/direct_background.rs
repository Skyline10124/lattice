// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

mod support;

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use lattice_system_harness::{
    CommandOutcome, FAKE_API_KEY, LatticeCommand, RunningCommand, ScenarioBudget, ScenarioEvidence,
    ScenarioWorld, SessionSendDriver, SseScript,
};
use serde_json::{Value, json};
use support::{assert_forced_kill, direct_world, driver, wire_gated_fake_llm};

const FIRST: &str = "00000000-0000-4000-8000-000000000001";
const SECOND: &str = "00000000-0000-4000-8000-000000000002";

fn command(world: &ScenarioWorld, budget: &ScenarioBudget, args: &[&str]) -> RunningCommand {
    driver()
        .start(
            world,
            &LatticeCommand {
                args: args.iter().map(|arg| (*arg).into()).collect(),
                timeout: Some(budget.cap_timeout(Duration::from_secs(30))),
            },
        )
        .expect("start product command")
}

fn run(
    world: &ScenarioWorld,
    budget: &ScenarioBudget,
    evidence: &mut ScenarioEvidence,
    args: &[&str],
) -> CommandOutcome {
    let outcome = command(world, budget, args)
        .wait(None)
        .expect("wait product command");
    evidence.record_command(args.join(" "), &outcome);
    outcome
}

fn lines(outcome: &CommandOutcome) -> Vec<Value> {
    assert!(
        outcome.stderr.is_empty(),
        "JSON command stderr: {}",
        outcome.summary()
    );
    std::str::from_utf8(&outcome.stdout)
        .expect("UTF-8 JSONL")
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).expect("valid JSONL"))
        .collect()
}

fn query(
    world: &ScenarioWorld,
    budget: &ScenarioBudget,
    evidence: &mut ScenarioEvidence,
    session: &str,
    turn: &str,
) -> Vec<Value> {
    let outcome = run(
        world,
        budget,
        evidence,
        &["--json", "session", "turn", session, turn],
    );
    assert!(outcome.is_clean_exit(0), "query: {}", outcome.summary());
    lines(&outcome)
}

fn wait_committed(
    world: &ScenarioWorld,
    budget: &ScenarioBudget,
    evidence: &mut ScenarioEvidence,
    session: &str,
    turn: &str,
) -> Vec<Value> {
    let deadline = Instant::now() + budget.cap_timeout(Duration::from_secs(30));
    loop {
        let records = query(world, budget, evidence, session, turn);
        if records[0]["view"]["requested_turn"]["state"] == "committed" {
            return records;
        }
        assert!(Instant::now() < deadline, "Turn did not durably close");
        std::thread::sleep(Duration::from_millis(10));
    }
}

// 独立编码公开 Local IPC，不依赖产品类型或读取私有 Git 对象。
fn write_frame(world: &ScenarioWorld, budget: &ScenarioBudget, request: &Value) -> UnixStream {
    let mut stream =
        UnixStream::connect(world.session_store_dir().join("local/api.sock")).expect("local IPC");
    let timeout = budget.cap_timeout(Duration::from_secs(30));
    stream.set_read_timeout(Some(timeout)).unwrap();
    stream.set_write_timeout(Some(timeout)).unwrap();
    let bytes = serde_json::to_vec(request).unwrap();
    stream.write_all(b"LTLOCAL2").unwrap();
    stream
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .unwrap();
    stream.write_all(&bytes).unwrap();
    stream
}

fn read_frame(stream: &mut UnixStream) -> Value {
    let mut header = [0_u8; 12];
    stream.read_exact(&mut header).expect("read local frame");
    assert_eq!(&header[..8], b"LTLOCAL2");
    let length = u32::from_be_bytes(header[8..].try_into().unwrap()) as usize;
    assert!(length <= 16 * 1024 * 1024);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).expect("read local body");
    serde_json::from_slice(&body).expect("local response JSON")
}

fn ipc(world: &ScenarioWorld, budget: &ScenarioBudget, request: Value) -> Value {
    read_frame(&mut write_frame(world, budget, &request))
}

fn submit(
    world: &ScenarioWorld,
    session: &str,
    turn: &str,
    input: &str,
    configuration: Value,
) -> Value {
    json!({"operation":"submit", "request":{
        "session_id":session, "turn_id":turn, "workspace":world.project_dir(), "input":input
    }, "configuration":configuration})
}

fn finish(evidence: ScenarioEvidence, result: std::thread::Result<()>) {
    let primary = result.as_ref().err().map(|error| {
        error
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| error.downcast_ref::<&str>().map(|text| text.to_string()))
            .unwrap_or_else(|| "scenario panicked".into())
    });
    evidence
        .finish_report(primary)
        .unwrap_or_else(|report| panic!("{report}"));
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
        let driver = driver();
        let session = SessionSendDriver::new(&driver, &world, &spec, "background", "main").unwrap();
        let sid = session.session_id();
        let front = command(
            &world,
            &budget,
            &[
                "--json",
                "session",
                "send",
                sid,
                "keep running",
                "--turn-id",
                FIRST,
            ],
        );
        gate.wait_until_stalled(1, budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        // 独立确认路径找回相同请求；不是凭 provider 到达推定持久接受。
        let accepted = ipc(
            &world,
            &budget,
            submit(&world, sid, FIRST, "keep running", Value::Null),
        );
        assert_eq!(accepted["response"], "acceptance");
        assert_eq!(
            accepted["value"],
            json!({"acceptance":"accepted", "turn_id":FIRST})
        );
        let pids = world.daemon_pids().unwrap();
        assert_eq!(pids.len(), 1);
        assert_ne!(pids[0], front.pgid());
        let killed = front.kill_and_wait().unwrap();
        evidence.record_command("close CLI after acceptance", &killed);
        assert_forced_kill(&killed);
        assert_eq!(world.daemon_pids().unwrap(), pids);
        let pending = query(&world, &budget, &mut evidence, sid, FIRST);
        assert_eq!(pending[0]["view"]["requested_turn"]["state"], "pending");
        assert!(
            pending.iter().any(|record| record["type"] == "transcript"
                && record.to_string().contains("keep running"))
        );
        gate.release(budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        let committed = wait_committed(&world, &budget, &mut evidence, sid, FIRST);
        assert_eq!(
            committed[0]["view"]["requested_turn"]["outcome"],
            json!({"completed":{"finish_reason":"stop"}})
        );
        assert!(
            committed
                .iter()
                .any(|record| record.to_string().contains("detached durable answer"))
        );
        assert_eq!(server.captured_requests().len(), 1);
        evidence.record_captures(server.captured_requests());
        world.stop_daemon().expect("normal daemon cleanup");
        assert!(world.daemon_pids().unwrap().is_empty());
        assert!(pids.into_iter().all(|pid| {
            lattice_system_harness::processes_in_pgid(pid)
                .unwrap()
                .is_empty()
        }));
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
        let driver = driver();
        let session = SessionSendDriver::new(&driver, &world, &spec, "background", "main").unwrap();
        let sid = session.session_id();
        let configuration = json!({
            "profile_toml":std::fs::read_to_string(world.project_dir().join(".lattice/agents/background/agent.toml")).unwrap(),
            "system_prompt":"You are a concise test agent.",
            "connection":{"alias":"compat:background", "provider":"openai_compatible", "provider_model":"background",
                "base_url":server.base_url(), "api_key":FAKE_API_KEY, "roots_der":[]}
        });
        // 此连接只写不读：接受回执即使已经进内核缓冲，也没有被客户端消费。
        let unread = write_frame(
            &world,
            &budget,
            &submit(&world, sid, FIRST, "identical input", configuration),
        );
        gate.wait_until_stalled(1, budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        assert_eq!(
            query(&world, &budget, &mut evidence, sid, FIRST)[0]["view"]["requested_turn"]["state"],
            "pending"
        );
        drop(unread);
        let retry = ipc(
            &world,
            &budget,
            submit(&world, sid, FIRST, "identical input", Value::Null),
        );
        assert_eq!(
            retry["value"],
            json!({"acceptance":"accepted", "turn_id":FIRST})
        );
        let conflict = ipc(
            &world,
            &budget,
            submit(&world, sid, FIRST, "different input", Value::Null),
        );
        assert_eq!(conflict["value"]["kind"], "conflict");
        gate.release(budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        let original = wait_committed(&world, &budget, &mut evidence, sid, FIRST);
        let commit = original[0]["view"]["requested_turn"]["commit_id"].clone();
        world.stop_daemon().unwrap();
        // 找回旧请求不能要求当前 profile 或凭据；重启后的 pin 仍是原身份。
        world.unset_env("OPENAI_COMPATIBLE_API_KEY");
        let profile = world
            .project_dir()
            .join(".lattice/agents/background/agent.toml");
        let original_profile = std::fs::read_to_string(&profile).unwrap();
        std::fs::remove_file(&profile).unwrap();
        let repeat = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "run", sid, "identical input", "--turn-id", FIRST],
        );
        assert!(repeat.is_clean_exit(0), "retry: {}", repeat.summary());
        assert_eq!(
            query(&world, &budget, &mut evidence, sid, FIRST)[0]["view"]["requested_turn"]["commit_id"],
            commit
        );
        assert_eq!(server.captured_requests().len(), 1);
        std::fs::write(profile, original_profile).unwrap();
        world.set_env("OPENAI_COMPATIBLE_API_KEY", FAKE_API_KEY);
        let independent = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "run", sid, "identical input", "--turn-id", SECOND],
        );
        assert!(
            independent.is_clean_exit(0),
            "new ID: {}",
            independent.summary()
        );
        assert_eq!(server.captured_requests().len(), 2);
        assert_eq!(
            query(&world, &budget, &mut evidence, sid, SECOND)[0]["view"]["commit_count"],
            2
        );
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
        let driver = driver();
        let a = SessionSendDriver::new(&driver, &world, &spec, "background", "main").unwrap();
        let b = SessionSendDriver::new(&driver, &world, &spec, "background", "main").unwrap();
        let first = command(
            &world,
            &budget,
            &[
                "--json",
                "run",
                a.session_id(),
                "parallel a",
                "--turn-id",
                FIRST,
            ],
        );
        gate.wait_until_stalled(1, budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        let second = command(
            &world,
            &budget,
            &[
                "--json",
                "run",
                b.session_id(),
                "parallel b",
                "--turn-id",
                SECOND,
            ],
        );
        gate.wait_until_stalled(2, budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        let busy = run(
            &world,
            &budget,
            &mut evidence,
            &[
                "--json",
                "run",
                a.session_id(),
                "must not queue",
                "--turn-id",
                SECOND,
            ],
        );
        assert!(busy.is_clean_exit(1), "Busy: {}", busy.summary());
        assert!(lines(&busy).iter().any(|record| record["kind"] == "busy"));
        assert_eq!(
            query(&world, &budget, &mut evidence, a.session_id(), FIRST)[0]["view"]["requested_turn"]
                ["state"],
            "pending"
        );
        let cancel = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "cancel", a.session_id(), FIRST],
        );
        assert!(cancel.is_clean_exit(0), "cancel: {}", cancel.summary());
        assert_eq!(
            lines(&cancel)[0],
            json!({"type":"cancel_acknowledged", "owned":true, "session_id":a.session_id(), "turn_id":FIRST})
        );
        let closed = first.kill_and_wait().unwrap();
        evidence.record_command("exit frontend after cancel acknowledgement", &closed);
        assert_forced_kill(&closed);
        let cancelled = wait_committed(&world, &budget, &mut evidence, a.session_id(), FIRST);
        assert!(
            cancelled[0]["view"]["requested_turn"]["outcome"]
                .get("cancelled")
                .is_some()
        );
        let stop = command(&world, &budget, &["--json", "daemon", "stop"]);
        let deadline = Instant::now() + budget.cap_timeout(Duration::from_secs(30));
        loop {
            let status = ipc(&world, &budget, json!({"operation":"status"}));
            if status["value"]["stopping"] == true {
                break;
            }
            assert!(Instant::now() < deadline, "stop did not close admission");
        }
        let rejected = ipc(
            &world,
            &budget,
            submit(&world, a.session_id(), SECOND, "after stop", Value::Null),
        );
        assert_eq!(rejected["value"]["kind"], "stopping");
        assert_eq!(world.daemon_pids().unwrap().len(), 1);
        assert_eq!(
            query(&world, &budget, &mut evidence, b.session_id(), SECOND)[0]["view"]["requested_turn"]
                ["state"],
            "pending"
        );
        gate.release(budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        let done = second.wait(None).unwrap();
        evidence.record_command("parallel b completion", &done);
        let done_records = lines(&done);
        if done.is_clean_exit(0) {
            assert!(
                done_records
                    .iter()
                    .any(|record| record["type"] == "transcript"
                        && record.to_string().contains("parallel answer")),
                "exit 0 must include the fixed result body"
            );
        } else {
            assert!(done.is_clean_exit(1), "second: {}", done.summary());
            assert!(
                done_records.iter().any(|record| record["type"] == "error"),
                "body read failure must remain an explicit command error"
            );
            assert!(
                done_records
                    .iter()
                    .any(|record| record["type"] == "snapshot"
                        && record["view"]["requested_turn"]["outcome"]
                            .get("completed")
                            .is_some()),
                "read failure must preserve completed execution facts"
            );
        }
        let drained = stop.wait(None).unwrap();
        evidence.record_command("stop drains live owner", &drained);
        assert!(drained.is_clean_exit(0), "stop: {}", drained.summary());
        assert_eq!(lines(&drained)[0]["owners"], 0);
        world.stop_daemon().unwrap();
        let reloaded = query(&world, &budget, &mut evidence, b.session_id(), SECOND);
        assert!(
            reloaded.iter().any(|record| record["type"] == "transcript"
                && record.to_string().contains("parallel answer")),
            "stopped backend result must reload from the same Turn"
        );
        assert_eq!(reloaded[0]["view"]["commit_count"], 1);
        assert_eq!(server.captured_requests().len(), 2);
        evidence.record_captures(server.captured_requests());
    }));
    finish(evidence, result);
}

#[test]
fn concurrent_autostart_has_one_owner_and_gap_reloads_result() {
    let (world, spec) = direct_world("background-start-gap", "background", "[]", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let a = command(
            &world,
            &budget,
            &[
                "--json",
                "session",
                "new",
                "--agent",
                "background",
                "--project-branch",
                "main",
            ],
        );
        let b = command(
            &world,
            &budget,
            &[
                "--json",
                "session",
                "new",
                "--agent",
                "background",
                "--project-branch",
                "main",
            ],
        );
        let a = a.wait(None).unwrap();
        let b = b.wait(None).unwrap();
        evidence.record_command("concurrent create A", &a);
        evidence.record_command("concurrent create B", &b);
        assert!(
            a.is_clean_exit(0) && b.is_clean_exit(0),
            "create: A={} B={}",
            a.summary(),
            b.summary()
        );
        assert_eq!(world.daemon_pids().unwrap().len(), 1);
        let created: Value = serde_json::from_slice(&a.stdout).unwrap();
        let sid = created["session_id"].as_str().unwrap().to_string();
        let (server, gate) =
            wire_gated_fake_llm(&world, "background", SseScript::plain("大".repeat(24_000)));
        let front = command(
            &world,
            &budget,
            &["--json", "run", &sid, "gap input", "--turn-id", FIRST],
        );
        gate.wait_until_stalled(1, budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        let mut observer = write_frame(
            &world,
            &budget,
            &json!({"operation":"subscribe", "session_id":sid, "turn_id":FIRST,
            "cursor":{"epoch":"previous-backend", "sequence":1}}),
        );
        assert_eq!(read_frame(&mut observer)["value"]["type"], "gap");
        let snapshot = read_frame(&mut observer);
        assert_eq!(
            snapshot["value"]["view"]["requested_turn"]["state"],
            "pending"
        );
        // 观察者放弃读取不会反压持久收尾；超大单事件通过 Gap 而非伪造完整增量。
        drop(observer);
        gate.release(budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        let done = front.wait(None).unwrap();
        evidence.record_command("oversized delta with fixed reload", &done);
        assert!(done.is_clean_exit(0), "Gap completion: {}", done.summary());
        let records = lines(&done);
        assert!(records.iter().any(|record| record["type"] == "gap"));
        let reloaded = query(&world, &budget, &mut evidence, &sid, FIRST);
        assert!(reloaded.iter().any(|record| record["type"] == "transcript"
            && record.to_string().contains(&"大".repeat(24_000))));
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
                support::tool_call(
                    "todo",
                    "session.todo_write",
                    r#"{"todos":[{"content":"isolation","status":"pending"}]}"#,
                ),
                SseScript::plain("frozen result"),
            ],
        };
        let (a, gate_a) = FakeLlmServer::start_gated(script.clone()).unwrap();
        let (b, gate_b) = FakeLlmServer::start_gated(script).unwrap();
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
        let second_project = world.root().join("second-project");
        std::fs::create_dir_all(second_project.join(".lattice/agents/background")).unwrap();
        let first_profile = world
            .project_dir()
            .join(".lattice/agents/background/agent.toml");
        let second_profile = std::fs::read_to_string(&first_profile)
            .unwrap()
            .replace("concise test agent", "beta test agent");
        std::fs::write(
            second_project.join(".lattice/agents/background/agent.toml"),
            second_profile,
        )
        .unwrap();
        std::fs::write(
            second_project.join(".env"),
            format!(
                "OPENAI_COMPATIBLE_API_KEY={key_b}\nOPENAI_COMPATIBLE_BASE_URL={}\n",
                a.base_url()
            ),
        )
        .unwrap();
        gix::init(&second_project).unwrap();
        let driver = driver();
        let first_session =
            SessionSendDriver::new(&driver, &world, &spec, "background", "main").unwrap();
        let mut create = world.command(driver.binary_path());
        create
            .current_dir(&second_project)
            .env("OPENAI_COMPATIBLE_BASE_URL", b.base_url())
            .args([
                "--json",
                "session",
                "new",
                "--agent",
                "background",
                "--project-branch",
                "main",
            ]);
        let created = RunningCommand::spawn(create)
            .unwrap()
            .wait(Some(budget.cap_timeout(Duration::from_secs(30))))
            .unwrap();
        evidence.record_command("create second project Session", &created);
        assert!(created.is_clean_exit(0));
        let created: Value = serde_json::from_slice(&created.stdout).unwrap();
        let second_session = created["session_id"].as_str().unwrap();
        let first = command(
            &world,
            &budget,
            &[
                "--json",
                "run",
                first_session.session_id(),
                "alpha",
                "--turn-id",
                FIRST,
            ],
        );
        gate_a
            .wait_until_stalled(1, budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        let mut second = world.command(driver.binary_path());
        second
            .current_dir(&second_project)
            .env("OPENAI_COMPATIBLE_BASE_URL", b.base_url())
            .args(["--json", "run", second_session, "beta", "--turn-id", SECOND]);
        let second = RunningCommand::spawn(second).unwrap();
        gate_b
            .wait_until_stalled(1, budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        assert_eq!(world.daemon_pids().unwrap().len(), 1);
        std::fs::write(first_profile, "invalid changed profile").unwrap();
        world
            .write_project_file(".env", &format!("OPENAI_COMPATIBLE_API_KEY={key_b}\n"))
            .unwrap();
        gate_a
            .release(budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        gate_b
            .release(budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        let first = first.wait(None).unwrap();
        let second = second
            .wait(Some(budget.cap_timeout(Duration::from_secs(30))))
            .unwrap();
        evidence.record_command("alpha frozen execution", &first);
        evidence.record_command("beta frozen execution", &second);
        assert!(
            first.is_clean_exit(0) && second.is_clean_exit(0),
            "alpha={} beta={}",
            first.summary(),
            second.summary()
        );
        for output in [&first, &second] {
            assert_no_secret(output, key_a).unwrap();
            assert_no_secret(output, key_b).unwrap();
        }
        let captures_a = a.captured_requests();
        let captures_b = b.captured_requests();
        assert_eq!(captures_a.len(), 2);
        assert_eq!(captures_b.len(), 2);
        for capture in &captures_a {
            assert_eq!(
                capture.raw["messages"][0]["content"],
                "You are a concise test agent."
            );
        }
        for capture in &captures_b {
            assert_eq!(
                capture.raw["messages"][0]["content"],
                "You are a beta test agent."
            );
        }
        let mut captures = captures_a;
        captures.extend(captures_b);
        evidence.record_captures(captures);
    }));
    finish(evidence, result);
}

#[test]
fn slow_subscriber_cannot_block_fixed_result_or_other_queries() {
    let (world, spec) = direct_world("background-slow-subscriber", "background", "[]", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // 总正文在2MiB内，但远超Unix socket和观察队列；不是单事件超限的Gap场景。
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
        let driver = driver();
        let session = SessionSendDriver::new(&driver, &world, &spec, "background", "main").unwrap();
        let front = command(
            &world,
            &budget,
            &[
                "--json",
                "run",
                session.session_id(),
                "slow observer",
                "--turn-id",
                FIRST,
            ],
        );
        gate.wait_until_stalled(1, budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        let mut slow = write_frame(
            &world,
            &budget,
            &json!({"operation":"subscribe", "session_id":session.session_id(), "turn_id":FIRST, "cursor":null}),
        );
        loop {
            let response = read_frame(&mut slow);
            if response["value"]["type"] == "snapshot" {
                break;
            }
            assert_eq!(response["value"]["type"], "gap");
        }
        gate.release(budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        // 在slow完全不读取期间，由另一个客户端取到最终正文，验证执行和控制面不被它反压。
        let done = front.wait(None).unwrap();
        evidence.record_command("normal subscriber beside stalled socket", &done);
        assert!(done.is_clean_exit(0), "{}", done.summary());
        let fixed = wait_committed(&world, &budget, &mut evidence, session.session_id(), FIRST);
        assert!(
            fixed
                .iter()
                .any(|record| record["type"] == "transcript" && record.to_string().contains(&text))
        );
        assert!(lines(&done).iter().any(|record| record["type"] == "gap"));
        drop(slow);
        assert_eq!(server.captured_requests().len(), 1);
        evidence.record_captures(server.captured_requests());
    }));
    finish(evidence, result);
}

/// 场景预算只有一个起点：较晚创建的发送驱动必须承接之前步骤已消耗的时间，
/// 而不是从自己的创建时刻重新计时。
#[test]
fn later_session_driver_keeps_the_scenario_budget_origin() {
    let (world, spec) = direct_world("background-budget-origin", "background", "[]", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // 先用一步真实命令消耗预算，再创建驱动。
        let help = run(&world, &budget, &mut evidence, &["--help"]);
        assert!(help.is_clean_exit(0), "help: {}", help.summary());
        let consumed = budget.elapsed();
        let creating = Instant::now();
        let driver = driver();
        let session = SessionSendDriver::new(&driver, &world, &spec, "background", "main").unwrap();
        let creation = creating.elapsed();
        let seen = session.budget().elapsed();
        assert!(
            seen >= consumed + creation,
            "driver budget restarted at creation: driver elapsed {seen:?} < scenario {consumed:?} + creation {creation:?}"
        );
        assert_eq!(
            session.budget().remaining().is_some(),
            budget.remaining().is_some()
        );
        world
            .stop_daemon()
            .expect("normal daemon cleanup within the scenario budget");
    }));
    finish(evidence, result);
}

/// 预算耗尽后的正常收尾必须记录失败并立刻转入有界的强制清理，不能再按固定 30 秒等待 stop。
#[test]
fn exhausted_budget_records_normal_stop_failure_without_fixed_wait() {
    let (world, mut spec) = direct_world("background-budget-exhausted", "background", "[]", "");
    spec.total_timeout_secs = Some(0);
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_eq!(budget.remaining(), Some(Duration::ZERO));
        // 显式启动自有后台只为制造需要收尾的进程组；这里用 step 超时，不用已耗尽的预算。
        let state_root = world
            .session_store_dir()
            .parent()
            .unwrap()
            .display()
            .to_string();
        let daemon = driver()
            .start(
                &world,
                &LatticeCommand {
                    args: ["daemon", "serve", "--state-root", &state_root]
                        .iter()
                        .map(|arg| (*arg).into())
                        .collect(),
                    timeout: Some(spec.step_timeout()),
                },
            )
            .unwrap();
        let deadline = Instant::now() + spec.step_timeout();
        while world.daemon_pids().unwrap() != vec![daemon.pgid()] {
            assert!(Instant::now() < deadline, "backend did not register");
            std::thread::sleep(Duration::from_millis(10));
        }
        let stopping = Instant::now();
        let stopped = world.stop_daemon();
        let took = stopping.elapsed();
        let errors = stopped.expect_err("exhausted budget must fail the normal stop");
        assert!(
            errors.iter().any(|error| error.contains("budget")),
            "cleanup errors must name the exhausted budget: {errors:?}"
        );
        assert!(took < Duration::from_secs(10), "cleanup waited {took:?}");
        assert!(world.daemon_pids().unwrap().is_empty());
        let killed = daemon.wait(None).unwrap();
        evidence.record_command("backend after exhausted-budget cleanup", &killed);
        assert!(
            killed.cleanup.surviving_pids.is_empty(),
            "{}",
            killed.summary()
        );
    }));
    finish(evidence, result);
}
