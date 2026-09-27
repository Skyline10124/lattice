// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 真实后台提交后 SIGSTOP/SIGKILL；只用 CLI/公开 IPC 读取会话事实。
mod support;

use lattice_system_harness::{
    CommandOutcome, FAKE_API_KEY, FakeLlmServer, LatticeCommand, ScenarioEvidence, ScenarioSpec,
    ScenarioWorld, SseScript,
};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::{Duration, Instant},
};

const TURN: &str = "00000000-0000-4000-8000-000000000001";
const OTHER: &str = "00000000-0000-4000-8000-000000000002";
const BARRIER: &str = "LATTICE_TEST_TURN_BARRIER";
const ACK: &str = "LATTICE_TEST_TURN_BARRIER_ACK";

// 每条连接独立重组 v3 ResponseFrame，不把半帧当成领域响应。
struct Wire {
    stream: UnixStream,
    message: u64,
}
impl Wire {
    fn connect(world: &ScenarioWorld, request: Value) -> Self {
        let mut stream =
            UnixStream::connect(world.session_store_dir().join("local/api.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let body = serde_json::to_vec(&request).unwrap();
        stream.write_all(b"LTLOCAL3").unwrap();
        stream
            .write_all(&(body.len() as u32).to_be_bytes())
            .unwrap();
        stream.write_all(&body).unwrap();
        Self { stream, message: 0 }
    }
    fn next(&mut self) -> Value {
        let mut logical = String::new();
        loop {
            let mut header = [0; 12];
            self.stream.read_exact(&mut header).unwrap();
            assert_eq!(&header[..8], b"LTLOCAL3");
            let len = u32::from_be_bytes(header[8..].try_into().unwrap()) as usize;
            assert!((1..=16 * 1024 * 1024).contains(&len));
            let mut bytes = vec![0; len];
            self.stream.read_exact(&mut bytes).unwrap();
            let frame: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(frame["message_id"], self.message);
            assert_eq!(frame["offset"], logical.len());
            logical.push_str(frame["json_fragment"].as_str().unwrap());
            if frame["complete"].as_bool().unwrap() {
                self.message += 1;
                return serde_json::from_str(&logical).unwrap();
            }
        }
    }
}
fn call(world: &ScenarioWorld, request: Value) -> Value {
    Wire::connect(world, request).next()
}
fn query(world: &ScenarioWorld, tid: &str) -> Value {
    let response = call(
        world,
        json!({"operation":"query","session_id":null,"turn_id":tid}),
    );
    assert_eq!(response["response"], "view", "{response}");
    response["value"].clone()
}
fn cli(
    world: &ScenarioWorld,
    spec: &ScenarioSpec,
    evidence: &mut ScenarioEvidence,
    args: &[&str],
    code: i32,
) -> CommandOutcome {
    let result = support::driver()
        .run(
            world,
            &LatticeCommand {
                args: args.iter().map(|v| (*v).into()).collect(),
                timeout: Some(world.budget(spec).cap_timeout(spec.step_timeout())),
            },
        )
        .unwrap();
    evidence.record_command(args.join(" "), &result);
    assert!(
        result.is_clean_exit(code),
        "{}: {}",
        args.join(" "),
        result.summary()
    );
    result
}
fn read_json(
    world: &ScenarioWorld,
    spec: &ScenarioSpec,
    evidence: &mut ScenarioEvidence,
    args: &[&str],
) -> Value {
    serde_json::from_slice(&cli(world, spec, evidence, args, 0).stdout).unwrap()
}
fn wait_for_ack(path: &Path, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(bytes) = std::fs::read(path)
            && let Ok(value) = serde_json::from_slice(&bytes)
        {
            return value;
        }
        assert!(Instant::now() < deadline, "barrier marker not reached");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn wait_view(world: &ScenarioWorld, tid: &str, predicate: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let view = query(world, tid);
        if predicate(&view) {
            return view;
        }
        assert!(
            Instant::now() < deadline,
            "expected durable/observation cut not reached: {view}"
        );
        std::thread::yield_now();
    }
}
fn history_tip(world: &ScenarioWorld) -> String {
    let paths: Vec<_> = std::fs::read_dir(world.session_store_dir().join("projects"))
        .unwrap()
        .map(|v| v.unwrap().path().join("workspace-history.git"))
        .filter(|p| p.is_dir())
        .collect();
    assert_eq!(paths.len(), 1);
    let repo = gix::open_opts(&paths[0], gix::open::Options::isolated()).unwrap();
    repo.find_reference("refs/lattice/workspace-history/current")
        .unwrap()
        .target()
        .try_id()
        .unwrap()
        .to_string()
}
fn chunk(delta: Value) -> Value {
    json!({"choices":[{"index":0,"delta":delta,"finish_reason":null}]})
}
fn calls(prefix: &str, path: &str) -> Value {
    json!({"tool_calls":[
        {"index":0,"id":format!("{prefix}-read"),"type":"function","function":{"name":"observe__workspace","arguments":"{\"mode\":\"read_file\",\"path\":\"seed.txt\"}"}},
        {"index":1,"id":format!("{prefix}-write"),"type":"function","function":{"name":"mutate__create","arguments":json!({"path":path,"content":"durable\n","create_parents":false}).to_string()}},
        {"index":2,"id":format!("{prefix}-tail"),"type":"function","function":{"name":"observe__workspace","arguments":"{\"mode\":\"read_file\",\"path\":\"seed.txt\"}"}}
    ]})
}
fn end() -> Value {
    json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}})
}
fn segments(reply: &Value) -> Vec<Value> {
    let mut result: Vec<Value> = vec![];
    for part in reply["parts"].as_array().unwrap() {
        let (kind, text) =
            if let Some(text) = part.pointer("/semantic/text/text").and_then(Value::as_str) {
                ("text", text)
            } else if let Some(text) = part
                .pointer("/semantic/reasoning/text")
                .and_then(Value::as_str)
            {
                ("reasoning", text)
            } else {
                continue;
            };
        if let Some(last) = result.last_mut()
            && last["type"] == kind
        {
            let mut joined = last["text"].as_str().unwrap().to_owned();
            joined.push_str(text);
            last["text"] = joined.into();
        } else {
            result.push(json!({"type":kind,"text":text}));
        }
    }
    result
}
fn finish(evidence: ScenarioEvidence, result: std::thread::Result<()>, name: &str) {
    if result.is_err() {
        support::crash::finish(evidence, result);
        return;
    }
    let base = std::env::var_os("LATTICE_HARNESS_SUCCESS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target/harness-success"));
    std::fs::create_dir_all(&base).unwrap();
    let run = tempfile::Builder::new()
        .prefix(&format!("{name}-"))
        .tempdir_in(base)
        .unwrap()
        .keep();
    let target = run.join("evidence");
    evidence.finish_success(&target).unwrap();
    eprintln!("SUCCESS_EVIDENCE={}", target.display());
}
fn provenance(world: &ScenarioWorld, evidence: &mut ScenarioEvidence, script: Value) {
    let driver = support::driver();
    let bytes = std::fs::read(driver.binary_path()).unwrap();
    evidence.record_json("provenance", &json!({"binary":driver.binary_path(),"binary_blake3":blake3::hash(&bytes).to_hex().to_string(),"features":"test-crash-barriers","profile":"debug","state_root":world.session_store_dir().parent(),"script":script,"reproduce":"CARGO_BUILD_JOBS=8 cargo build -p lattice-cli --bin lattice --features test-crash-barriers && LATTICE_BIN=$(pwd)/target/debug/lattice CARGO_BUILD_JOBS=8 cargo test -p lattice-system-harness --features crash-barriers --test direct_crash_barriers r2_s2_multicall_reconnect_busy_sigkill_auto_close -- --exact --nocapture"})).unwrap();
}
fn stage(
    world: &ScenarioWorld,
    spec: &ScenarioSpec,
    evidence: &mut ScenarioEvidence,
    sid: &str,
    tid: &str,
) -> Value {
    read_json(world, spec, evidence, &["--json", "session", "list"]);
    read_json(world, spec, evidence, &["--json", "session", "log", sid]);
    cli(world, spec, evidence, &["session", "log", sid], 0);
    let turn = read_json(world, spec, evidence, &["--json", "session", "turn", tid]);
    read_json(
        world,
        spec,
        evidence,
        &["--json", "audit", "--session", sid, "--turn", tid],
    );
    read_json(
        world,
        spec,
        evidence,
        &["--json", "audit", "--tail", "--since", "0"],
    );
    read_json(
        world,
        spec,
        evidence,
        &["--json", "audit", "replay", "--session", sid, "--turn", tid],
    );
    read_json(world, spec, evidence, &["--json", "lancet", "status"]);
    turn
}

#[test]
fn r2_s2_multicall_reconnect_busy_sigkill_auto_close() {
    let (world, spec) = support::direct_world(
        "r2-s2-vertical",
        "vertical",
        "[\"observe.workspace\",\"mutate.create\"]",
        "",
    );
    world.write_project_file("seed.txt", "seed\n").unwrap();
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let first = vec![
            chunk(json!({"content":"first text"})),
            chunk(json!({"reasoning_content":"first reason"})),
            chunk(calls("first", "created.txt")),
            end(),
        ];
        let mut second: Vec<_> = (0..70)
            .map(|i| {
                chunk(if i % 2 == 0 {
                    json!({"content":format!("t{i:02};")})
                } else {
                    json!({"reasoning_content":format!("r{i:02};")})
                })
            })
            .collect();
        second.push(chunk(json!({"content":"t70;"})));
        second.push(chunk(calls("second", "must-not-exist.txt")));
        second.push(end());
        provenance(
            &world,
            &mut evidence,
            json!({"first":first,"second":second,"turn":TURN}),
        );
        let (mut provider, gate) = FakeLlmServer::start_segmented(
            SseScript::Sequence {
                scripts: vec![
                    SseScript::Chunks { chunks: first },
                    SseScript::Chunks { chunks: second },
                    SseScript::plain("new branch complete"),
                ],
            },
            budget.cap_timeout(spec.step_timeout()),
        )
        .unwrap();
        world.set_env("OPENAI_COMPATIBLE_BASE_URL", provider.base_url());
        world.set_env("OPENAI_COMPATIBLE_MODEL", "vertical");
        world.set_env("OPENAI_COMPATIBLE_API_KEY", FAKE_API_KEY);
        let ack = world.root().join("barrier.json");
        world.set_env(BARRIER, "invocation_started:1:72");
        world.set_env(ACK, ack.display().to_string());
        let daemon = support::crash::start(&world, &spec, &budget);
        let frontend = support::driver()
            .start(
                &world,
                &LatticeCommand {
                    args: vec![
                        "--json".into(),
                        "session".into(),
                        "send".into(),
                        "original input".into(),
                        "--agent".into(),
                        "vertical".into(),
                        "--turn-id".into(),
                        TURN.into(),
                    ],
                    timeout: Some(budget.cap_timeout(spec.step_timeout())),
                },
            )
            .unwrap();
        gate.wait_until_stalled(1, budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        let accepted = query(&world, TURN);
        let sid = accepted["session"]["session_id"]
            .as_str()
            .unwrap()
            .to_owned();
        evidence.record_json("accepted", &accepted).unwrap();
        for i in 1..=5 {
            gate.wait_until_stalled(i, budget.cap_timeout(spec.step_timeout()))
                .unwrap();
            gate.release_next(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
        }
        gate.wait_until_stalled(6, budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        let first_done = stage(&world, &spec, &mut evidence, &sid, TURN);
        assert_eq!(
            first_done["steps"][0]["state"]["end"],
            json!({"end":"continue","node":2})
        );
        let slots = first_done["steps"][0]["state"]["slots"].as_array().unwrap();
        assert_eq!(slots.len(), 3);
        assert!(slots.iter().all(|v| v["state"]["outcome"] == "succeeded"));
        assert_eq!(
            std::fs::read_to_string(world.project_dir().join("created.txt")).unwrap(),
            "durable\n"
        );
        let tip = history_tip(&world);
        let mut observer = Wire::connect(
            &world,
            json!({"operation":"subscribe","target":{"session_id":sid,"turn_id":TURN},"cursor":null}),
        );
        let cut = observer.next();
        assert_eq!(cut["value"]["type"], "snapshot");
        let cursor = cut["value"]["view"]["cursor"].clone();
        evidence.record_json("before-disconnect", &cut).unwrap();
        drop(observer);
        let detached = frontend.kill_and_wait().unwrap();
        support::assert_forced_kill(&detached);
        assert!(detached.stdout_complete && detached.stderr_complete);
        evidence.record_command("kill frontend only; backend continues", &detached);
        assert_eq!(world.daemon_pids().unwrap(), vec![daemon.pgid()]);
        for i in 6..=75 {
            gate.wait_until_stalled(i, budget.cap_timeout(spec.step_timeout()))
                .unwrap();
            gate.release_next(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
        }
        gate.wait_until_stalled(76, budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        let view = wait_view(&world, TURN, |v| {
            v["attempt"]["segments"]
                .as_array()
                .is_some_and(|s| s.len() == 70)
        });
        assert!(
            view["cursor"]["sequence"].as_u64().unwrap()
                > cursor["sequence"].as_u64().unwrap() + 64
        );
        let mut expected: Vec<_> = (0..70).map(|i|json!({"type":if i%2==0 {"text"}else{"reasoning"},"text":format!("{}{i:02};",if i%2==0 {"t"}else{"r"})})).collect();
        assert_eq!(view["attempt"]["segments"], json!(expected));
        assert_eq!(
            view["attempt"]["key"],
            json!({"turn_id":TURN,"model_step_ordinal":1,"attempt_ordinal":0})
        );
        let mut resumed = Wire::connect(
            &world,
            json!({"operation":"subscribe","target":{"session_id":sid,"turn_id":TURN},"cursor":cursor}),
        );
        let reset = resumed.next();
        assert_eq!(reset["value"]["type"], "snapshot");
        assert_eq!(reset["value"]["view"]["attempt"], view["attempt"]);
        assert!(
            reset["value"]["view"]["cursor"]["sequence"]
                .as_u64()
                .unwrap()
                >= view["cursor"]["sequence"].as_u64().unwrap()
        );
        evidence
            .record_json("reconnected-outside-window", &reset)
            .unwrap();
        gate.release_next(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        gate.wait_until_stalled(77, budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        let event = resumed.next();
        assert_eq!(event["value"]["type"], "event");
        assert_eq!(
            event["value"]["cursor"]["sequence"].as_u64().unwrap(),
            reset["value"]["view"]["cursor"]["sequence"]
                .as_u64()
                .unwrap()
                + 1
        );
        assert_eq!(event["value"]["event"]["type"], "token_delta");
        assert_eq!(event["value"]["event"]["key"], view["attempt"]["key"]);
        assert_eq!(event["value"]["event"]["content"], "t70;");
        expected.push(json!({"type":"text","text":"t70;"}));
        evidence.record_json("resumed-event", &event).unwrap();
        drop(resumed);
        for (index,request) in [json!({"operation":"submit","request":{"session_id":sid,"turn_id":OTHER,"workspace":world.project_dir(),"input":"must be busy","agent":"vertical"},"configuration":null}),json!({"operation":"rewind","session_id":sid,"target":2,"workspace":world.project_dir()})].into_iter().enumerate() {
            let rejected = call(&world,request); assert_eq!(rejected["value"]["kind"],"busy"); evidence.record_json(&format!("busy-{index}"),&rejected).unwrap();
        }
        let existing = call(
            &world,
            json!({"operation":"submit","request":{"session_id":null,"turn_id":TURN,"workspace":world.project_dir(),"input":"original input","agent":null},"configuration":null}),
        );
        assert_eq!(existing["value"]["acceptance"], "existing");
        assert_eq!(existing["value"]["turn"]["session_id"], sid);
        evidence.record_json("existing", &existing).unwrap();
        assert_eq!(provider.captured_requests().len(), 2);
        let log = read_json(
            &world,
            &spec,
            &mut evidence,
            &["--json", "session", "log", &sid],
        );
        assert_eq!(log["nodes"].as_array().unwrap().len(), 3);
        // 同一后台第二 Session 在 A 的 gate 放行前到达独立 provider，证明不是全 Turn 全局排队。
        let mut peer =
            support::wire_fake_llm(&world, "vertical", SseScript::plain("parallel session"));
        cli(
            &world,
            &spec,
            &mut evidence,
            &[
                "--json",
                "session",
                "send",
                "parallel",
                "--agent",
                "vertical",
                "--turn-id",
                OTHER,
            ],
            0,
        );
        assert_eq!(peer.captured_requests().len(), 1);
        peer.shutdown(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        world.set_env("OPENAI_COMPATIBLE_BASE_URL", provider.base_url());
        gate.release(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        let marker = wait_for_ack(&ack, budget.cap_timeout(spec.step_timeout()));
        assert_eq!(marker["barrier"], "invocation_started");
        assert_eq!(marker["turn_id"], TURN);
        assert_eq!(marker["session_id"], sid);
        assert_eq!(marker["step"], 1);
        assert_eq!(marker["part"], 72);
        evidence.record_json("barrier", &marker).unwrap();
        assert_eq!(world.daemon_pids().unwrap(), vec![daemon.pgid()]);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let stat = std::fs::read_to_string(format!("/proc/{}/stat", daemon.pgid())).unwrap();
            if stat
                .rsplit_once(')')
                .unwrap()
                .1
                .trim_start()
                .starts_with('T')
            {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        evidence.record_json("daemon-identity",&json!({"pid":daemon.pgid(),"pgid":daemon.pgid(),"state_root":world.session_store_dir().parent(),"binary":support::driver().binary_path(),"stopped":true})).unwrap();
        let killed = daemon.kill_and_wait().unwrap();
        support::assert_forced_kill(&killed);
        assert!(killed.stdout_complete && killed.stderr_complete);
        evidence.record_command("SIGKILL barrier backend", &killed);
        assert!(world.daemon_pids().unwrap().is_empty());
        world.unset_env(BARRIER);
        world.unset_env(ACK);
        let reopened = stage(&world, &spec, &mut evidence, &sid, TURN);
        assert!(reopened["summary"].is_null());
        assert_eq!(reopened["steps"][0], first_done["steps"][0]);
        let slots = reopened["steps"][1]["state"]["slots"].as_array().unwrap();
        assert_eq!(slots.len(), 3);
        assert_eq!(slots[0]["state"]["outcome"], "succeeded");
        assert_eq!(slots[1]["state"]["state"], "started");
        assert_eq!(slots[2]["state"]["state"], "registered");
        assert_eq!(slots[1]["invocation_id"], marker["invocation_id"]);
        let fresh = query(&world, TURN);
        assert_eq!(fresh["owned"], false);
        assert!(fresh["attempt"].is_null());
        assert_ne!(fresh["cursor"]["epoch"], cursor["epoch"]);
        assert_eq!(segments(&fresh["active"]["state"]["reply"]), expected);
        evidence.record_json("read-only-reopen", &fresh).unwrap();
        let mut reconnected = Wire::connect(
            &world,
            json!({"operation":"subscribe","target":{"session_id":sid,"turn_id":TURN},"cursor":cursor}),
        );
        let restarted_snapshot = reconnected.next();
        assert_eq!(restarted_snapshot["value"]["type"], "snapshot");
        assert_eq!(restarted_snapshot["value"]["view"], fresh);
        evidence
            .record_json("old-epoch-subscription", &restarted_snapshot)
            .unwrap();
        drop(reconnected);
        assert_eq!(provider.captured_requests().len(), 2);
        assert!(!world.project_dir().join("must-not-exist.txt").exists());
        assert_eq!(history_tip(&world), tip);
        let moved = read_json(
            &world,
            &spec,
            &mut evidence,
            &["--json", "session", "rewind", &sid, "2"],
        );
        assert_eq!(moved["current"], 2);
        let closed = stage(&world, &spec, &mut evidence, &sid, TURN);
        let replay = read_json(
            &world,
            &spec,
            &mut evidence,
            &[
                "--json",
                "audit",
                "replay",
                "--session",
                &sid,
                "--turn",
                TURN,
            ],
        );
        assert_eq!(closed["summary"]["outcome"], "failed");
        assert_eq!(closed["summary"]["notice"]["level"], "error");
        assert_eq!(closed["steps"][0], first_done["steps"][0]);
        let stopped = &closed["steps"][1]["state"];
        assert_eq!(stopped["end"]["end"], "stop");
        assert_eq!(stopped["end"]["node"], 3);
        assert_eq!(stopped["slots"][0], slots[0]);
        assert_eq!(stopped["slots"][1]["state"]["outcome"], "indeterminate");
        assert_eq!(stopped["slots"][2]["state"]["outcome"], "not_executed");
        assert_eq!(
            stopped["slots"][2]["state"]["reason"],
            json!({"kind":"not_executed","value":"preceding_invocation_indeterminate"})
        );
        assert_eq!(
            stopped["end"]["cause"],
            json!({"cause":"invocation_indeterminate","invocation_id":marker["invocation_id"]})
        );
        // 自动关闭产生的完整配对节点通过公开 History 获取，而非读取 redb。
        read_json(
            &world,
            &spec,
            &mut evidence,
            &["--json", "session", "rewind", &sid, "3"],
        );
        let mut history = Wire::connect(
            &world,
            json!({"operation":"subscribe","target":{"session_id":sid,"turn_id":TURN},"cursor":null}),
        );
        assert_eq!(history.next()["value"]["view"]["selected_turn"], closed);
        let finished = history.next();
        assert_eq!(finished["value"]["type"], "finished");
        let entries = finished["value"]["history"]["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 3);
        let content = &entries[2]["content"]["content"];
        assert_eq!(content["results"].as_array().unwrap().len(), 3);
        assert_eq!(segments(&content["assistant"]), expected);
        evidence.record_json("complete-history", &finished).unwrap();
        read_json(
            &world,
            &spec,
            &mut evidence,
            &["--json", "session", "rewind", &sid, "2"],
        );
        assert_eq!(
            read_json(
                &world,
                &spec,
                &mut evidence,
                &["--json", "session", "turn", TURN]
            ),
            closed
        );
        assert_eq!(
            read_json(
                &world,
                &spec,
                &mut evidence,
                &[
                    "--json",
                    "audit",
                    "replay",
                    "--session",
                    &sid,
                    "--turn",
                    TURN
                ]
            ),
            replay
        );
        let log = read_json(
            &world,
            &spec,
            &mut evidence,
            &["--json", "session", "log", &sid],
        );
        assert_eq!(log["nodes"].as_array().unwrap().len(), 4);
        assert_eq!(log["session"]["current"], 2);
        assert_eq!(history_tip(&world), tip);
        assert_eq!(provider.captured_requests().len(), 2);
        cli(
            &world,
            &spec,
            &mut evidence,
            &["--json", "session", "send", "new input", "--session", &sid],
            0,
        );
        let captures = provider.captured_requests();
        assert_eq!(captures.len(), 3);
        assert!(
            !captures[2].raw["messages"]
                .to_string()
                .contains("must-not-exist")
        );
        assert!(
            captures[2].raw["messages"]
                .to_string()
                .contains("first text")
        );
        evidence.record_captures(captures);
        provider
            .shutdown(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        evidence.record_json("state-summary",&json!({"turn":closed,"history_current":tip,"file_blake3":blake3::hash(&std::fs::read(world.project_dir().join("created.txt")).unwrap()).to_hex().to_string(),"provider_requests":3,"peer_requests":1})).unwrap();
    }));
    finish(evidence, result, "s2-vertical");
}

#[test]
fn crash_start_cannot_reset_an_expired_scenario_budget() {
    let (world, spec) = support::direct_world("crash-budget-expired", "budget", "[]", "");
    let budget = lattice_system_harness::ScenarioBudget::new(Some(Duration::ZERO));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        support::crash::start(&world, &spec, &budget)
    }));
    assert!(result.is_err());
    assert!(world.daemon_pids().unwrap().is_empty());
}

/// 旧 Git 子阶段归并为单库提交点；每个 case 核验表中相应关闭行，不重放 provider/Tool。
#[test]
fn redb_commit_windows_close_only_and_preserve_terminal_facts() {
    for (name, barrier, mode, closed_outcome) in [
        ("accepted", "turn_accepted", "plain", None),
        ("requesting", "request_started", "plain", None),
        ("retry", "attempt_stopped", "retry", None),
        ("registered", "calls_registered", "tool", None),
        ("started", "invocation_started", "tool", None),
        ("terminal", "invocation_finished", "tool", None),
        ("continue", "step_closed", "tool", None),
        ("oversize", "invocation_finished", "oversize", None),
        ("finish", "step_closed", "plain", Some("completed")),
        ("invalid", "step_closed", "invalid", Some("failed")),
    ] {
        let (world, spec) = support::direct_world(
            &format!("r2-crash-{name}"),
            "matrix",
            "[\"mutate.create\",\"session.todo_write\"]",
            "\n[llm.retry]\nmax_retries = 1\n",
        );
        let budget = world.budget(&spec);
        let mut evidence = support::evidence(&world, &spec);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let script = match mode {
                "tool" => support::tool_call(
                    "create",
                    "mutate.create",
                    r#"{"path":"kept.txt","content":"kept\n","create_parents":false}"#,
                ),
                "oversize" => support::tool_call(
                    "large",
                    "session.todo_write",
                    &json!({"todos":[{"content":"x".repeat(70_000),"status":"pending"}]})
                        .to_string(),
                ),
                "retry" => SseScript::HttpError {
                    status: 503,
                    body: r#"{"error":{"message":"retry"}}"#.into(),
                },
                "invalid" => {
                    support::tool_call("outsider", "fetch.web", r#"{"url":"https://example.com"}"#)
                }
                _ => SseScript::plain("complete"),
            };
            provenance(
                &world,
                &mut evidence,
                json!({"case":name,"barrier":barrier}),
            );
            let mut provider = support::wire_fake_llm(&world, "matrix", script);
            let ack = world.root().join("marker.json");
            world.set_env(BARRIER, barrier);
            world.set_env(ACK, ack.display().to_string());
            let daemon = support::crash::start(&world, &spec, &budget);
            let frontend = support::driver()
                .start(
                    &world,
                    &LatticeCommand {
                        args: vec![
                            "--json".into(),
                            "session".into(),
                            "send".into(),
                            "matrix input".into(),
                            "--agent".into(),
                            "matrix".into(),
                            "--turn-id".into(),
                            TURN.into(),
                        ],
                        timeout: Some(budget.cap_timeout(spec.step_timeout())),
                    },
                )
                .unwrap();
            let marker = wait_for_ack(&ack, budget.cap_timeout(spec.step_timeout()));
            assert_eq!(marker["barrier"], barrier);
            assert_eq!(marker["turn_id"], TURN);
            evidence.record_json("marker", &marker).unwrap();
            support::crash::kill(&world, daemon, frontend, &mut evidence, &budget);
            world.unset_env(BARRIER);
            world.unset_env(ACK);
            let sid = marker["session_id"].as_str().unwrap();
            let before = stage(&world, &spec, &mut evidence, sid, TURN);
            let requests = provider.captured_requests().len();
            assert_eq!(
                requests,
                usize::from(!matches!(name, "accepted" | "requesting"))
            );
            assert_eq!(before["summary"]["outcome"].as_str(), closed_outcome);
            let slots = before["steps"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|s| s["state"]["slots"].as_array().into_iter().flatten())
                .cloned()
                .collect::<Vec<_>>();
            if mode == "tool" || mode == "oversize" {
                assert_eq!(slots.len(), 1);
                assert_eq!(
                    slots[0]["state"]["state"],
                    if name == "registered" {
                        "registered"
                    } else if name == "started" {
                        "started"
                    } else {
                        "terminal"
                    }
                );
            }
            if name == "retry" {
                assert_eq!(before["steps"][0]["state"]["state"], "retry_ready");
                assert_eq!(before["steps"][0]["attempts"].as_array().unwrap().len(), 1);
            }
            if name == "oversize" {
                assert_eq!(slots[0]["state"]["outcome"], "succeeded");
                assert_eq!(slots[0]["state"]["material"]["kind"], "output_too_large");
                assert!(
                    slots[0]["state"]["material"]["byte_length"]
                        .as_u64()
                        .unwrap()
                        > 65_536
                );
            }
            let installed = world.project_dir().join("kept.txt").exists();
            assert_eq!(installed, matches!(name, "terminal" | "continue"));
            let tip = installed.then(|| history_tip(&world));
            read_json(
                &world,
                &spec,
                &mut evidence,
                &["--json", "session", "rewind", sid, "0"],
            );
            let after = stage(&world, &spec, &mut evidence, sid, TURN);
            if closed_outcome.is_some() {
                assert_eq!(before, after);
            } else {
                assert_eq!(after["summary"]["outcome"], "failed");
                assert_eq!(after["summary"]["notice"]["level"], "error");
                let steps = after["steps"].as_array().unwrap();
                let last = &steps.last().unwrap()["state"]["end"];
                assert_eq!(last["end"], "stop");
                assert_eq!(
                    last["cause"]["cause"],
                    if name == "started" {
                        "invocation_indeterminate"
                    } else {
                        "process_exit"
                    }
                );
                assert_eq!(
                    last["node"].is_null(),
                    matches!(name, "accepted" | "requesting" | "retry" | "continue")
                );
                assert!(
                    steps[..steps.len() - 1]
                        .iter()
                        .all(|s| s["state"]["end"]["end"] == "continue")
                );
                if name == "accepted" || name == "continue" {
                    assert!(
                        steps.last().unwrap()["attempts"]
                            .as_array()
                            .unwrap()
                            .is_empty()
                    );
                }
                if name == "requesting" {
                    assert_eq!(
                        steps[0]["attempts"][0]["end"]["cause"]["cause"],
                        "process_exit"
                    );
                }
                if name == "retry" {
                    assert_eq!(steps[0]["attempts"], before["steps"][0]["attempts"]);
                }
                if let Some(original) = slots.first() {
                    let terminal = &steps[0]["state"]["slots"][0];
                    if original["state"]["state"] == "terminal" {
                        assert_eq!(terminal, original);
                    } else {
                        assert_eq!(
                            terminal["state"]["outcome"],
                            if name == "started" {
                                "indeterminate"
                            } else {
                                "not_executed"
                            }
                        );
                    }
                }
            }
            if name == "invalid" {
                assert_eq!(
                    after["steps"][0]["state"]["end"]["cause"]["cause"],
                    "invalid_batch"
                );
                assert_eq!(after["usage"]["total"]["total_tokens"], 2);
            }
            if let Some(tip) = tip {
                assert_eq!(history_tip(&world), tip);
                assert_eq!(
                    std::fs::read_to_string(world.project_dir().join("kept.txt")).unwrap(),
                    "kept\n"
                );
            }
            read_json(
                &world,
                &spec,
                &mut evidence,
                &["--json", "session", "rewind", sid, "0"],
            );
            assert_eq!(
                read_json(
                    &world,
                    &spec,
                    &mut evidence,
                    &["--json", "session", "turn", TURN]
                ),
                after
            );
            assert_eq!(provider.captured_requests().len(), requests);
            evidence.record_captures(provider.captured_requests());
            provider
                .shutdown(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
        }));
        finish(evidence, result, name);
    }
}

#[test]
fn persisted_cancel_after_mutation_survives_crash_without_replay() {
    let (world, spec) =
        support::direct_world("r2-crash-cancelled", "cancelled", "[\"mutate.create\"]", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (mut provider, gate) = support::wire_gated_fake_llm(
            &world,
            "cancelled",
            support::sequence(vec![
                support::tool_call(
                    "mutation",
                    "mutate.create",
                    r#"{"path":"kept.txt","content":"kept\n","create_parents":false}"#,
                ),
                SseScript::plain("must not arrive"),
            ]),
        );
        provenance(
            &world,
            &mut evidence,
            json!({"case":"persisted cancellation after mutation","barrier":"attempt_stopped"}),
        );
        let ack = world.root().join("marker.json");
        world.set_env(BARRIER, "attempt_stopped");
        world.set_env(ACK, ack.display().to_string());
        let daemon = support::crash::start(&world, &spec, &budget);
        let frontend = support::driver()
            .start(
                &world,
                &LatticeCommand {
                    args: vec![
                        "--json".into(),
                        "session".into(),
                        "send".into(),
                        "create then cancel".into(),
                        "--agent".into(),
                        "cancelled".into(),
                        "--turn-id".into(),
                        TURN.into(),
                    ],
                    timeout: Some(budget.cap_timeout(spec.step_timeout())),
                },
            )
            .unwrap();
        gate.wait_until_stalled(1, budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        gate.release_next(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        gate.wait_until_stalled(2, budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        let before = query(&world, TURN);
        let sid = before["session"]["session_id"].as_str().unwrap();
        assert_eq!(
            before["selected_turn"]["steps"][0]["state"]["slots"][0]["state"]["outcome"],
            "succeeded"
        );
        let tip = history_tip(&world);
        let cancel = call(
            &world,
            json!({"operation":"cancel","target":{"session_id":sid,"turn_id":TURN}}),
        );
        assert_eq!(cancel["value"]["owned"], true);
        evidence.record_json("cancel", &cancel).unwrap();
        let marker = wait_for_ack(&ack, budget.cap_timeout(spec.step_timeout()));
        assert_eq!(marker["barrier"], "attempt_stopped");
        assert_eq!(marker["step"], 1);
        assert_eq!(marker["turn_id"], TURN);
        evidence.record_json("marker", &marker).unwrap();
        support::crash::kill(&world, daemon, frontend, &mut evidence, &budget);
        world.unset_env(BARRIER);
        world.unset_env(ACK);
        let closed = stage(&world, &spec, &mut evidence, sid, TURN);
        assert_eq!(closed["summary"]["outcome"], "cancelled");
        assert_eq!(closed["summary"]["notice"]["level"], "info");
        assert_eq!(closed["steps"][0], before["selected_turn"]["steps"][0]);
        assert_eq!(
            closed["steps"][1]["state"]["end"],
            json!({"end":"stop","node":null,"cause":{"cause":"cancelled"}})
        );
        read_json(
            &world,
            &spec,
            &mut evidence,
            &["--json", "session", "rewind", sid, "0"],
        );
        assert_eq!(
            read_json(
                &world,
                &spec,
                &mut evidence,
                &["--json", "session", "turn", TURN]
            ),
            closed
        );
        assert_eq!(history_tip(&world), tip);
        assert_eq!(
            std::fs::read_to_string(world.project_dir().join("kept.txt")).unwrap(),
            "kept\n"
        );
        assert_eq!(provider.captured_requests().len(), 2);
        evidence.record_captures(provider.captured_requests());
        provider
            .shutdown(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
    }));
    finish(evidence, result, "cancelled");
}
