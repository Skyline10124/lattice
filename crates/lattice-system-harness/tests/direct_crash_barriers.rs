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
    /// `wait` 内未收到任何字节；不读取半帧，收到即失败。
    fn assert_silent(&mut self, wait: Duration) {
        self.stream.set_read_timeout(Some(wait)).unwrap();
        let read = self.stream.read(&mut [0; 1]);
        self.stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        assert!(
            matches!(&read, Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)),
            "unexpected delivery: {read:?}"
        );
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
fn provenance(world: &ScenarioWorld, evidence: &mut ScenarioEvidence, script: Value) {
    let driver = support::driver();
    let bytes = std::fs::read(driver.binary_path()).unwrap();
    let test = script["test"].as_str().expect("exact test identity");
    evidence.record_json("provenance", &json!({
        "binary":driver.binary_path(),"binary_blake3":blake3::hash(&bytes).to_hex().to_string(),
        "features":"test-crash-barriers","harness_features":"crash-barriers","profile":"debug",
        "state_root":world.session_store_dir().parent(),"script":script,
        "reproduce":format!("CARGO_BUILD_JOBS=8 cargo build -p lattice-cli --bin lattice --features test-crash-barriers && LATTICE_BIN=$(pwd)/target/debug/lattice CARGO_BUILD_JOBS=8 cargo test -p lattice-system-harness --features crash-barriers --test direct_crash_barriers {test} -- --exact --nocapture")
    })).unwrap();
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
fn late_successful_history_page_cannot_replace_newer_tui_snapshot() {
    use lattice_system_harness::PtyTuiDriver;
    let (world, spec) = support::direct_world(
        "history-late-success",
        "history",
        "[\"observe.workspace\",\"mutate.create\"]",
        "",
    );
    world.write_project_file("seed.txt", "seed\n").unwrap();
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut delta = calls("late", "kept.txt");
        delta["content"] = json!("OLD_STEP_BODY");
        let script = support::sequence(vec![
            SseScript::Chunks {
                chunks: vec![chunk(delta), end()],
            },
            SseScript::plain("NEW_FINAL_BODY"),
        ]);
        provenance(
            &world,
            &mut evidence,
            json!({"test":"late_successful_history_page_cannot_replace_newer_tui_snapshot",
            "script":format!("{script:?}"),"old_anchor":2,"new_anchor":3}),
        );
        let (provider, gate) = FakeLlmServer::start_segmented(script, spec.step_timeout()).unwrap();
        let schedule = world.root().join("schedule");
        std::fs::create_dir(&schedule).unwrap();
        std::fs::write(schedule.join("history_ready_2.arm"), b"").unwrap();
        world.set_env("LATTICE_TEST_SCHEDULE_DIR", schedule.display().to_string());
        let daemon = support::crash::start(&world, &spec, &budget);
        let request = json!({"operation":"submit","request":{"session_id":null,"turn_id":TURN,
            "workspace":world.project_dir(),"input":"late page","agent":"history"},"configuration":{
            "profile_toml":std::fs::read_to_string(world.project_dir().join(".lattice/agents/history/agent.toml")).unwrap(),
            "system_prompt":"You are a concise test agent.","connection":{"alias":"compat:history",
            "provider":"openai_compatible","provider_model":"history","base_url":provider.base_url(),"api_key":FAKE_API_KEY,"roots_der":[]}}});
        let accepted = call(&world, request);
        assert_eq!(accepted["value"]["acceptance"], "accepted", "{accepted}");
        for n in 1..=2 {
            gate.wait_until_stalled(n, budget.cap_timeout(spec.step_timeout()))
                .unwrap();
            gate.release_next(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
        }
        gate.wait_until_stalled(3, budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        let old = wait_view(&world, TURN, |v| v["session"]["current"] == 2);
        let sid = old["session"]["session_id"].as_str().unwrap();
        let mut tui = PtyTuiDriver::reconnect(&support::driver(), &world, sid).unwrap();
        let observed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tui.wait_for_ready(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
            let ready = wait_for_ack(
                &schedule.join("history_ready_2.ready"),
                budget.cap_timeout(spec.step_timeout()),
            );
            assert_eq!(ready["anchor"], old["history"]);
            assert!(ready["fragments"].as_u64().unwrap() > 0);
            assert!(!schedule.join("history_ready_2.release").exists());
            // 第二条公开读取核实旧页实际内容，首个TUI读取仍留在成功后的gate内。
            let page = call(
                &world,
                json!({"operation":"history","anchor":old["history"],"cursor":null,"max_bytes":65536}),
            );
            assert_eq!(page["response"], "history");
            assert!(page.to_string().contains("OLD_STEP_BODY"));
            assert!(!page.to_string().contains("NEW_FINAL_BODY"));
            gate.release(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
            let closed = wait_view(&world, TURN, |v| {
                v["selected_turn"]["summary"]["outcome"] == "completed"
            });
            tui.send_line("/reconnect").unwrap();
            tui.wait_for(budget.cap_timeout(spec.step_timeout()), |text| {
                text.contains("NEW_FINAL_BODY") && text.contains("completed")
            })
            .unwrap();
            let newer = tui
                .redraw_text(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
            assert!(
                newer.contains("OLD_STEP_BODY")
                    && newer.contains("NEW_FINAL_BODY")
                    && newer.contains("completed")
            );
            std::fs::write(schedule.join("history_return_2.arm"), b"").unwrap();
            std::fs::write(schedule.join("history_return_2.release"), b"").unwrap();
            std::fs::write(schedule.join("history_ready_2.release"), b"").unwrap();
            let returned = wait_for_ack(
                &schedule.join("history_return_2.ready"),
                budget.cap_timeout(spec.step_timeout()),
            );
            assert_eq!(returned["anchor"], old["history"]);
            let after = tui
                .redraw_text(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
            assert!(
                after.contains("NEW_FINAL_BODY") && after.contains("completed"),
                "{after}"
            );
            assert_eq!(after.matches("OLD_STEP_BODY").count(), 1);
            assert_eq!(after.matches("NEW_FINAL_BODY").count(), 1);
            let mut observer = Wire::connect(
                &world,
                json!({"operation":"subscribe","target":{"session_id":sid,"turn_id":TURN},"cursor":null}),
            );
            assert_eq!(observer.next()["value"]["view"]["history"]["head"], 3);
            let finished = observer.next();
            assert_eq!(finished["value"]["type"], "finished");
            assert_paired_history(&finished["value"]["history"], &closed["selected_turn"]);
            evidence
                .record_json(
                    "late-success-window",
                    &json!({"ready":ready,"old_page":page,"newer_frame":newer,
                "returned":returned,"after_frame":after,"finished":finished}),
                )
                .unwrap();
            stage(&world, &spec, &mut evidence, sid, TURN);
            evidence
                .record_json(
                    "state-summary",
                    &json!({"turn":closed["selected_turn"],"provider_requests":2}),
                )
                .unwrap();
            tui.send_line("/quit").unwrap();
            tui.wait_for_exit(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
        }));
        evidence.record_pty(tui.output());
        drop(tui);
        if let Err(error) = observed {
            std::panic::resume_unwind(error);
        }
        assert_eq!(provider.captured_requests().len(), 2);
        evidence.record_captures(provider.captured_requests());
        cli(
            &world,
            &spec,
            &mut evidence,
            &["--json", "daemon", "stop"],
            0,
        );
        let stopped = daemon.wait(None).unwrap();
        evidence.record_command("late-page backend stop", &stopped);
        assert!(stopped.is_clean_exit(0));
    }));
    support::finish_recorded(evidence, result);
}

#[test]
fn cross_project_operator_lifecycle_cursor_and_payload_isolation() {
    use lattice_system_harness::{RunningCommand, SessionSendDriver};
    use std::collections::{BTreeMap, BTreeSet};
    let (world, spec) = support::direct_world(
        "operator-global-matrix",
        "operator",
        "[\"observe.workspace\",\"session.todo_write\",\"fetch.web\"]",
        "\n[lancet.capabilities]\nmax_boundary = \"ex\"\n[lancet.network]\nallow_localhost = true\nallowed_hosts = [\"localhost\"]\n",
    );
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let profile = world
            .project_dir()
            .join(".lattice/agents/operator/agent.toml");
        let beta = world.root().join("beta");
        std::fs::create_dir_all(beta.join(".lattice/agents/operator")).unwrap();
        std::fs::copy(&profile, beta.join(".lattice/agents/operator/agent.toml")).unwrap();
        std::fs::write(beta.join(".env"), "").unwrap();
        gix::init(&beta).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!(
            "http://localhost:{}/operator-hold",
            listener.local_addr().unwrap().port()
        );
        let bad_tls = lattice_system_harness::FixtureHttpServer::start(
            lattice_system_harness::HttpFixture::plain("not TLS"),
        )
        .unwrap();
        let mut batch = (0..19)
            .map(|i| {
                (
                    format!("small-{i}"),
                    "session.todo_write".into(),
                    json!({"todos":[{"content":format!("private-small-{i}"),"status":"pending"}]})
                        .to_string(),
                )
            })
            .collect::<Vec<_>>();
        batch.extend([
            (
                "large".into(),
                "session.todo_write".into(),
                json!({"todos":[{"content":"private-large-".repeat(6000),"status":"pending"}]})
                    .to_string(),
            ),
            (
                "bad-tls".into(),
                "fetch.web".into(),
                json!({"url":bad_tls.url_path("/tls").replacen("http:", "https:", 1)}).to_string(),
            ),
            (
                "missing".into(),
                "observe.workspace".into(),
                json!({"mode":"read_file","path":"absent-private.txt"}).to_string(),
            ),
        ]);
        let waiting = SseScript::ToolCalls {
            calls: vec![
                (
                    "fetch".into(),
                    "fetch.web".into(),
                    json!({"url":url}).to_string(),
                ),
                (
                    "tail".into(),
                    "session.todo_write".into(),
                    json!({"todos":[{"content":"never-private","status":"pending"}]}).to_string(),
                ),
            ],
        };
        let script = support::sequence(vec![
            SseScript::ToolCalls { calls: batch },
            SseScript::plain("matrix complete"),
            waiting.clone(),
            waiting,
        ]);
        provenance(
            &world,
            &mut evidence,
            json!({"test":"cross_project_operator_lifecycle_cursor_and_payload_isolation",
            "script":format!("{script:?}"),"projects":2,"first_calls":22,"http":"hold response for cancellation or SIGKILL"}),
        );
        let provider = support::wire_fake_llm(&world, "operator", script);
        let driver = support::driver();
        let mut daemon = Some(support::crash::start(&world, &spec, &budget));
        let send = SessionSendDriver::new(&driver, &world, &spec, "operator");
        let first = send.send("all determinate outcomes").unwrap();
        evidence.record_command("alpha determinate", &first.command);
        support::assert_clean_send(&first).unwrap();
        assert!(first.command.is_clean_exit(0));
        let sid = send.session_id().unwrap();
        let tid = first.accepted_turn_id().unwrap();
        let initial = stage(&world, &spec, &mut evidence, sid, tid);
        let slots = initial["steps"][0]["state"]["slots"].as_array().unwrap();
        assert_eq!(slots.len(), 22);
        assert_eq!(slots[19]["state"]["outcome"], "succeeded");
        assert_eq!(slots[19]["state"]["material"]["kind"], "output_too_large");
        assert!(
            slots[19]["state"]["material"]["byte_length"]
                .as_u64()
                .unwrap()
                > 65536
        );
        assert_eq!(slots[20]["state"]["outcome"], "failed");
        // 策略或校验拒绝不再是独立结局：它是 Failed 里的错误族，错误码与可重试性原样保留。
        assert_eq!(slots[21]["state"]["outcome"], "failed");
        assert_eq!(
            slots[21]["state"]["reason"],
            json!({"kind":"failed","value":{"code":"type_error","retriability":"permanent"}})
        );
        let before_beta = read_json(&world, &spec, &mut evidence, &["--json", "audit", "--tail"]);
        let cursor = before_beta["next_after"].as_u64().unwrap().to_string();
        let beta_provider = FakeLlmServer::start(support::sequence(vec![
            support::tool_call(
                "beta",
                "session.todo_write",
                r#"{"todos":[{"content":"beta-private","status":"pending"}]}"#,
            ),
            SseScript::plain("beta complete"),
        ]))
        .unwrap();
        let beta_read = |args: &[&str], evidence: &mut ScenarioEvidence| {
            let mut cmd = world.command(driver.binary_path());
            cmd.current_dir(&beta)
                .env("OPENAI_COMPATIBLE_BASE_URL", beta_provider.base_url())
                .args(args);
            let out = RunningCommand::spawn(cmd)
                .unwrap()
                .wait(Some(budget.cap_timeout(spec.step_timeout())))
                .unwrap();
            evidence.record_command(format!("beta {}", args.join(" ")), &out);
            assert!(out.is_clean_exit(0), "{}", out.summary());
            out
        };
        let second = beta_read(
            &[
                "--json",
                "session",
                "send",
                "beta input",
                "--agent",
                "operator",
                "--turn-id",
                OTHER,
            ],
            &mut evidence,
        );
        assert!(
            std::str::from_utf8(&second.stdout)
                .unwrap()
                .lines()
                .all(|l| serde_json::from_str::<Value>(l).is_ok())
        );
        assert_eq!(world.daemon_pids().unwrap().len(), 1);
        let beta_turn: Value = serde_json::from_slice(
            &beta_read(&["--json", "session", "turn", OTHER], &mut evidence).stdout,
        )
        .unwrap();
        let beta_sid = beta_turn["session_id"].as_str().unwrap();
        let empty = read_json(
            &world,
            &spec,
            &mut evidence,
            &["--json", "audit", "--tail", "--since", &cursor],
        );
        assert_eq!(empty["rows"], json!([]));
        assert_eq!(
            empty["next_after"].as_u64().unwrap(),
            cursor.parse::<u64>().unwrap() + 3
        );
        assert_eq!(empty["done"], true);
        let beta_tail: Value = serde_json::from_slice(
            &beta_read(
                &["--json", "audit", "--tail", "--since", &cursor],
                &mut evidence,
            )
            .stdout,
        )
        .unwrap();
        assert_eq!(beta_tail["rows"].as_array().unwrap().len(), 3);
        assert_eq!(beta_tail["next_after"], empty["next_after"]);
        let mut turns = vec![tid.to_string()];
        let mut windows = Vec::new();
        for (index, crash) in [false, true].into_iter().enumerate() {
            let turn = format!("00000000-0000-4000-8000-{:012}", index + 3);
            let front = driver
                .start(
                    &world,
                    &LatticeCommand {
                        args: [
                            "--json",
                            "session",
                            "send",
                            "wait fetch",
                            "--session",
                            sid,
                            "--turn-id",
                            &turn,
                        ]
                        .into_iter()
                        .map(String::from)
                        .collect(),
                        timeout: Some(budget.cap_timeout(spec.step_timeout())),
                    },
                )
                .unwrap();
            let deadline = Instant::now() + budget.cap_timeout(spec.step_timeout());
            let mut http = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "fetch did not arrive");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            http.set_read_timeout(Some(budget.cap_timeout(spec.step_timeout())))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                http.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                assert!(request.len() <= 16384);
            }
            assert!(request.starts_with(b"GET /operator-hold HTTP/1.1\r\n"));
            let active = read_json(
                &world,
                &spec,
                &mut evidence,
                &["--json", "audit", "--turn", &turn],
            );
            let states: BTreeSet<_> = active["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["state"]["state"].as_str().unwrap())
                .collect();
            assert_eq!(states, BTreeSet::from(["registered", "started"]));
            let replay = read_json(
                &world,
                &spec,
                &mut evidence,
                &[
                    "--json",
                    "audit",
                    "replay",
                    "--session",
                    sid,
                    "--turn",
                    &turn,
                ],
            );
            assert_eq!(replay["calls"][0]["state"]["state"], "started");
            assert_eq!(replay["calls"][1]["state"]["state"], "registered");
            let count = provider.captured_requests().len();
            if crash {
                let owned = daemon.take().unwrap();
                assert_eq!(world.daemon_pids().unwrap(), vec![owned.pgid()]);
                evidence.record_json("crash-processes", &json!({"backend_pgid":owned.pgid(),"frontend_pgid":front.pgid(),"cut":"real fetch received, Started durable","state_root":world.session_store_dir().parent()})).unwrap();
                assert_ne!(owned.pgid(), front.pgid());
                let killed = owned.kill_and_wait().unwrap();
                evidence.record_command("SIGKILL backend during real fetch", &killed);
                support::assert_forced_kill(&killed);
                assert!(world.daemon_pids().unwrap().is_empty());
                let out = front.wait(None).unwrap();
                evidence.record_command("frontend observes fetch crash", &out);
                assert!(out.is_clean_exit(1));
                let orphan = stage(&world, &spec, &mut evidence, sid, &turn);
                assert!(orphan["summary"].is_null());
                assert_eq!(
                    orphan["steps"][0]["state"]["slots"][0]["state"]["state"],
                    "started"
                );
                let head = query(&world, &turn)["session"]["current"]
                    .as_u64()
                    .unwrap()
                    .to_string();
                read_json(
                    &world,
                    &spec,
                    &mut evidence,
                    &["--json", "session", "rewind", sid, &head],
                );
            } else {
                assert_eq!(
                    call(
                        &world,
                        json!({"operation":"cancel","target":{"session_id":sid,"turn_id":turn}})
                    )["value"]["owned"],
                    true
                );
                let out = front.wait(None).unwrap();
                evidence.record_command("cancelled fetch", &out);
                assert!(out.is_clean_exit(2));
            }
            let closed = stage(&world, &spec, &mut evidence, sid, &turn);
            // 已 Started 的调用没有确定结果：崩溃时是未取得结果，取消时是 LANCET 返回的 CallCancelled。
            assert_eq!(
                closed["steps"][0]["state"]["slots"][0]["state"]["outcome"],
                "indeterminate"
            );
            assert_eq!(
                closed["steps"][0]["state"]["slots"][0]["state"]["reason"],
                json!({"kind":"indeterminate","value":if crash { "no_result" } else { "cancelled" }})
            );
            assert_eq!(
                closed["steps"][0]["state"]["end"]["cause"]["cause"],
                if crash { "process_exit" } else { "cancelled" }
            );
            assert!(closed["steps"][0]["state"]["slots"][1]["state"]["reason"].is_null());
            assert_eq!(
                closed["steps"][0]["state"]["slots"][1]["state"]["outcome"],
                "not_executed"
            );
            assert_eq!(provider.captured_requests().len(), count);
            windows.push(
                json!({"turn":turn,"active":active,"replay":replay,"closed":closed,"crash":crash}),
            );
            turns.push(turn);
        }
        std::fs::remove_file(&profile).unwrap();
        std::fs::remove_file(beta.join(".lattice/agents/operator/agent.toml")).unwrap();
        world.unset_env("OPENAI_COMPATIBLE_API_KEY");
        let alpha = read_json(&world, &spec, &mut evidence, &["--json", "audit"]);
        let alpha_tail = read_json(&world, &spec, &mut evidence, &["--json", "audit", "--tail"]);
        let beta_query: Value =
            serde_json::from_slice(&beta_read(&["--json", "audit"], &mut evidence).stdout).unwrap();
        assert_eq!(alpha["rows"].as_array().unwrap().len(), 26);
        assert_eq!(beta_query["rows"].as_array().unwrap().len(), 1);
        for (data, expected) in [(&alpha, sid), (&beta_query, beta_sid)] {
            assert!(
                data["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|r| r["binding"]["session_id"] == expected)
            );
        }
        let outcomes: BTreeSet<_> = alpha["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["state"]["outcome"].as_str().unwrap())
            .collect();
        assert_eq!(
            outcomes,
            BTreeSet::from(["succeeded", "failed", "not_executed", "indeterminate"])
        );
        let changes = alpha_tail["rows"].as_array().unwrap();
        let mut latest = BTreeMap::new();
        let mut previous = 0;
        for change in changes {
            let seq = change["sequence"].as_u64().unwrap();
            assert!(seq > previous);
            previous = seq;
            assert_eq!(change["row"]["binding"]["session_id"], sid);
            latest.insert(change["row"]["invocation_id"].as_str().unwrap(), change);
        }
        for row in alpha["rows"].as_array().unwrap() {
            assert_eq!(&latest[row["invocation_id"].as_str().unwrap()]["row"], row);
        }
        let mut newest = latest.values().collect::<Vec<_>>();
        newest.sort_by_key(|r| std::cmp::Reverse(r["sequence"].as_u64().unwrap()));
        let status = read_json(
            &world,
            &spec,
            &mut evidence,
            &["--json", "lancet", "status"],
        );
        assert_eq!(
            status["recent"],
            json!(
                newest
                    .iter()
                    .take(20)
                    .map(|r| &r["row"])
                    .collect::<Vec<_>>()
            )
        );
        assert_eq!(status["latest_change"], alpha_tail["latest_change"]);
        let beta_status: Value = serde_json::from_slice(
            &beta_read(&["--json", "lancet", "status"], &mut evidence).stdout,
        )
        .unwrap();
        assert_eq!(beta_status["recent"], beta_query["rows"]);
        assert_eq!(beta_status["latest_change"], status["latest_change"]);
        let final_cursor = alpha_tail["next_after"].as_u64().unwrap().to_string();
        let final_page = read_json(
            &world,
            &spec,
            &mut evidence,
            &["--json", "audit", "--tail", "--since", &final_cursor],
        );
        assert_eq!(final_page["rows"], json!([]));
        assert_eq!(final_page["next_after"], alpha_tail["next_after"]);
        let foreign = read_json(
            &world,
            &spec,
            &mut evidence,
            &["--json", "audit", "--session", beta_sid],
        );
        assert_eq!(foreign["rows"], json!([]));
        cli(
            &world,
            &spec,
            &mut evidence,
            &[
                "--json",
                "audit",
                "replay",
                "--session",
                beta_sid,
                "--turn",
                OTHER,
            ],
            2,
        );
        for value in [&alpha, &alpha_tail, &beta_query, &status, &beta_status] {
            let text = value.to_string();
            // Cell 证据允许项目相对路径；这里仅禁止参数/结果正文与 schema。
            for private in [
                "private-small-",
                "private-large-",
                "beta-private",
                "never-private",
                "input_schema",
            ] {
                assert!(!text.contains(private), "operator leaked {private}");
            }
        }
        for turn in &turns {
            stage(&world, &spec, &mut evidence, sid, turn);
        }
        beta_read(&["session", "log", beta_sid], &mut evidence);
        beta_read(&["--json", "session", "log", beta_sid], &mut evidence);
        beta_read(
            &[
                "--json",
                "audit",
                "replay",
                "--session",
                beta_sid,
                "--turn",
                OTHER,
            ],
            &mut evidence,
        );
        assert_eq!(provider.captured_requests().len(), 4);
        assert_eq!(beta_provider.captured_requests().len(), 2);
        evidence.record_json("state-summary", &json!({"alpha":alpha,"beta":beta_query,"windows":windows,
            "empty_project_page":empty,"foreign_changes":beta_tail,"recent":status,"beta_recent":beta_status})).unwrap();
        evidence.record_captures(provider.captured_requests());
        evidence.record_captures(beta_provider.captured_requests());
    }));
    support::finish_recorded(evidence, result);
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
            json!({"test":"r2_s2_multicall_reconnect_busy_sigkill_auto_close","first":first,"second":second,"turn":TURN}),
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
            stopped["slots"][1]["state"]["reason"],
            json!({"kind":"indeterminate","value":"no_result"})
        );
        // 没轮到的槽位不带原因；为什么停只记在 Step 的 end.cause。
        assert!(stopped["slots"][2]["state"]["reason"].is_null());
        // 崩溃发生在 Started 前后，停止原因都是进程退出。
        assert_eq!(stopped["end"]["cause"], json!({"cause":"process_exit"}));
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
    support::finish_recorded(evidence, result);
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

/// 公开历史中的每个 Step 必须与详情逐槽位配对，不只比较调用数。
fn assert_paired_history(history: &Value, turn: &Value) {
    let steps = turn["steps"].as_array().unwrap();
    let mut ids = std::collections::BTreeSet::new();
    let entries = history["entries"].as_array().unwrap();
    let mut expected_nodes = Vec::new();
    for (ordinal, step) in steps.iter().enumerate() {
        assert_eq!(step["state"]["state"], "closed");
        let end = &step["state"]["end"];
        assert_eq!(end["end"] == "continue", ordinal + 1 < steps.len());
        let Some(node) = end["node"].as_u64() else {
            continue;
        };
        expected_nodes.push(node);
        let entry = entries.iter().find(|entry| entry["node"] == node).unwrap();
        assert_eq!(entry["content"]["step_ordinal"], ordinal);
        assert_eq!(entry["content"]["turn_id"], turn["turn_id"]);
        let content = &entry["content"]["content"];
        let calls: Vec<_> = content["assistant"]["parts"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .filter_map(|(part, value)| {
                value
                    .pointer("/semantic/tool_call/call")
                    .map(|call| (part, call))
            })
            .collect();
        let results = content["results"].as_array().unwrap();
        let slots = step["state"]["slots"].as_array().unwrap();
        assert_eq!(calls.len(), results.len());
        assert_eq!(calls.len(), slots.len());
        for (((part, call), result), slot) in calls.iter().zip(results).zip(slots) {
            assert!(ids.insert(call["invocation_id"].as_str().unwrap()));
            assert_eq!(call["invocation_id"], result["invocation_id"]);
            assert_eq!(call["invocation_id"], slot["invocation_id"]);
            assert_eq!(call["name"], result["name"]);
            assert_eq!(call["name"], slot["binding"]["method"]);
            assert_eq!(call["address"]["part_ordinal"], *part);
            assert_eq!(
                call["address"]["message"],
                json!({
                    "session_id":turn["session_id"],"turn_id":turn["turn_id"],"model_step_ordinal":ordinal
                })
            );
            assert_eq!(result["payload"]["outcome"], slot["state"]["outcome"]);
        }
    }
    let actual_nodes: Vec<_> = entries
        .iter()
        .filter(|entry| entry["content"]["kind"] == "step")
        .map(|entry| entry["node"].as_u64().unwrap())
        .collect();
    assert_eq!(actual_nodes, expected_nodes);
    let end = &steps.last().unwrap()["state"]["end"];
    let outcome = if end["end"] == "finish" {
        "completed"
    } else if end["cause"]["cause"] == "cancelled" {
        "cancelled"
    } else {
        "failed"
    };
    assert_eq!(turn["summary"]["outcome"], outcome);
}

#[test]
fn redb_commit_windows_close_only_and_preserve_terminal_facts() {
    for (name, barrier, mode, closed_outcome, requests) in [
        ("accepted", "turn_accepted", "tool", None, 0),
        ("requesting", "request_started", "tool", None, 0),
        ("requesting-second", "request_started:1", "tool", None, 1),
        ("retry", "attempt_stopped:1", "retry", None, 2),
        (
            "nonretry",
            "attempt_stopped:1",
            "nonretry",
            Some("failed"),
            2,
        ),
        ("registered", "calls_registered:1", "tool", None, 2),
        ("started", "invocation_started:1:3", "tool", None, 2),
        ("terminal", "invocation_finished:1:3", "tool", None, 2),
        ("all-terminal", "invocation_finished:1:4", "tool", None, 2),
        ("continue", "step_closed:1", "tool", None, 2),
        ("oversize", "invocation_finished:1:3", "oversize", None, 2),
        ("finish", "step_closed:2", "tool", Some("completed"), 3),
        ("invalid", "step_closed:1", "invalid", Some("failed"), 2),
    ] {
        let (world, spec) = support::direct_world(
            &format!("r2-crash-{name}"),
            "matrix",
            "[\"observe.workspace\",\"mutate.create\",\"session.todo_write\"]",
            "\n[llm.retry]\nmax_retries = 1\n",
        );
        world.write_project_file("seed.txt", "seed\n").unwrap();
        let budget = world.budget(&spec);
        let mut evidence = support::evidence(&world, &spec);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let batch = |prefix, path| SseScript::Chunks {
                chunks: vec![
                    chunk(json!({"content":format!("{prefix} text")})),
                    chunk(json!({"reasoning_content":format!("{prefix} reason")})),
                    chunk(calls(prefix, path)),
                    end(),
                ],
            };
            let second = match mode {
                "oversize" => {
                    let mut delta = calls("second", "kept.txt");
                    delta["tool_calls"][1]["function"] = json!({"name":"session__todo_write",
                        "arguments":json!({"todos":[{"content":"x".repeat(70_000),"status":"pending"}]}).to_string()});
                    SseScript::Chunks {
                        chunks: vec![
                            chunk(json!({"content":"second text"})),
                            chunk(json!({"reasoning_content":"second reason"})),
                            chunk(delta),
                            end(),
                        ],
                    }
                }
                "nonretry" => SseScript::HttpError {
                    status: 400,
                    body: r#"{"error":{"message":"nonretry"}}"#.into(),
                },
                "retry" => SseScript::HttpError {
                    status: 503,
                    body: r#"{"error":{"message":"retry"}}"#.into(),
                },
                "invalid" => {
                    let mut rejected_end = end();
                    rejected_end["usage"] = json!({
                        "prompt_tokens":10,"completion_tokens":5,"total_tokens":15
                    });
                    SseScript::Chunks {
                        chunks: vec![
                            chunk(json!({"tool_calls":[{
                                "index":0,"id":"outsider","type":"function",
                                "function":{"name":"fetch__web",
                                    "arguments":r#"{"url":"https://example.com"}"#}
                            }]})),
                            rejected_end,
                        ],
                    }
                }
                _ => batch("second", "kept.txt"),
            };
            let script = support::sequence(vec![
                batch("first", "first.txt"),
                second,
                SseScript::plain("complete"),
            ]);
            provenance(
                &world,
                &mut evidence,
                json!({"test":"redb_commit_windows_close_only_and_preserve_terminal_facts",
                "case":name,"barrier":barrier,"script":format!("{script:?}")}),
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
                        args: [
                            "--json",
                            "session",
                            "send",
                            "matrix input",
                            "--agent",
                            "matrix",
                            "--turn-id",
                            TURN,
                        ]
                        .into_iter()
                        .map(String::from)
                        .collect(),
                        timeout: Some(budget.cap_timeout(spec.step_timeout())),
                    },
                )
                .unwrap();
            let marker = wait_for_ack(&ack, budget.cap_timeout(spec.step_timeout()));
            assert_eq!(marker["barrier"], barrier.split(':').next().unwrap());
            assert_eq!(marker["turn_id"], TURN);
            if let Some(step) = barrier.split(':').nth(1) {
                assert_eq!(marker["step"], step.parse::<u32>().unwrap());
            }
            if let Some(part) = barrier.split(':').nth(2) {
                assert_eq!(marker["part"], part.parse::<u32>().unwrap());
            }
            evidence.record_json("marker", &marker).unwrap();
            support::crash::kill(&world, daemon, frontend, &mut evidence, &budget);
            world.unset_env(BARRIER);
            world.unset_env(ACK);
            let sid = marker["session_id"].as_str().unwrap();
            let before = stage(&world, &spec, &mut evidence, sid, TURN);
            assert_eq!(provider.captured_requests().len(), requests);
            assert_eq!(before["summary"]["outcome"].as_str(), closed_outcome);
            let mut observer = Wire::connect(
                &world,
                json!({"operation":"subscribe",
                "target":{"session_id":sid,"turn_id":TURN},"cursor":null}),
            );
            assert_eq!(observer.next()["value"]["view"]["selected_turn"], before);
            drop(observer);
            assert_eq!(
                query(&world, TURN)["selected_turn"],
                before,
                "observation is read-only"
            );
            let prior = before["steps"].as_array().unwrap();
            if requests > 0 {
                assert_eq!(prior[0]["state"]["end"], json!({"end":"continue","node":2}));
                assert_eq!(prior[0]["state"]["slots"].as_array().unwrap().len(), 3);
                assert!(
                    prior[0]["state"]["slots"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|s| s["state"]["outcome"] == "succeeded")
                );
            }
            let slots = prior
                .get(1)
                .and_then(|s| s["state"]["slots"].as_array())
                .cloned()
                .unwrap_or_default();
            if matches!(
                name,
                "registered"
                    | "started"
                    | "terminal"
                    | "all-terminal"
                    | "continue"
                    | "oversize"
                    | "finish"
            ) {
                assert_eq!(slots.len(), 3);
                let states: Vec<_> = slots
                    .iter()
                    .map(|s| s["state"]["state"].as_str().unwrap())
                    .collect();
                assert_eq!(
                    states,
                    match name {
                        "registered" => vec!["registered", "registered", "registered"],
                        "started" => vec!["terminal", "started", "registered"],
                        "terminal" | "oversize" => vec!["terminal", "terminal", "registered"],
                        _ => vec!["terminal"; 3],
                    }
                );
            }
            if name == "retry" {
                assert_eq!(prior[1]["state"]["state"], "retry_ready");
                assert_eq!(prior[1]["attempts"].as_array().unwrap().len(), 1);
            }
            if name == "oversize" {
                assert_eq!(slots[1]["state"]["outcome"], "succeeded");
                assert_eq!(slots[1]["state"]["material"]["kind"], "output_too_large");
                assert!(
                    slots[1]["state"]["material"]["byte_length"]
                        .as_u64()
                        .unwrap()
                        > 65_536
                );
            }
            let installed = world.project_dir().join("kept.txt").exists();
            assert_eq!(
                installed,
                matches!(name, "terminal" | "all-terminal" | "continue" | "finish")
            );
            assert_eq!(world.project_dir().join("first.txt").exists(), requests > 0);
            let tip = (requests > 0).then(|| history_tip(&world));
            read_json(
                &world,
                &spec,
                &mut evidence,
                &["--json", "session", "rewind", sid, "1"],
            );
            let after = stage(&world, &spec, &mut evidence, sid, TURN);
            let steps = after["steps"].as_array().unwrap();
            if closed_outcome.is_some() {
                assert_eq!(before, after);
            } else {
                assert_eq!(after["summary"]["outcome"], "failed");
                assert_eq!(after["summary"]["notice"]["level"], "error");
                let node = if matches!(
                    name,
                    "registered" | "started" | "terminal" | "all-terminal" | "oversize"
                ) {
                    json!(3)
                } else {
                    Value::Null
                };
                // 无论崩溃落在 Started 前后，自动关闭的原因都是进程退出。
                assert_eq!(
                    steps.last().unwrap()["state"]["end"],
                    json!({"end":"stop","node":node,"cause":{"cause":"process_exit"}})
                );
                if name == "accepted" || name == "continue" {
                    assert!(
                        steps.last().unwrap()["attempts"]
                            .as_array()
                            .unwrap()
                            .is_empty()
                    );
                }
                if matches!(name, "requesting" | "requesting-second") {
                    assert_eq!(
                        steps.last().unwrap()["attempts"][0]["end"]["cause"]["cause"],
                        "process_exit"
                    );
                }
                if name == "retry" {
                    assert_eq!(steps[1]["attempts"], prior[1]["attempts"]);
                }
            }
            // 每个已关闭前序 Step、每个已写终态逐字不变；缺失槽位才补齐。
            for (old, new) in prior.iter().zip(steps) {
                if old["state"]["state"] == "closed" {
                    assert_eq!(old, new);
                }
                for (old_slot, new_slot) in old["state"]["slots"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .zip(new["state"]["slots"].as_array().into_iter().flatten())
                {
                    assert_eq!(old_slot["binding"], new_slot["binding"]);
                    assert_eq!(old_slot["invocation_id"], new_slot["invocation_id"]);
                    if old_slot["state"]["state"] == "terminal" {
                        assert_eq!(old_slot, new_slot);
                    } else if old_slot["state"]["state"] == "started" {
                        assert_eq!(new_slot["state"]["outcome"], "indeterminate");
                        assert_eq!(
                            new_slot["state"]["reason"],
                            json!({"kind":"indeterminate","value":"no_result"})
                        );
                    } else {
                        assert_eq!(new_slot["state"]["outcome"], "not_executed");
                        assert!(new_slot["state"]["reason"].is_null());
                    }
                }
            }
            if name == "invalid" {
                assert_eq!(
                    steps[1]["state"]["end"],
                    json!({"end":"stop","node":null,"cause":{"cause":"invalid_batch"}})
                );
                // 首个成功批次 (1,1,2) 与被拒批次 (10,5,15) 均须完整保留；
                // 不相等的分项能捕获丢失或串位，且关闭后崩溃重开仍为 Complete。
                assert_eq!(
                    after["usage"],
                    json!({"kind":"complete","total":{
                        "prompt_tokens":11,"completion_tokens":6,"total_tokens":17
                    }}),
                    "invalid batch must preserve complete usage and each token counter"
                );
            }
            if name == "nonretry" {
                assert_eq!(
                    steps[1]["state"]["end"],
                    json!({"end":"stop","node":null,"cause":{"cause":"provider_error","retryable":false}})
                );
                assert_eq!(steps[1]["attempts"].as_array().unwrap().len(), 1);
            }
            let log = read_json(
                &world,
                &spec,
                &mut evidence,
                &["--json", "session", "log", sid],
            );
            let head = log["nodes"].as_array().unwrap().last().unwrap()["node"]["id"]
                .as_u64()
                .unwrap();
            read_json(
                &world,
                &spec,
                &mut evidence,
                &["--json", "session", "rewind", sid, &head.to_string()],
            );
            let mut observer = Wire::connect(
                &world,
                json!({"operation":"subscribe",
                "target":{"session_id":sid,"turn_id":TURN},"cursor":null}),
            );
            assert_eq!(observer.next()["value"]["view"]["selected_turn"], after);
            let finished = observer.next();
            assert_eq!(finished["value"]["type"], "finished");
            assert_paired_history(&finished["value"]["history"], &after);
            evidence.record_json("paired-history", &finished).unwrap();
            evidence
                .record_json(
                    "state-summary",
                    &json!({"case":name,"before":before,"after":after,
                "provider_requests":requests,"barrier":marker,"history_current":tip}),
                )
                .unwrap();
            if let Some(tip) = tip {
                assert_eq!(history_tip(&world), tip);
                assert_eq!(
                    std::fs::read_to_string(world.project_dir().join("first.txt")).unwrap(),
                    "durable\n"
                );
            }
            if installed {
                assert_eq!(
                    std::fs::read_to_string(world.project_dir().join("kept.txt")).unwrap(),
                    "durable\n"
                );
            }
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
        support::finish_recorded(evidence, result);
    }
}

#[test]
fn persisted_cancel_after_mutation_survives_crash_without_replay() {
    let (world, spec) = support::direct_world(
        "r2-crash-cancelled",
        "cancelled",
        "[\"observe.workspace\",\"mutate.create\"]",
        "",
    );
    world.write_project_file("seed.txt", "seed\n").unwrap();
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (mut provider, gate) = support::wire_gated_fake_llm(
            &world,
            "cancelled",
            support::sequence(vec![
                SseScript::Chunks {
                    chunks: vec![chunk(calls("first", "kept.txt")), end()],
                },
                SseScript::plain("must not arrive"),
            ]),
        );
        provenance(
            &world,
            &mut evidence,
            json!({"test":"persisted_cancel_after_mutation_survives_crash_without_replay","case":"persisted cancellation after mutation","barrier":"attempt_stopped","first":calls("first","kept.txt"),"script":"three calls, then gated plain must not arrive; cancel at request 2"}),
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
        let mut observer = Wire::connect(
            &world,
            json!({"operation":"subscribe","target":{"session_id":sid,"turn_id":TURN},"cursor":null}),
        );
        assert_eq!(observer.next()["value"]["type"], "snapshot");
        let finished = observer.next();
        assert_eq!(finished["value"]["type"], "finished");
        assert_paired_history(&finished["value"]["history"], &closed);
        assert_eq!(
            closed["steps"][0]["state"]["slots"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        evidence.record_json("paired-history", &finished).unwrap();
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
            "durable\n"
        );
        assert_eq!(provider.captured_requests().len(), 2);
        evidence
            .record_json(
                "state-summary",
                &json!({"turn":closed,"history_current":tip,"provider_requests":2}),
            )
            .unwrap();
        evidence.record_captures(provider.captured_requests());
        provider
            .shutdown(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
    }));
    support::finish_recorded(evidence, result);
}

#[test]
fn rewind_commit_survives_sigkill_and_keeps_both_branches() {
    for target in [2, 0] {
        let (world, spec) = support::direct_world(
            &format!("r2-rewind-crash-{target}"),
            "rewind",
            "[\"observe.workspace\",\"mutate.create\"]",
            "",
        );
        world.write_project_file("seed.txt", "seed\n").unwrap();
        let budget = world.budget(&spec);
        let mut evidence = support::evidence(&world, &spec);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let script = support::sequence(vec![
                SseScript::Chunks {
                    chunks: vec![chunk(calls("rewind", "kept.txt")), end()],
                },
                SseScript::plain("original final"),
                SseScript::plain("fork final"),
            ]);
            provenance(
                &world,
                &mut evidence,
                json!({"test":"rewind_commit_survives_sigkill_and_keeps_both_branches","target":target,"script":format!("{script:?}")}),
            );
            let mut provider = support::wire_fake_llm(&world, "rewind", script);
            let ack = world.root().join("rewind-marker.json");
            world.set_env(BARRIER, "rewind_committed");
            world.set_env(ACK, ack.display().to_string());
            let daemon = support::crash::start(&world, &spec, &budget);
            cli(
                &world,
                &spec,
                &mut evidence,
                &[
                    "--json",
                    "session",
                    "send",
                    "original",
                    "--agent",
                    "rewind",
                    "--turn-id",
                    TURN,
                ],
                0,
            );
            let view = query(&world, TURN);
            let sid = view["session"]["session_id"].as_str().unwrap();
            let original = stage(&world, &spec, &mut evidence, sid, TURN);
            let tip = history_tip(&world);
            let target_arg = target.to_string();
            let frontend = support::driver()
                .start(
                    &world,
                    &LatticeCommand {
                        args: ["--json", "session", "rewind", sid, &target_arg]
                            .iter()
                            .map(|s| s.to_string())
                            .collect(),
                        timeout: Some(budget.cap_timeout(spec.step_timeout())),
                    },
                )
                .unwrap();
            let marker = wait_for_ack(&ack, budget.cap_timeout(spec.step_timeout()));
            assert_eq!(marker["barrier"], "rewind_committed");
            assert_eq!(marker["session_id"], sid);
            assert_eq!(marker["current"], target);
            evidence.record_json("marker", &marker).unwrap();
            support::crash::kill(&world, daemon, frontend, &mut evidence, &budget);
            world.unset_env(BARRIER);
            world.unset_env(ACK);
            let reopened = stage(&world, &spec, &mut evidence, sid, TURN);
            assert_eq!(reopened, original);
            let log = read_json(
                &world,
                &spec,
                &mut evidence,
                &["--json", "session", "log", sid],
            );
            assert_eq!(log["session"]["current"], target);
            assert_eq!(log["nodes"].as_array().unwrap().len(), 4);
            let mut observer = Wire::connect(
                &world,
                json!({"operation":"subscribe","target":{"session_id":sid,"turn_id":TURN},"cursor":null}),
            );
            assert_eq!(observer.next()["value"]["type"], "snapshot");
            let finished = observer.next();
            assert_eq!(finished["value"]["type"], "finished");
            assert_eq!(
                finished["value"]["history"]["entries"]
                    .as_array()
                    .unwrap()
                    .len(),
                target as usize
            );
            evidence.record_json("rewound-history", &finished).unwrap();
            assert_eq!(provider.captured_requests().len(), 2);
            cli(
                &world,
                &spec,
                &mut evidence,
                &[
                    "--json",
                    "session",
                    "send",
                    "fork",
                    "--session",
                    sid,
                    "--agent",
                    "rewind",
                ],
                0,
            );
            let fork = read_json(
                &world,
                &spec,
                &mut evidence,
                &["--json", "session", "log", sid],
            );
            assert_eq!(fork["nodes"][4]["node"]["parent"], target);
            assert_eq!(fork["nodes"][4]["node"]["id"], 4);
            read_json(
                &world,
                &spec,
                &mut evidence,
                &["--json", "session", "rewind", sid, "3"],
            );
            assert_eq!(stage(&world, &spec, &mut evidence, sid, TURN), original);
            let mut original_reader = Wire::connect(
                &world,
                json!({"operation":"subscribe","target":{"session_id":sid,"turn_id":TURN},"cursor":null}),
            );
            assert_eq!(original_reader.next()["value"]["type"], "snapshot");
            let original_history = original_reader.next();
            assert_eq!(original_history["value"]["type"], "finished");
            assert_paired_history(&original_history["value"]["history"], &original);
            assert_eq!(
                original["steps"][0]["state"]["slots"]
                    .as_array()
                    .unwrap()
                    .len(),
                3
            );
            evidence
                .record_json("original-paired-history", &original_history)
                .unwrap();
            assert_eq!(history_tip(&world), tip);
            assert_eq!(
                std::fs::read_to_string(world.project_dir().join("kept.txt")).unwrap(),
                "durable\n"
            );
            let captures = provider.captured_requests();
            assert_eq!(captures.len(), 3);
            assert!(
                !captures[2].raw["messages"]
                    .to_string()
                    .contains("original final")
            );
            if target == 0 {
                assert_eq!(captures[2].raw["messages"].as_array().unwrap().len(), 2);
            }
            evidence.record_json("state-summary", &json!({"target":target,"turn":original,"tree":fork,"history_current":tip,"provider_requests":3})).unwrap();
            evidence.record_captures(captures);
            provider
                .shutdown(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
        }));
        support::finish_recorded(evidence, result);
    }
}

#[test]
fn history_read_failure_preserves_live_text_and_summary_then_reconnect_clears_error() {
    use lattice_system_harness::PtyTuiDriver;
    let (world, spec) = support::direct_world("r2-history-fault", "history", "[]", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let chunks = vec![
            chunk(json!({"content":"READ_FAULT_LIVE_BODY"})),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
        ];
        provenance(
            &world,
            &mut evidence,
            json!({"test":"history_read_failure_preserves_live_text_and_summary_then_reconnect_clears_error",
            "chunks":chunks,"read_anchor":2,"fault":"pause all final history reads, then error, then successful reload"}),
        );
        let (mut provider, gate) =
            FakeLlmServer::start_segmented(SseScript::Chunks { chunks }, spec.step_timeout())
                .unwrap();
        world.set_env("OPENAI_COMPATIBLE_BASE_URL", provider.base_url());
        world.set_env("OPENAI_COMPATIBLE_MODEL", "history");
        world.set_env("OPENAI_COMPATIBLE_API_KEY", FAKE_API_KEY);
        let schedule = world.root().join("schedule");
        std::fs::create_dir(&schedule).unwrap();
        std::fs::write(schedule.join("history_read_2.arm"), b"").unwrap();
        std::fs::write(schedule.join("history_read_2.error"), b"").unwrap();
        world.set_env("LATTICE_TEST_SCHEDULE_DIR", schedule.display().to_string());
        let daemon = support::crash::start(&world, &spec, &budget);
        let front = support::driver()
            .start(
                &world,
                &LatticeCommand {
                    args: [
                        "--json",
                        "session",
                        "send",
                        "history fault",
                        "--agent",
                        "history",
                        "--turn-id",
                        TURN,
                    ]
                    .into_iter()
                    .map(String::from)
                    .collect(),
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
        let partial = wait_view(&world, TURN, |v| {
            v["attempt"]["segments"] == json!([{"type":"text","text":"READ_FAULT_LIVE_BODY"}])
        });
        let sid = partial["session"]["session_id"].as_str().unwrap();
        let mut tui = PtyTuiDriver::reconnect(&support::driver(), &world, sid).unwrap();
        let tui_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tui.wait_for_ready(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
            tui.wait_for(budget.cap_timeout(spec.step_timeout()), |text| {
                text.contains("READ_FAULT_LIVE_BODY")
            })
            .unwrap();
            let live = tui
                .redraw_text(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
            assert!(live.contains("READ_FAULT_LIVE_BODY"));
            gate.release(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
            let pause = wait_for_ack(
                &schedule.join("history_read_2.ready"),
                budget.cap_timeout(spec.step_timeout()),
            );
            let closed = query(&world, TURN);
            assert_eq!(closed["selected_turn"]["summary"]["outcome"], "completed");
            tui.wait_for(budget.cap_timeout(spec.step_timeout()), |text| {
                text.contains("completed")
            })
            .unwrap();
            let waiting = tui
                .redraw_text(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
            assert!(
                waiting.contains("READ_FAULT_LIVE_BODY") && waiting.contains("completed"),
                "{waiting}"
            );
            assert_eq!(waiting.matches("READ_FAULT_LIVE_BODY").count(), 1);
            let mut observer = Wire::connect(
                &world,
                json!({"operation":"subscribe","target":{"session_id":sid,"turn_id":TURN},"cursor":null}),
            );
            let snapshot = observer.next();
            assert_eq!(
                snapshot["value"]["view"]["selected_turn"],
                closed["selected_turn"]
            );
            assert!(!schedule.join("history_read_2.release").exists());
            std::fs::write(schedule.join("history_read_2.release"), b"").unwrap();
            let failed = observer.next();
            assert_eq!(failed["response"], "history_error");
            let done = front.wait(None).unwrap();
            evidence.record_command("Completed but final history unavailable", &done);
            assert!(done.is_clean_exit(1), "{}", done.summary());
            let records: Vec<Value> = std::str::from_utf8(&done.stdout)
                .unwrap()
                .lines()
                .map(|s| serde_json::from_str(s).unwrap())
                .collect();
            assert!(records.iter().any(|v| v["type"] == "snapshot"
                && v["view"]["selected_turn"]["summary"]["outcome"] == "completed"));
            assert!(records.iter().any(|v| v["type"] == "error"
                && v["kind"] == "store_unavailable"
                && v["message"] == "injected history read failure"));
            assert!(
                !records
                    .iter()
                    .any(|v| v["type"] == "history" && v["history"]["anchor"]["head"] == 2)
            );
            tui.wait_for(budget.cap_timeout(spec.step_timeout()), |text| {
                text.contains("持久正文不可用")
            })
            .unwrap();
            let errored = tui
                .redraw_text(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
            assert!(
                errored.contains("READ_FAULT_LIVE_BODY") && errored.contains("completed"),
                "{errored}"
            );
            std::fs::remove_file(schedule.join("history_read_2.error")).unwrap();
            tui.send_line("/reconnect").unwrap();
            // 独立公开重读核验原结果未改变；同一 TUI 重载完成后退出报告不能保留当前读取错误。
            let read = cli(
                &world,
                &spec,
                &mut evidence,
                &["--json", "session", "turn", TURN, "--follow"],
                0,
            );
            let history = std::str::from_utf8(&read.stdout)
                .unwrap()
                .lines()
                .map(|s| serde_json::from_str::<Value>(s).unwrap())
                .find(|v| v["type"] == "history")
                .unwrap();
            assert_paired_history(&history["history"], &closed["selected_turn"]);
            let reloaded = tui
                .redraw_text(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
            assert!(
                reloaded.contains("READ_FAULT_LIVE_BODY") && reloaded.contains("completed"),
                "{reloaded}"
            );
            assert_eq!(reloaded.matches("READ_FAULT_LIVE_BODY").count(), 1);
            let exit_start = tui.output().len();
            tui.send_line("/quit").unwrap();
            tui.wait_for_exit(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
            assert!(
                lattice_system_harness::processes_in_pgid(tui.pgid().unwrap())
                    .unwrap()
                    .is_empty()
            );
            let exit_output = String::from_utf8_lossy(&tui.output()[exit_start..]);
            assert!(!exit_output.contains("持久正文不可用："), "{exit_output}");
            assert_eq!(
                query(&world, TURN)["selected_turn"],
                closed["selected_turn"]
            );
            evidence.record_json("read-windows", &json!({"pause":pause,"snapshot":snapshot,"error":failed,
                "live":live,"waiting":waiting,"errored":errored,"reloaded":reloaded,"exit":exit_output,"history":history})).unwrap();
            stage(&world, &spec, &mut evidence, sid, TURN);
            evidence.record_json("state-summary", &json!({"turn":closed["selected_turn"],"provider_requests":1,"history_error_cleared":true})).unwrap();
        }));
        evidence.record_pty(tui.output());
        drop(tui);
        if let Err(error) = tui_result {
            std::panic::resume_unwind(error);
        }
        evidence.record_captures(provider.captured_requests());
        assert_eq!(provider.captured_requests().len(), 1);
        provider
            .shutdown(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        cli(
            &world,
            &spec,
            &mut evidence,
            &["--json", "daemon", "stop"],
            0,
        );
        let stopped = daemon.wait(None).unwrap();
        evidence.record_command("history fault backend shutdown", &stopped);
        assert!(stopped.is_clean_exit(0));
    }));
    support::finish_recorded(evidence, result);
}

#[test]
fn slow_window_and_closed_snapshot_deliver_exact_history() {
    let (world, spec) = support::direct_world(
        "r2-observation-windows",
        "windows",
        "[\"observe.workspace\",\"mutate.create\"]",
        "\n[llm.retry]\nmax_retries = 1\n",
    );
    world.write_project_file("seed.txt", "seed\n").unwrap();
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut chunks: Vec<_> = (0..71)
            .map(|i| {
                chunk(if i % 2 == 0 {
                    json!({"content":format!("text-{i};")})
                } else {
                    json!({"reasoning_content":format!("reason-{i};")})
                })
            })
            .collect();
        chunks.push(json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}));
        let failed = vec![
            chunk(json!({"content":"discarded partial"})),
            json!({"error":{"type":"server_error","message":"retryable after delta"}}),
        ];
        let first = vec![
            chunk(json!({"content":"tools explanation"})),
            chunk(json!({"reasoning_content":"tools reason"})),
            chunk(calls("first", "created.txt")),
            end(),
        ];
        provenance(
            &world,
            &mut evidence,
            json!({"test":"slow_window_and_closed_snapshot_deliver_exact_history",
            "failed":failed,"first":first,"chunks":chunks,"window":64}),
        );
        let (mut provider, gate) = FakeLlmServer::start_segmented(
            support::sequence(vec![
                SseScript::Chunks { chunks: failed },
                SseScript::Chunks { chunks: first },
                SseScript::Chunks { chunks },
            ]),
            budget.cap_timeout(spec.step_timeout()),
        )
        .unwrap();
        world.set_env("OPENAI_COMPATIBLE_BASE_URL", provider.base_url());
        world.set_env("OPENAI_COMPATIBLE_MODEL", "windows");
        world.set_env("OPENAI_COMPATIBLE_API_KEY", FAKE_API_KEY);
        let schedule = world.root().join("schedule");
        std::fs::create_dir(&schedule).unwrap();
        for name in ["subscription", "turn_closed", "invocation_started"] {
            std::fs::write(schedule.join(format!("{name}.arm")), b"").unwrap();
        }
        world.set_env("LATTICE_TEST_SCHEDULE_DIR", schedule.display().to_string());
        let daemon = support::crash::start(&world, &spec, &budget);
        let front = support::driver()
            .start(
                &world,
                &LatticeCommand {
                    args: [
                        "--json",
                        "session",
                        "send",
                        "observe windows",
                        "--agent",
                        "windows",
                        "--turn-id",
                        TURN,
                    ]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
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
        let partial = wait_view(&world, TURN, |v| {
            v["attempt"]["segments"] == json!([{"type":"text","text":"discarded partial"}])
        });
        assert_eq!(partial["attempt"]["key"]["attempt_ordinal"], 0);
        for i in 2..=3 {
            gate.wait_until_stalled(i, budget.cap_timeout(spec.step_timeout()))
                .unwrap();
            gate.release_next(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
        }
        gate.wait_until_stalled(4, budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        let retry = query(&world, TURN);
        assert_eq!(
            retry["attempt"]["key"],
            json!({"turn_id":TURN,"model_step_ordinal":0,"attempt_ordinal":1})
        );
        assert_eq!(retry["attempt"]["segments"], json!([]));
        assert_eq!(
            retry["selected_turn"]["steps"][0]["attempts"][0]["end"]["cause"],
            json!({"cause":"provider_error","retryable":true})
        );
        for i in 4..=8 {
            gate.wait_until_stalled(i, budget.cap_timeout(spec.step_timeout()))
                .unwrap();
            gate.release_next(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
        }
        let tool_pause = wait_for_ack(
            &schedule.join("invocation_started.ready"),
            budget.cap_timeout(spec.step_timeout()),
        );
        let executing = query(&world, TURN);
        assert_eq!(executing["active"]["state"]["state"], "executing");
        assert_eq!(
            executing["attempt"]["segments"],
            json!([{"type":"text","text":"tools explanation"},{"type":"reasoning","text":"tools reason"}])
        );
        let sid = executing["session"]["session_id"].as_str().unwrap();
        let mut reconnect = Wire::connect(
            &world,
            json!({"operation":"subscribe","target":{"session_id":sid,"turn_id":TURN},"cursor":null}),
        );
        let tool_snapshot = reconnect.next();
        assert_eq!(
            tool_snapshot["value"]["view"]["attempt"],
            executing["attempt"]
        );
        assert_eq!(
            tool_snapshot["value"]["view"]["active"],
            executing["active"]
        );
        drop(reconnect);
        evidence.record_json("attempt-and-tool-windows", &json!({"partial":partial,"retry":retry,"tool_pause":tool_pause,"executing":executing,"reconnected":tool_snapshot})).unwrap();
        std::fs::write(schedule.join("invocation_started.release"), b"").unwrap();
        gate.wait_until_stalled(9, budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        let before = query(&world, TURN);
        let sid = before["session"]["session_id"].as_str().unwrap();
        let mut slow = Wire::connect(
            &world,
            json!({"operation":"subscribe","target":{"session_id":sid,"turn_id":TURN},"cursor":before["cursor"]}),
        );
        let paused = wait_for_ack(
            &schedule.join("subscription.ready"),
            budget.cap_timeout(spec.step_timeout()),
        );
        assert_eq!(paused["turn_id"], TURN);
        assert_eq!(paused["sequence"], before["cursor"]["sequence"]);
        assert_eq!(
            before["attempt"]["key"],
            json!({"turn_id":TURN,"model_step_ordinal":1,"attempt_ordinal":0})
        );
        for i in 9..=78 {
            gate.wait_until_stalled(i, budget.cap_timeout(spec.step_timeout()))
                .unwrap();
            gate.release_next(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
        }
        gate.wait_until_stalled(79, budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        let populated = wait_view(&world, TURN, |v| {
            v["attempt"]["segments"]
                .as_array()
                .is_some_and(|s| s.len() == 70)
        });
        assert!(
            populated["cursor"]["sequence"].as_u64().unwrap()
                > paused["sequence"].as_u64().unwrap() + 64
        );
        std::fs::write(schedule.join("subscription.release"), b"").unwrap();
        let reset = slow.next();
        assert_eq!(reset["value"]["type"], "snapshot");
        assert_eq!(reset["value"]["view"]["attempt"], populated["attempt"]);
        evidence
            .record_json(
                "slow-window",
                &json!({"before":before,"pause":paused,"populated":populated,"reset":reset}),
            )
            .unwrap();
        gate.release_next(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        gate.wait_until_stalled(80, budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        let event = slow.next();
        assert_eq!(event["value"]["type"], "event");
        assert_eq!(
            event["value"]["cursor"]["sequence"].as_u64().unwrap(),
            reset["value"]["view"]["cursor"]["sequence"]
                .as_u64()
                .unwrap()
                + 1
        );
        assert_eq!(event["value"]["event"]["content"], "text-70;");
        let mut reconstructed = populated["attempt"]["segments"].as_array().unwrap().clone();
        reconstructed.push(json!({"type":"text","text":"text-70;"}));
        gate.release(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        let closed_pause = wait_for_ack(
            &schedule.join("turn_closed.ready"),
            budget.cap_timeout(spec.step_timeout()),
        );
        assert_eq!(closed_pause["turn_id"], TURN);
        let closed = query(&world, TURN);
        assert!(closed["owned"].as_bool().unwrap());
        assert_eq!(closed["selected_turn"]["summary"]["outcome"], "completed");
        let mut observer = Wire::connect(
            &world,
            json!({"operation":"subscribe","target":{"session_id":sid,"turn_id":TURN},"cursor":null}),
        );
        // Owner 仍在、终态尚未追加：新订阅先得到关闭快照，然后等待终态交付，
        // 不凭关闭快照提前结束。
        assert!(!schedule.join("turn_closed.release").exists());
        let initial = observer.next();
        assert_eq!(initial["value"]["type"], "snapshot");
        assert_eq!(
            initial["value"]["view"]["selected_turn"],
            closed["selected_turn"]
        );
        observer.assert_silent(Duration::from_millis(500));
        stage(&world, &spec, &mut evidence, sid, TURN);
        std::fs::write(schedule.join("turn_closed.release"), b"").unwrap();
        let terminal = observer.next();
        assert_eq!(terminal["value"]["type"], "event");
        assert_eq!(terminal["value"]["event"]["type"], "turn_completed");
        assert_eq!(
            terminal["value"]["cursor"]["sequence"].as_u64().unwrap(),
            initial["value"]["view"]["cursor"]["sequence"]
                .as_u64()
                .unwrap()
                + 1
        );
        let snapshot = observer.next();
        assert_eq!(snapshot["value"]["type"], "snapshot");
        let finished = observer.next();
        assert_eq!(finished["value"]["type"], "finished");
        let entries = finished["value"]["history"]["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 3);
        assert_paired_history(&finished["value"]["history"], &closed["selected_turn"]);
        assert_eq!(
            segments(&entries[2]["content"]["content"]["assistant"]),
            reconstructed
        );
        assert_eq!(
            snapshot["value"]["view"]["selected_turn"],
            closed["selected_turn"]
        );
        evidence.record_json("closed-before-event", &json!({"pause":closed_pause,"terminal":terminal,"snapshot":snapshot,"finished":finished,"resumed_event":event})).unwrap();
        let done = front.wait(None).unwrap();
        evidence.record_command("front finishes after notification release", &done);
        assert!(done.is_clean_exit(0), "{}", done.summary());
        let mut updates = Vec::new();
        loop {
            let update = slow.next();
            let finished = update["value"]["type"] == "finished";
            updates.push(update);
            if finished {
                break;
            }
        }
        assert_eq!(
            updates.last().unwrap()["value"]["history"],
            finished["value"]["history"]
        );
        assert!(
            updates[..updates.len() - 1]
                .iter()
                .any(|u| u["value"]["type"] == "event"
                    && u["value"]["event"]["type"] == "turn_completed"),
            "slow subscriber must receive turn_completed before finished"
        );
        evidence
            .record_json("slow-through-finish", &json!(updates))
            .unwrap();
        evidence.record_json("state-summary", &json!({"turn":closed["selected_turn"],"sequence_delta":populated["cursor"]["sequence"].as_u64().unwrap()-paused["sequence"].as_u64().unwrap(),"daemon":daemon.pgid(),"provider_requests":3})).unwrap();
        let captures = provider.captured_requests();
        assert_eq!(captures.len(), 3);
        assert_eq!(captures[0].raw["messages"], captures[1].raw["messages"]);
        assert!(
            !captures[2].raw["messages"]
                .to_string()
                .contains("discarded partial")
        );
        assert!(
            !finished["value"]["history"]
                .to_string()
                .contains("discarded partial")
        );
        assert_eq!(
            closed["selected_turn"]["steps"][0]["attempts"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        evidence.record_captures(captures);
        provider
            .shutdown(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        cli(
            &world,
            &spec,
            &mut evidence,
            &["--json", "daemon", "stop"],
            0,
        );
        let stopped = daemon.wait(None).unwrap();
        evidence.record_command("scheduled backend shutdown", &stopped);
        assert!(stopped.is_clean_exit(0));
        world.unset_env("LATTICE_TEST_SCHEDULE_DIR");
        // 同一重试/多调用/慢订阅脚本跨 epoch；旧游标不能把新后台当作旧事件续流。
        let reopened = stage(&world, &spec, &mut evidence, sid, TURN);
        assert_eq!(reopened, closed["selected_turn"]);
        let mut after_restart = Wire::connect(
            &world,
            json!({"operation":"subscribe",
            "target":{"session_id":sid,"turn_id":TURN},"cursor":event["value"]["cursor"]}),
        );
        let new_snapshot = after_restart.next();
        assert_eq!(new_snapshot["value"]["type"], "snapshot");
        assert_ne!(
            new_snapshot["value"]["view"]["cursor"]["epoch"],
            event["value"]["cursor"]["epoch"]
        );
        assert!(new_snapshot["value"]["view"]["attempt"].is_null());
        let new_finished = after_restart.next();
        assert_eq!(new_finished["value"]["type"], "finished");
        assert_eq!(
            new_finished["value"]["history"],
            finished["value"]["history"]
        );
        assert_eq!(provider.captured_requests().len(), 3);
        evidence.record_json("epoch-reconnect", &json!({"old_cursor":event["value"]["cursor"],"snapshot":new_snapshot,"finished":new_finished})).unwrap();
    }));
    support::finish_recorded(evidence, result);
}

#[test]
fn cancelled_slot_live_and_crash_share_ordered_closure() {
    for crash in [false, true] {
        let (world, spec) = support::direct_world(
            &format!("r2-cancel-slot-{crash}"),
            "slot",
            "[\"observe.workspace\",\"mutate.create\",\"fetch.web\"]",
            "\n[lancet.capabilities]\nmax_boundary = \"ex\"\n[lancet.network]\nallow_localhost = true\nallowed_hosts = [\"localhost\"]\n",
        );
        world.write_project_file("seed.txt", "seed\n").unwrap();
        let budget = world.budget(&spec);
        let mut evidence = support::evidence(&world, &spec);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!(
                "http://localhost:{}/cancel",
                listener.local_addr().unwrap().port()
            );
            let chunks = vec![
                chunk(json!({"content":"create then fetch", "tool_calls":[
                    {"index":0,"id":"keep","type":"function","function":{"name":"mutate__create","arguments":r#"{"path":"kept.txt","content":"kept\n","create_parents":false}"#}},
                    {"index":1,"id":"fetch","type":"function","function":{"name":"fetch__web","arguments":json!({"url":url}).to_string()}},
                    {"index":2,"id":"tail","type":"function","function":{"name":"mutate__create","arguments":r#"{"path":"never.txt","content":"never","create_parents":false}"#}}
                ]})),
                end(),
            ];
            provenance(
                &world,
                &mut evidence,
                json!({"test":"cancelled_slot_live_and_crash_share_ordered_closure","crash":crash,"first":calls("first","first.txt"),"chunks":chunks,"http":"GET accepted; no response until cancellation"}),
            );
            let mut provider = support::wire_fake_llm(
                &world,
                "slot",
                support::sequence(vec![
                    SseScript::Chunks {
                        chunks: vec![chunk(calls("first", "first.txt")), end()],
                    },
                    SseScript::Chunks { chunks },
                ]),
            );
            let ack = world.root().join("cancel-slot.json");
            if crash {
                world.set_env(BARRIER, "invocation_finished:1:2");
                world.set_env(ACK, ack.display().to_string());
            }
            let daemon = support::crash::start(&world, &spec, &budget);
            let front = support::driver()
                .start(
                    &world,
                    &LatticeCommand {
                        args: [
                            "--json",
                            "session",
                            "send",
                            "cancel second call",
                            "--agent",
                            "slot",
                            "--turn-id",
                            TURN,
                        ]
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                        timeout: Some(budget.cap_timeout(spec.step_timeout())),
                    },
                )
                .unwrap();
            let deadline = Instant::now() + budget.cap_timeout(spec.step_timeout());
            let mut http = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "fetch did not reach real HTTP listener"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("HTTP accept: {error}"),
                }
            };
            http.set_read_timeout(Some(budget.cap_timeout(spec.step_timeout())))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                http.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                assert!(request.len() <= 16384);
            }
            assert!(request.starts_with(b"GET /cancel HTTP/1.1\r\n"));
            let running = query(&world, TURN);
            let sid = running["session"]["session_id"].as_str().unwrap();
            let tip = history_tip(&world);
            assert_eq!(
                running["selected_turn"]["steps"][1]["state"]["slots"][0]["state"]["outcome"],
                "succeeded"
            );
            let cancel = call(
                &world,
                json!({"operation":"cancel","target":{"session_id":sid,"turn_id":TURN}}),
            );
            assert_eq!(cancel["value"]["owned"], true);
            evidence.record_json("cancel-taken", &cancel).unwrap();
            if crash {
                let marker = wait_for_ack(&ack, budget.cap_timeout(spec.step_timeout()));
                assert_eq!(marker["barrier"], "invocation_finished");
                assert_eq!(marker["part"], 2);
                assert_eq!(marker["turn_id"], TURN);
                evidence.record_json("marker", &marker).unwrap();
                support::crash::kill(&world, daemon, front, &mut evidence, &budget);
                world.unset_env(BARRIER);
                world.unset_env(ACK);
                let orphan = stage(&world, &spec, &mut evidence, sid, TURN);
                assert!(orphan["summary"].is_null());
                assert_eq!(
                    orphan["steps"][1]["state"]["slots"][1]["state"]["reason"],
                    json!({"kind":"indeterminate","value":"cancelled"})
                );
                assert_eq!(
                    orphan["steps"][1]["state"]["slots"][2]["state"]["state"],
                    "registered"
                );
                read_json(
                    &world,
                    &spec,
                    &mut evidence,
                    &["--json", "session", "rewind", sid, "3"],
                );
            } else {
                let done = front.wait(None).unwrap();
                evidence.record_command("live cancellation", &done);
                assert!(done.is_clean_exit(2), "{}", done.summary());
                cli(
                    &world,
                    &spec,
                    &mut evidence,
                    &["--json", "daemon", "stop"],
                    0,
                );
                let stopped = daemon.wait(None).unwrap();
                evidence.record_command("live backend stop", &stopped);
                assert!(stopped.is_clean_exit(0));
            }
            let closed = stage(&world, &spec, &mut evidence, sid, TURN);
            assert_eq!(closed["summary"]["outcome"], "cancelled");
            assert_eq!(closed["summary"]["notice"]["level"], "info");
            assert_eq!(closed["steps"][0], running["selected_turn"]["steps"][0]);
            let state = &closed["steps"][1]["state"];
            assert_eq!(
                state["end"],
                json!({"end":"stop","node":3,"cause":{"cause":"cancelled"}})
            );
            let slots = state["slots"].as_array().unwrap();
            assert_eq!(slots.len(), 3);
            assert_eq!(
                slots[0],
                running["selected_turn"]["steps"][1]["state"]["slots"][0]
            );
            assert_eq!(slots[1]["state"]["outcome"], "indeterminate");
            assert_eq!(
                slots[1]["state"]["reason"],
                json!({"kind":"indeterminate","value":"cancelled"})
            );
            assert_eq!(slots[2]["state"]["outcome"], "not_executed");
            assert!(slots[2]["state"]["reason"].is_null());
            let mut observer = Wire::connect(
                &world,
                json!({"operation":"subscribe","target":{"session_id":sid,"turn_id":TURN},"cursor":null}),
            );
            assert_eq!(observer.next()["value"]["type"], "snapshot");
            let finished = observer.next();
            assert_eq!(finished["value"]["type"], "finished");
            assert_paired_history(&finished["value"]["history"], &closed);
            assert_eq!(
                finished["value"]["history"]["entries"]
                    .as_array()
                    .unwrap()
                    .len(),
                3
            );
            evidence.record_json("paired-history", &finished).unwrap();
            assert_eq!(history_tip(&world), tip);
            assert!(!world.project_dir().join("never.txt").exists());
            assert_eq!(
                std::fs::read_to_string(world.project_dir().join("kept.txt")).unwrap(),
                "kept\n"
            );
            assert_eq!(
                std::fs::read_to_string(world.project_dir().join("first.txt")).unwrap(),
                "durable\n"
            );
            assert_eq!(provider.captured_requests().len(), 2);
            assert!(
                matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock)
            );
            evidence.record_json("state-summary", &json!({"turn":closed,"history_current":tip,"http_requests":1,"provider_requests":2,"crash":crash})).unwrap();
            evidence.record_captures(provider.captured_requests());
            provider
                .shutdown(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
        }));
        support::finish_recorded(evidence, result);
    }
}

#[test]
fn dispatch_deadline_tail_is_atomic_and_survives_crash() {
    let (world, spec) = support::direct_world(
        "r2-dispatch-deadline",
        "deadline",
        "[\"observe.workspace\",\"mutate.create\"]",
        "\n[llm]\ntimeout_secs = 2\n",
    );
    world.write_project_file("seed.txt", "seed\n").unwrap();
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let chunks = vec![
            chunk(json!({"tool_calls":[
                {"index":0,"id":"first","type":"function","function":{"name":"mutate__create","arguments":r#"{"path":"first.txt","content":"first","create_parents":false}"#}},
                {"index":1,"id":"second","type":"function","function":{"name":"mutate__create","arguments":r#"{"path":"second.txt","content":"second","create_parents":false}"#}}
            ]})),
            end(),
        ];
        provenance(
            &world,
            &mut evidence,
            json!({"test":"dispatch_deadline_tail_is_atomic_and_survives_crash","first":calls("prefix","prefix.txt"),"chunks":chunks,"timeout_secs":2,"pause_after":"calls_registered at step1","barrier":"step_closed:1"}),
        );
        let (mut provider, gate) = support::wire_gated_fake_llm(
            &world,
            "deadline",
            support::sequence(vec![
                SseScript::Chunks {
                    chunks: vec![chunk(calls("prefix", "prefix.txt")), end()],
                },
                SseScript::Chunks { chunks },
            ]),
        );
        let schedule = world.root().join("schedule");
        std::fs::create_dir(&schedule).unwrap();
        world.set_env("LATTICE_TEST_SCHEDULE_DIR", schedule.display().to_string());
        let ack = world.root().join("deadline.json");
        world.set_env(BARRIER, "step_closed:1");
        world.set_env(ACK, ack.display().to_string());
        let daemon = support::crash::start(&world, &spec, &budget);
        let front = support::driver()
            .start(
                &world,
                &LatticeCommand {
                    args: [
                        "--json",
                        "session",
                        "send",
                        "expire before dispatch",
                        "--agent",
                        "deadline",
                        "--turn-id",
                        TURN,
                    ]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
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
        let first = query(&world, TURN);
        assert_eq!(
            first["selected_turn"]["steps"][0]["state"]["end"],
            json!({"end":"continue","node":2})
        );
        let tip = history_tip(&world);
        std::fs::write(schedule.join("calls_registered.arm"), b"").unwrap();
        gate.release_next(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        let paused = wait_for_ack(
            &schedule.join("calls_registered.ready"),
            budget.cap_timeout(spec.step_timeout()),
        );
        assert_eq!(paused["turn_id"], TURN);
        let before = query(&world, TURN);
        let sid = before["session"]["session_id"].as_str().unwrap();
        assert!(
            before["selected_turn"]["steps"][1]["state"]["slots"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s["state"]["state"] == "registered")
        );
        // 从已命中的登记点再等满冻结预算，确定 deadline 已过；不是用耗时猜并发。
        std::thread::sleep(Duration::from_secs(2));
        std::fs::write(schedule.join("calls_registered.release"), b"").unwrap();
        let marker = wait_for_ack(&ack, budget.cap_timeout(spec.step_timeout()));
        assert_eq!(marker["barrier"], "step_closed");
        assert_eq!(marker["turn_id"], TURN);
        evidence
            .record_json(
                "windows",
                &json!({"pause":paused,"before":before,"marker":marker}),
            )
            .unwrap();
        support::crash::kill(&world, daemon, front, &mut evidence, &budget);
        world.unset_env(BARRIER);
        world.unset_env(ACK);
        world.unset_env("LATTICE_TEST_SCHEDULE_DIR");
        let closed = stage(&world, &spec, &mut evidence, sid, TURN);
        assert_eq!(closed["summary"]["outcome"], "failed");
        assert_eq!(
            closed["steps"][1]["state"]["end"],
            json!({"end":"stop","node":3,"cause":{"cause":"deadline"}})
        );
        assert_eq!(closed["steps"][0], first["selected_turn"]["steps"][0]);
        let slots = closed["steps"][1]["state"]["slots"].as_array().unwrap();
        assert_eq!(slots.len(), 2);
        for slot in slots {
            assert_eq!(slot["state"]["outcome"], "not_executed");
            assert!(slot["state"]["reason"].is_null());
        }
        let mut observer = Wire::connect(
            &world,
            json!({"operation":"subscribe","target":{"session_id":sid,"turn_id":TURN},"cursor":null}),
        );
        assert_eq!(observer.next()["value"]["type"], "snapshot");
        let finished = observer.next();
        assert_eq!(finished["value"]["type"], "finished");
        assert_paired_history(&finished["value"]["history"], &closed);
        evidence.record_json("paired-history", &finished).unwrap();
        assert_eq!(history_tip(&world), tip);
        assert_eq!(
            std::fs::read_to_string(world.project_dir().join("prefix.txt")).unwrap(),
            "durable\n"
        );
        read_json(
            &world,
            &spec,
            &mut evidence,
            &["--json", "session", "rewind", sid, "0"],
        );
        assert_eq!(stage(&world, &spec, &mut evidence, sid, TURN), closed);
        assert!(!world.project_dir().join("first.txt").exists());
        assert!(!world.project_dir().join("second.txt").exists());
        assert_eq!(provider.captured_requests().len(), 2);
        evidence
            .record_json(
                "state-summary",
                &json!({"turn":closed,"provider_requests":2,"tail_files_created":false,"history_current":tip}),
            )
            .unwrap();
        evidence.record_captures(provider.captured_requests());
        provider
            .shutdown(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
    }));
    support::finish_recorded(evidence, result);
}

#[test]
fn closure_commit_fault_stops_other_sessions_until_real_reopen() {
    for point in ["calls_registered", "step_closed"] {
        for (fault, missing_on_reopen) in [
            ("poisoned", false),
            ("unknown_before", false),
            ("unknown_after", false),
            ("unknown_before", true),
        ] {
            let name = format!("store-fault-{point}-{fault}-{missing_on_reopen}");
            let (world, spec) = support::direct_world(&name, "fault", "[\"mutate.create\"]", "");
            let budget = world.budget(&spec);
            let mut evidence = support::evidence(&world, &spec);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let script = SseScript::Sequence {
                    scripts: vec![
                        support::tool_call(
                            "kept",
                            "mutate.create",
                            r#"{"path":"kept.txt","content":"kept\n","create_parents":false}"#,
                        ),
                        SseScript::plain("only the new Turn may reach this response"),
                    ],
                };
                provenance(
                    &world,
                    &mut evidence,
                    json!({
                        "test":"closure_commit_fault_stops_other_sessions_until_real_reopen",
                        "point":point,"fault":fault,"missing_on_reopen":missing_on_reopen,"script":format!("{script:?}")
                    }),
                );
                let mut provider = support::wire_fake_llm(&world, "fault", script);
                let ack = world.root().join("crash.json");
                world.set_env(BARRIER, "request_started");
                world.set_env(ACK, ack.display().to_string());
                let daemon = support::crash::start(&world, &spec, &budget);
                let start = |tid: &str| {
                    support::driver()
                        .start(
                            &world,
                            &LatticeCommand {
                                args: [
                                    "--json",
                                    "session",
                                    "send",
                                    "fault input",
                                    "--agent",
                                    "fault",
                                    "--turn-id",
                                    tid,
                                ]
                                .iter()
                                .map(|s| s.to_string())
                                .collect(),
                                timeout: Some(budget.cap_timeout(spec.step_timeout())),
                            },
                        )
                        .unwrap()
                };
                let front = start(TURN);
                let crashed = wait_for_ack(&ack, budget.cap_timeout(spec.step_timeout()));
                assert_eq!(crashed["turn_id"], TURN);
                assert_eq!(crashed["barrier"], "request_started");
                support::crash::kill(&world, daemon, front, &mut evidence, &budget);
                world.unset_env(BARRIER);
                world.unset_env(ACK);
                assert!(provider.captured_requests().is_empty());
                let sid = crashed["session_id"].as_str().unwrap();
                let schedule = world.root().join("schedule");
                std::fs::create_dir(&schedule).unwrap();
                for gate in [point, "store_reopen"] {
                    std::fs::write(schedule.join(format!("{gate}.arm")), b"").unwrap();
                }
                world.set_env("LATTICE_TEST_SCHEDULE_DIR", schedule.display().to_string());
                let mut daemon = support::crash::start(&world, &spec, &budget);
                let first_before = stage(&world, &spec, &mut evidence, sid, TURN);
                assert!(first_before["summary"].is_null());
                let other = start(OTHER);
                let paused = wait_for_ack(
                    &schedule.join(format!("{point}.ready")),
                    budget.cap_timeout(spec.step_timeout()),
                );
                assert_eq!(paused["turn_id"], OTHER);
                let second_before = query(&world, OTHER);
                let sid2 = second_before["session"]["session_id"].as_str().unwrap();
                assert_ne!(sid, sid2);
                stage(&world, &spec, &mut evidence, sid2, OTHER);
                assert_eq!(provider.captured_requests().len(), 1);
                let installed = point == "step_closed";
                assert_eq!(world.project_dir().join("kept.txt").exists(), installed);
                let tip = installed.then(|| history_tip(&world));
                let competing = call(
                    &world,
                    json!({"operation":"submit","request":{"session_id":sid2,
                    "turn_id":"00000000-0000-4000-8000-000000000004","workspace":world.project_dir(),
                    "input":"same Session must stay busy","agent":null},"configuration":null}),
                );
                let busy_rewind = call(
                    &world,
                    json!({"operation":"rewind","session_id":sid2,"target":0,"workspace":world.project_dir()}),
                );
                assert_eq!(competing["value"]["kind"], "busy");
                assert_eq!(busy_rewind["value"]["kind"], "busy");
                if installed {
                    assert_eq!(
                        second_before["selected_turn"]["steps"][0]["state"]["end"]["end"],
                        "continue"
                    );
                    assert_eq!(
                        second_before["selected_turn"]["steps"]
                            .as_array()
                            .unwrap()
                            .len(),
                        1
                    );
                }
                evidence
                    .record_json(
                        "whole-turn-busy",
                        &json!({"snapshot":second_before,"submit":competing,"rewind":busy_rewind}),
                    )
                    .unwrap();
                // 故障只包围实际 closure 事务；此前的第二 Session 已在可核验停点。
                std::fs::write(schedule.join("auto_close.fault"), fault).unwrap();
                let refused = call(
                    &world,
                    json!({"operation":"rewind","session_id":sid,"target":0,"workspace":world.project_dir()}),
                );
                evidence.record_json("closure-error", &refused).unwrap();
                assert_eq!(refused["response"], "error", "closure must fail: {refused}");
                assert_eq!(refused["value"]["kind"], "recovery_required");
                let injected = wait_for_ack(
                    &schedule.join("auto_close.fault-hit"),
                    budget.cap_timeout(spec.step_timeout()),
                );
                assert_eq!(injected["fault"], fault);
                assert!(
                    !schedule.join("store_reopen.ready").exists(),
                    "second Owner still owns old generation"
                );
                let blocked_query = call(
                    &world,
                    json!({"operation":"query","session_id":sid,"turn_id":TURN}),
                );
                assert_eq!(blocked_query["response"], "error");
                // 新 Session、新 Turn 及当前位置移动均不得趁全库停写逃逸。
                let third = "00000000-0000-4000-8000-000000000003";
                let new_input = json!({"operation":"submit","request":{"session_id":null,"turn_id":third,
                    "workspace":world.project_dir(),"input":"must not be accepted","agent":"fault"},"configuration":null});
                let refused_input = call(&world, new_input.clone());
                assert_eq!(refused_input["response"], "error");
                let refused_move = call(
                    &world,
                    json!({"operation":"rewind","session_id":sid,"target":0,"workspace":world.project_dir()}),
                );
                assert_eq!(refused_move["response"], "error");
                evidence
                    .record_json(
                        "quiescing",
                        &json!({"second":second_before,"pause":paused,"injection":injected,
                    "query":blocked_query,"new_input":refused_input,"rewind":refused_move}),
                    )
                    .unwrap();
                std::fs::write(schedule.join(format!("{point}.release")), b"").unwrap();
                let drained = wait_for_ack(
                    &schedule.join("store_reopen.ready"),
                    budget.cap_timeout(spec.step_timeout()),
                );
                assert_eq!(drained["generation"], 0);
                assert_eq!(
                    provider.captured_requests().len(),
                    1,
                    "other Session must not start next provider"
                );
                assert_eq!(
                    world.project_dir().join("kept.txt").exists(),
                    installed,
                    "other Session must not dispatch"
                );
                if let Some(tip) = &tip {
                    assert_eq!(&history_tip(&world), tip);
                }
                let still_blocked = call(&world, new_input);
                assert_eq!(still_blocked["response"], "error");
                let database = world.session_store_dir().join("lattice.redb");
                let saved = world
                    .session_store_dir()
                    .join("temporarily-unavailable.redb");
                let inode =
                    std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&database).unwrap());
                if missing_on_reopen {
                    std::fs::rename(&database, &saved).unwrap();
                }
                std::fs::write(schedule.join("store_reopen.release"), b"").unwrap();
                let deadline = Instant::now() + budget.cap_timeout(spec.step_timeout());
                if missing_on_reopen {
                    loop {
                        let response = call(
                            &world,
                            json!({"operation":"query","session_id":sid,"turn_id":TURN}),
                        );
                        if response["value"]["store"]["type"] == "not_initialized" {
                            evidence.record_json("reopen-failure", &response).unwrap();
                            break;
                        }
                        assert!(
                            Instant::now() < deadline,
                            "missing-file reopen failure not observed: {response}"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    let refused = cli(
                        &world,
                        &spec,
                        &mut evidence,
                        &[
                            "--json",
                            "session",
                            "send",
                            "no replacement database",
                            "--agent",
                            "fault",
                        ],
                        1,
                    );
                    assert!(
                        !String::from_utf8_lossy(&refused.stdout).contains("\"kind\":\"accepted\"")
                    );
                    assert!(
                        !database.exists(),
                        "failed reopen must not create a replacement"
                    );
                    std::fs::rename(&saved, &database).unwrap();
                    assert_eq!(
                        std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&database).unwrap()),
                        inode
                    );
                    let unavailable = call(
                        &world,
                        json!({"operation":"query","session_id":sid,"turn_id":TURN}),
                    );
                    assert_eq!(
                        unavailable["value"]["store"]["type"], "not_initialized",
                        "must not silently retry unavailable generation"
                    );
                    cli(
                        &world,
                        &spec,
                        &mut evidence,
                        &["--json", "daemon", "stop"],
                        1,
                    );
                    let stopped = daemon.wait(None).unwrap();
                    evidence.record_command(
                        "unavailable backend stops without creating a database",
                        &stopped,
                    );
                    assert!(stopped.is_clean_exit(0));
                    daemon = support::crash::start(&world, &spec, &budget);
                }
                loop {
                    let response = call(
                        &world,
                        json!({"operation":"query","session_id":sid,"turn_id":TURN}),
                    );
                    if response["response"] == "view" {
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "strict reopen did not finish: {response}"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                let first_after = stage(&world, &spec, &mut evidence, sid, TURN);
                assert_eq!(first_after["summary"].is_object(), fault == "unknown_after");
                if fault != "unknown_after" {
                    assert_eq!(first_after, first_before);
                }
                assert_eq!(
                    query(&world, TURN)["session"]["current"],
                    1,
                    "failed rewind must not move current"
                );
                let second_after = stage(&world, &spec, &mut evidence, sid2, OTHER);
                assert!(
                    second_after["summary"].is_null(),
                    "reopen must not auto-close other Session"
                );
                assert_eq!(second_after, second_before["selected_turn"]);
                assert_eq!(
                    read_json(&world, &spec, &mut evidence, &["--json", "session", "list"])
                        .as_array()
                        .unwrap()
                        .len(),
                    2
                );
                let other_exit = other.wait(None).unwrap();
                evidence.record_command("other Session stops on global store failure", &other_exit);
                assert!(other_exit.is_clean_exit(1), "{}", other_exit.summary());
                for (s, t) in [(sid, TURN), (sid2, OTHER)] {
                    read_json(
                        &world,
                        &spec,
                        &mut evidence,
                        &["--json", "session", "rewind", s, "0"],
                    );
                    let closed = stage(&world, &spec, &mut evidence, s, t);
                    assert_eq!(closed["summary"]["outcome"], "failed");
                    let steps = closed["steps"].as_array().unwrap();
                    assert_eq!(
                        steps.last().unwrap()["state"]["end"]["cause"]["cause"],
                        "process_exit"
                    );
                    assert!(
                        steps[..steps.len() - 1]
                            .iter()
                            .all(|step| step["state"]["end"]["end"] == "continue")
                    );
                    if t == OTHER && !installed {
                        assert_eq!(
                            steps[0]["state"]["slots"][0]["state"]["outcome"],
                            "not_executed"
                        );
                    }
                }
                assert_eq!(provider.captured_requests().len(), 1);
                if let Some(tip) = &tip {
                    assert_eq!(&history_tip(&world), tip);
                }
                evidence.record_json("state-summary", &json!({"fault":fault,"point":point,"drained":drained,
                    "first_before":first_before,"first_after":first_after,"second_after":second_after,
                    "provider_requests":1,"installed":installed,"backend":daemon.pgid()})).unwrap();
                evidence.record_captures(provider.captured_requests());
                provider
                    .shutdown(budget.cap_timeout(spec.step_timeout()))
                    .unwrap();
                let stop = cli(
                    &world,
                    &spec,
                    &mut evidence,
                    &["--json", "daemon", "stop"],
                    i32::from(!missing_on_reopen),
                );
                let status: Value = serde_json::from_slice(&stop.stdout).unwrap();
                assert_eq!(status["owners"], 0);
                assert_eq!(
                    status["unconfirmed_operations"],
                    if missing_on_reopen { 0 } else { 3 }
                );
                let stopped = daemon.wait(None).unwrap();
                evidence.record_command("fault backend shutdown", &stopped);
                assert!(stopped.is_clean_exit(0));
            }));
            support::finish_recorded(evidence, result);
        }
    }
}

#[test]
fn memory_only_cancel_crash_is_process_exit_and_next_send_runs_once() {
    let (world, spec) = support::direct_world("memory-cancel-crash", "cancel", "[]", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        provenance(
            &world,
            &mut evidence,
            json!({"test":"memory_only_cancel_crash_is_process_exit_and_next_send_runs_once",
            "pause":"request_started","cancel":"true before stopped attempt is durable","next":"new input"}),
        );
        let mut provider = support::wire_fake_llm(&world, "cancel", SseScript::plain("new reply"));
        let schedule = world.root().join("schedule");
        std::fs::create_dir(&schedule).unwrap();
        std::fs::write(schedule.join("request_started.arm"), b"").unwrap();
        world.set_env("LATTICE_TEST_SCHEDULE_DIR", schedule.display().to_string());
        let ack = world.root().join("crash.json");
        world.set_env(BARRIER, "request_started");
        world.set_env(ACK, ack.display().to_string());
        let daemon = support::crash::start(&world, &spec, &budget);
        let front = support::driver()
            .start(
                &world,
                &LatticeCommand {
                    args: [
                        "--json",
                        "session",
                        "send",
                        "cancel input",
                        "--agent",
                        "cancel",
                        "--turn-id",
                        TURN,
                    ]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
                    timeout: Some(budget.cap_timeout(spec.step_timeout())),
                },
            )
            .unwrap();
        let pause = wait_for_ack(
            &schedule.join("request_started.ready"),
            budget.cap_timeout(spec.step_timeout()),
        );
        assert_eq!(pause["turn_id"], TURN);
        let before = query(&world, TURN);
        let sid = before["session"]["session_id"].as_str().unwrap();
        let taken = call(
            &world,
            json!({"operation":"cancel","target":{"session_id":sid,"turn_id":TURN}}),
        );
        assert_eq!(
            taken,
            json!({"response":"cancelled","value":{"owned":true}})
        );
        assert_eq!(
            query(&world, TURN)["selected_turn"],
            before["selected_turn"]
        );
        std::fs::write(schedule.join("request_started.release"), b"").unwrap();
        let marker = wait_for_ack(&ack, budget.cap_timeout(spec.step_timeout()));
        assert_eq!(marker["turn_id"], TURN);
        evidence
            .record_json(
                "memory-only-cancel",
                &json!({"before":before,"pause":pause,"taken":taken,"marker":marker}),
            )
            .unwrap();
        support::crash::kill(&world, daemon, front, &mut evidence, &budget);
        world.unset_env(BARRIER);
        world.unset_env(ACK);
        world.unset_env("LATTICE_TEST_SCHEDULE_DIR");
        let reopened = stage(&world, &spec, &mut evidence, sid, TURN);
        assert_eq!(reopened, before["selected_turn"]);
        assert!(reopened["summary"].is_null());
        assert!(provider.captured_requests().is_empty());
        cli(
            &world,
            &spec,
            &mut evidence,
            &[
                "--json",
                "session",
                "send",
                "new input",
                "--session",
                sid,
                "--turn-id",
                OTHER,
            ],
            0,
        );
        let old = stage(&world, &spec, &mut evidence, sid, TURN);
        assert_eq!(old["summary"]["outcome"], "failed");
        assert_eq!(old["summary"]["notice"]["level"], "error");
        assert_eq!(
            old["steps"][0]["state"]["end"],
            json!({"end":"stop","node":null,"cause":{"cause":"process_exit"}})
        );
        assert_eq!(
            old["steps"][0]["attempts"][0]["end"]["cause"]["cause"],
            "process_exit"
        );
        let new = stage(&world, &spec, &mut evidence, sid, OTHER);
        assert_eq!(new["summary"]["outcome"], "completed");
        let captures = provider.captured_requests();
        assert_eq!(captures.len(), 1);
        let messages = &captures[0].raw["messages"];
        let users: Vec<_> = messages
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "user")
            .map(|m| m["content"].clone())
            .collect();
        assert_eq!(users, vec![json!("cancel input"), json!("new input")]);
        let tree = read_json(
            &world,
            &spec,
            &mut evidence,
            &["--json", "session", "log", sid],
        );
        assert_eq!(tree["session"]["current"], 3);
        evidence
            .record_json(
                "state-summary",
                &json!({"old":old,"new":new,"tree":tree,"provider_requests":1}),
            )
            .unwrap();
        evidence.record_captures(captures);
        provider
            .shutdown(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
    }));
    support::finish_recorded(evidence, result);
}
