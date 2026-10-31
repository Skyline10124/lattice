// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 真实后台提交后 SIGSTOP/SIGKILL；只用 CLI/公开 IPC 读取会话事实。
mod support;

use lattice_system_harness::{
    CommandOutcome, FAKE_API_KEY, FakeLlmServer, LatticeCommand, ScenarioEvidence, ScenarioSpec,
    ScenarioWorld, Segment, SegmentKind, SseScript, StepEnd, StepProgress, ViewState,
    merge_segments,
};
use serde_json::{Value, json};
use std::{
    io::Read,
    path::Path,
    time::{Duration, Instant},
};
use support::{Watcher, Wire, ipc, parse_send_stdout, submit_request, turn_closed, turn_lookup};

/// 直接经 IPC 提交时由测试充当客户端生成的身份。
const TURN: &str = "00000000-0000-4000-8000-000000000001";
const OTHER: &str = "00000000-0000-4000-8000-000000000002";
const THIRD: &str = "00000000-0000-4000-8000-000000000003";
const SESSION: &str = "5e550000000000000000000000000001";
const BARRIER: &str = "LATTICE_TEST_TURN_BARRIER";
const ACK: &str = "LATTICE_TEST_TURN_BARRIER_ACK";

fn timeout(world: &ScenarioWorld, spec: &ScenarioSpec) -> Duration {
    world.budget(spec).cap_timeout(spec.step_timeout())
}
/// 一段观察等待的绝对截止时间。
fn deadline(world: &ScenarioWorld, spec: &ScenarioSpec) -> Instant {
    Instant::now() + timeout(world, spec)
}
/// 新开一次 `watch` 并读完开头的会话头与最近几轮。
fn watch(world: &ScenarioWorld, spec: &ScenarioSpec, sid: &str) -> Watcher {
    let mut watcher = Watcher::open(world, timeout(world, spec), sid);
    watcher.initial();
    watcher
}
/// 按 TurnId 查询已接受的一轮，返回 TurnDetail。
fn accepted_detail(world: &ScenarioWorld, spec: &ScenarioSpec, tid: &str) -> Value {
    let response = turn_lookup(world, timeout(world, spec), tid);
    assert_eq!(response["response"], "turn", "{response}");
    assert_eq!(response["value"]["status"], "accepted", "{response}");
    response["value"]["turn"].clone()
}
fn cancel(world: &ScenarioWorld, spec: &ScenarioSpec, tid: &str) -> Value {
    ipc(
        world,
        timeout(world, spec),
        json!({"operation":"cancel","turn_id":tid}),
    )
}
/// CLI 生成的身份：从会话列表中唯一一个未关闭的最后一轮读出。
fn running_turn(
    world: &ScenarioWorld,
    spec: &ScenarioSpec,
    evidence: &mut ScenarioEvidence,
) -> (String, String) {
    let list = read_json(world, spec, evidence, &["--json", "session", "list"]);
    let running: Vec<_> = list
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| {
            Some((
                row["session_id"].as_str()?.to_owned(),
                row["unclosed_turn"].as_str()?.to_owned(),
            ))
        })
        .collect();
    assert_eq!(running.len(), 1, "{list}");
    running.into_iter().next().unwrap()
}
/// 已知会话中未关闭的最后一轮。
fn unclosed_turn(
    world: &ScenarioWorld,
    spec: &ScenarioSpec,
    evidence: &mut ScenarioEvidence,
    sid: &str,
) -> String {
    read_json(world, spec, evidence, &["--json", "session", "log", sid])["unclosed_turn"]
        .as_str()
        .expect("unclosed turn")
        .to_owned()
}
/// 观察者持有的某个 Step 条目的进度。
fn step_progress(view: &ViewState, node: u64) -> Option<&StepProgress> {
    view.entries
        .get(&node)
        .and_then(|entry| entry.step())
        .map(|step| &step.progress)
}
fn segment(kind: SegmentKind, text: &str) -> Segment {
    Segment {
        kind,
        text: text.into(),
    }
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
        {"index":0,"id":format!("{prefix}-read"),"type":"function","function":{"name":"observe__workspace","arguments":"{\"request\":{\"mode\":\"read_file\",\"path\":\"seed.txt\"}}"}},
        {"index":1,"id":format!("{prefix}-write"),"type":"function","function":{"name":"mutate__create","arguments":json!({"path":path,"content":"durable\n","create_parents":false}).to_string()}},
        {"index":2,"id":format!("{prefix}-tail"),"type":"function","function":{"name":"observe__workspace","arguments":"{\"request\":{\"mode\":\"read_file\",\"path\":\"seed.txt\"}}"}}
    ]})
}
fn end() -> Value {
    json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}})
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
fn cross_project_operator_lifecycle_cursor_and_payload_isolation() {
    use lattice_system_harness::{RunningCommand, SessionSendDriver};
    use std::collections::{BTreeMap, BTreeSet};
    let (world, spec) = support::direct_world(
        "operator-global-matrix",
        "operator",
        "[\"observe__workspace\",\"session__todo_write\",\"fetch__web\"]",
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
                    "session__todo_write".into(),
                    json!({"todos":[{"content":format!("private-small-{i}"),"status":"pending"}]})
                        .to_string(),
                )
            })
            .collect::<Vec<_>>();
        batch.extend([
            (
                "large".into(),
                "session__todo_write".into(),
                json!({"todos":[{"content":"private-large-".repeat(6000),"status":"pending"}]})
                    .to_string(),
            ),
            (
                "bad-tls".into(),
                "fetch__web".into(),
                json!({"url":bad_tls.url_path("/tls").replacen("http:", "https:", 1)}).to_string(),
            ),
            (
                "missing".into(),
                "observe__workspace".into(),
                json!({"request": {"mode":"read_file","path":"absent-private.txt"}}).to_string(),
            ),
        ]);
        let waiting = SseScript::ToolCalls {
            calls: vec![
                (
                    "fetch".into(),
                    "fetch__web".into(),
                    json!({"url":url}).to_string(),
                ),
                (
                    "tail".into(),
                    "session__todo_write".into(),
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
        let slots = initial["steps"][0]["slots"].as_array().unwrap();
        assert_eq!(slots.len(), 22);
        assert_eq!(slots[19]["outcome"], "succeeded");
        assert_eq!(slots[19]["material"]["kind"], "output_too_large");
        assert!(slots[19]["material"]["byte_length"].as_u64().unwrap() > 65536);
        assert_eq!(slots[20]["outcome"], "failed");
        assert_eq!(
            slots[20]["reason"],
            json!({"kind":"failed","value":{"code":"execution_failed","retriability":"transient"}})
        );
        // 网络不可用也包含连接失败；公开 Host 证据必须见证已连接后在 TLS 阶段失败。
        assert_eq!(slots[20]["evidence"]["kind"], "recorded");
        let effects = slots[20]["evidence"]["value"]["effects"]["effects"]
            .as_array()
            .expect("bad-TLS call must expose recorded effects");
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0]["source"], "operation");
        let network = &effects[0]["network"];
        assert_eq!(network["failure_phase"], "tls", "bad-TLS failure phase");
        assert_eq!(network["transport"]["tls"], json!({"state":"failed"}));
        assert_eq!(network["policy_verdict"], "allowed");
        assert_eq!(
            network["transport"]["connected_peer"],
            json!({"ip":"127.0.0.1","port":bad_tls.port()})
        );
        // 策略或校验拒绝不再是独立结局：它是 Failed 里的错误族，错误码与可重试性原样保留。
        assert_eq!(slots[21]["outcome"], "failed");
        assert_eq!(
            slots[21]["reason"],
            json!({"kind":"failed","value":{"code":"type_error","retriability":"permanent"}})
        );
        let before_beta = read_json(&world, &spec, &mut evidence, &["--json", "audit", "--tail"]);
        let cursor = before_beta["next_after"].as_u64().unwrap().to_string();
        let beta_provider = FakeLlmServer::start(support::sequence(vec![
            support::tool_call(
                "beta",
                "session__todo_write",
                r#"{"todos":[{"content":"beta-private","status":"pending"}]}"#,
            ),
            SseScript::plain("beta complete"),
        ]))
        .unwrap();
        world
            .wire_provider(
                lattice_system_harness::ProviderFixture::chat_completions("operator"),
                &beta_provider,
                FAKE_API_KEY,
            )
            .unwrap();
        let beta_read = |args: &[&str], evidence: &mut ScenarioEvidence| {
            let mut cmd = world.command(driver.binary_path());
            cmd.current_dir(&beta).args(args);
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
            ],
            &mut evidence,
        );
        let second = parse_send_stdout(&second.stdout, second.exit_code());
        assert!(second.malformed_lines.is_empty());
        let beta_tid = second.accepted_turn_id().unwrap().to_owned();
        assert_eq!(world.daemon_pids().unwrap().len(), 1);
        let beta_turn: Value = serde_json::from_slice(
            &beta_read(&["--json", "session", "turn", &beta_tid], &mut evidence).stdout,
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
        world
            .wire_provider(
                lattice_system_harness::ProviderFixture::chat_completions("operator"),
                &provider,
                FAKE_API_KEY,
            )
            .unwrap();
        let mut turns = vec![tid.to_string()];
        let mut windows = Vec::new();
        for crash in [false, true] {
            let front = driver
                .start(
                    &world,
                    &LatticeCommand {
                        args: ["--json", "session", "send", "wait fetch", "--session", sid]
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
            let turn = unclosed_turn(&world, &spec, &mut evidence, sid);
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
                assert_eq!(orphan["summary"]["outcome"], "failed");
                assert_eq!(
                    orphan["steps"][0]["slots"][0]["progress"]["state"],
                    "started"
                );
                let head = read_json(
                    &world,
                    &spec,
                    &mut evidence,
                    &["--json", "session", "log", sid],
                )["session"]["current"]
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
                assert_eq!(cancel(&world, &spec, &turn)["value"]["owned"], true);
                let out = front.wait(None).unwrap();
                evidence.record_command("cancelled fetch", &out);
                assert!(out.is_clean_exit(2));
            }
            let closed = stage(&world, &spec, &mut evidence, sid, &turn);
            // 已 Started 的调用没有确定结果：崩溃时是未取得结果，取消时是 LANCET 返回的 CallCancelled。
            assert_eq!(closed["steps"][0]["slots"][0]["outcome"], "indeterminate");
            assert_eq!(
                closed["steps"][0]["slots"][0]["reason"],
                json!({"kind":"indeterminate","value":if crash { "no_result" } else { "cancelled" }})
            );
            assert_eq!(
                closed["steps"][0]["end"]["cause"]["cause"],
                if crash { "process_exit" } else { "cancelled" }
            );
            assert!(closed["steps"][0]["slots"][1]["reason"].is_null());
            assert_eq!(closed["steps"][0]["slots"][1]["outcome"], "not_executed");
            assert_eq!(provider.captured_requests().len(), count);
            windows.push(
                json!({"turn":turn,"active":active,"replay":replay,"closed":closed,"crash":crash}),
            );
            turns.push(turn);
        }
        std::fs::remove_file(&profile).unwrap();
        std::fs::remove_file(beta.join(".lattice/agents/operator/agent.toml")).unwrap();
        world
            .register_key(
                lattice_system_harness::ProviderFixture::chat_completions("operator"),
                None,
            )
            .unwrap();
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
        // audit 保留实际阶段；尚未 Terminal 的结论从 Turn 的槽位详情核对。
        let outcomes: BTreeSet<_> = std::iter::once(&initial)
            .chain(windows.iter().map(|window| &window["closed"]))
            .flat_map(|turn| turn["steps"].as_array().unwrap())
            .flat_map(|step| step["slots"].as_array().unwrap())
            .map(|slot| slot["outcome"].as_str().unwrap())
            .collect();
        assert_eq!(
            outcomes,
            BTreeSet::from(["succeeded", "failed", "not_executed", "indeterminate"])
        );
        let changes = alpha_tail["rows"].as_array().unwrap();
        assert_eq!(
            changes.len(),
            22 * 3 + 4 + 3,
            "closure adds no synthetic slot progress"
        );
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
                &beta_tid,
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
                &beta_tid,
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
fn r2_s2_multicall_reconnect_busy_sigkill_preserves_unclosed_step() {
    let (world, spec) = support::direct_world(
        "r2-s2-vertical",
        "vertical",
        "[\"observe__workspace\",\"mutate__create\"]",
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
            json!({"test":"r2_s2_multicall_reconnect_busy_sigkill_preserves_unclosed_step","first":first,"second":second}),
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
        world
            .wire_provider(
                lattice_system_harness::ProviderFixture::chat_completions("vertical"),
                &provider,
                FAKE_API_KEY,
            )
            .unwrap();
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
                    ],
                    timeout: Some(budget.cap_timeout(spec.step_timeout())),
                },
            )
            .unwrap();
        gate.wait_until_stalled(1, budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        let (sid, tid) = running_turn(&world, &spec, &mut evidence);
        evidence
            .record_json("accepted", &accepted_detail(&world, &spec, &tid))
            .unwrap();
        for i in 1..=5 {
            gate.wait_until_stalled(i, budget.cap_timeout(spec.step_timeout()))
                .unwrap();
            gate.release_next(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
        }
        gate.wait_until_stalled(6, budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        let first_done = stage(&world, &spec, &mut evidence, &sid, &tid);
        assert_eq!(first_done["steps"][0]["end"], json!({"end":"continue"}));
        let slots = first_done["steps"][0]["slots"].as_array().unwrap();
        assert_eq!(slots.len(), 3);
        assert!(slots.iter().all(|v| v["outcome"] == "succeeded"));
        assert_eq!(
            std::fs::read_to_string(world.project_dir().join("created.txt")).unwrap(),
            "durable\n"
        );
        let tip = history_tip(&world);
        // 第一次观察在第二步生成前断开；之后重新开始的观察不依赖它看到过什么。
        let cut = watch(&world, &spec, &sid);
        assert!(cut.view.head.as_ref().unwrap().owner);
        assert_eq!(
            step_progress(&cut.view, 2),
            Some(&StepProgress::Closed(StepEnd::Continue))
        );
        evidence
            .record_json("before-disconnect", &json!(cut.received))
            .unwrap();
        drop(cut);
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
        let mut expected: Vec<_> = (0..70)
            .map(|i| {
                if i % 2 == 0 {
                    segment(SegmentKind::Text, &format!("t{i:02};"))
                } else {
                    segment(SegmentKind::Reasoning, &format!("r{i:02};"))
                }
            })
            .collect();
        // 新的观察者从这次尝试的开头收到已生成的文字，之后只收到接在后面的部分。
        let mut resumed = watch(&world, &spec, &sid);
        resumed.until(deadline(&world, &spec), "70 text segments", |view| {
            view.texts.get(&(3, 0)).is_some_and(|text| text.len() == 70)
        });
        assert_eq!(resumed.view.texts[&(3, 0)], expected);
        let first_text = resumed
            .received
            .iter()
            .find(|message| message["value"]["type"] == "text")
            .unwrap();
        assert_eq!(first_text["value"]["offset"], 0);
        evidence
            .record_json("reconnected-midway", &json!(resumed.received))
            .unwrap();
        gate.release_next(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        gate.wait_until_stalled(77, budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        resumed.until(deadline(&world, &spec), "the 71st text segment", |view| {
            view.texts[&(3, 0)].len() == 71
        });
        let sent: usize = expected.iter().map(|segment| segment.text.len()).sum();
        let appended = resumed.received.last().unwrap();
        assert_eq!(
            appended["value"],
            json!({"type":"text","node":3,"attempt":0,"offset":sent,
                "segments":[{"kind":"text","text":"t70;"}]})
        );
        expected.push(segment(SegmentKind::Text, "t70;"));
        drop(resumed);
        for (index, request) in [
            submit_request(&world, OTHER, json!({"existing":sid}), "must be busy", Some("vertical")),
            json!({"operation":"rewind","session_id":sid,"target":2,"workspace":world.project_dir()}),
        ]
        .into_iter()
        .enumerate()
        {
            let rejected = ipc(&world, timeout(&world, &spec), request);
            assert_eq!(rejected["value"]["kind"], "busy", "{rejected}");
            evidence
                .record_json(&format!("busy-{index}"), &rejected)
                .unwrap();
        }
        // 同一 TurnId 再次提交不找回原请求，而是冲突；新会话不会因此建立。
        let reused = ipc(
            &world,
            timeout(&world, &spec),
            submit_request(
                &world,
                &tid,
                json!({"new":SESSION}),
                "original input",
                Some("vertical"),
            ),
        );
        assert_eq!(reused["response"], "error", "{reused}");
        assert_eq!(reused["value"]["kind"], "conflict");
        assert_eq!(reused["value"]["conflict"], "turn_id_used");
        evidence.record_json("turn-id-used", &reused).unwrap();
        assert_eq!(
            read_json(&world, &spec, &mut evidence, &["--json", "session", "list"])
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(provider.captured_requests().len(), 2);
        let log = read_json(
            &world,
            &spec,
            &mut evidence,
            &["--json", "session", "log", &sid],
        );
        assert_eq!(log["nodes"].as_array().unwrap().len(), 4);
        // 同一后台第二 Session 在 A 的 gate 放行前到达独立 provider，证明不是全 Turn 全局排队。
        let mut peer =
            support::wire_fake_llm(&world, "vertical", SseScript::plain("parallel session"));
        cli(
            &world,
            &spec,
            &mut evidence,
            &[
                "--json", "session", "send", "parallel", "--agent", "vertical",
            ],
            0,
        );
        assert_eq!(peer.captured_requests().len(), 1);
        peer.shutdown(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        world
            .wire_provider(
                lattice_system_harness::ProviderFixture::chat_completions("vertical"),
                &provider,
                FAKE_API_KEY,
            )
            .unwrap();
        gate.release(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        let marker = wait_for_ack(&ack, budget.cap_timeout(spec.step_timeout()));
        assert_eq!(marker["barrier"], "invocation_started");
        assert_eq!(marker["turn_id"], tid);
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
        let reopened = stage(&world, &spec, &mut evidence, &sid, &tid);
        assert_eq!(reopened["summary"]["outcome"], "failed");
        assert_eq!(reopened["steps"][0], first_done["steps"][0]);
        let slots = reopened["steps"][1]["slots"].as_array().unwrap();
        assert_eq!(slots.len(), 3);
        assert_eq!(slots[0]["outcome"], "succeeded");
        assert_eq!(slots[1]["progress"]["state"], "started");
        assert_eq!(slots[2]["progress"]["state"], "registered");
        assert_eq!(slots[1]["invocation_id"], marker["invocation_id"]);
        // 重启后不必先观察那一轮：会话头直接说明它没有 Owner，生成中的文字随进程消失。
        let fresh = watch(&world, &spec, &sid);
        let head = fresh.view.head.as_ref().unwrap();
        assert!(!head.owner);
        assert_eq!(head.turn_id.as_deref(), Some(tid.as_str()));
        assert_eq!(head.position, 3);
        assert!(fresh.view.texts.is_empty());
        let unclosed = fresh.view.entries[&3].step().unwrap();
        assert_eq!(merge_segments(&unclosed.reply), expected);
        evidence
            .record_json("read-only-reopen", &json!(fresh.received))
            .unwrap();
        drop(fresh);
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
        let closed = stage(&world, &spec, &mut evidence, &sid, &tid);
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
                &tid,
            ],
        );
        assert_eq!(closed["summary"]["outcome"], "failed");
        assert_eq!(closed["summary"]["notice"]["level"], "error");
        assert_eq!(closed["steps"][0], first_done["steps"][0]);
        assert_eq!(closed, reopened, "rewind must not alter the old Turn");
        let stopped = &closed["steps"][1];
        assert_eq!(stopped["end"]["end"], "stop");
        assert_eq!(closed["steps"][1]["node"], 3);
        assert_eq!(stopped["slots"][0], slots[0]);
        assert_eq!(stopped["slots"][1]["outcome"], "indeterminate");
        assert_eq!(stopped["slots"][2]["outcome"], "not_executed");
        assert_eq!(
            stopped["slots"][1]["reason"],
            json!({"kind":"indeterminate","value":"no_result"})
        );
        // 没轮到的槽位不带原因；为什么停只记在 Step 的 end.cause。
        assert!(stopped["slots"][2]["reason"].is_null());
        // 崩溃发生在 Started 前后，停止原因都是进程退出。
        assert_eq!(stopped["end"]["cause"], json!({"cause":"process_exit"}));
        // 未关闭的末端 Step 与它之前的节点一样作为条目给出，并带这一轮的结局。
        read_json(
            &world,
            &spec,
            &mut evidence,
            &["--json", "session", "rewind", &sid, "3"],
        );
        let current = watch(&world, &spec, &sid);
        let entries = current.view.raw_entries();
        assert_eq!(entries.len(), 3);
        assert_paired_entries(&entries, &closed);
        let content = current.view.entries[&3].step().unwrap();
        assert_eq!(content.slots.len(), 3);
        assert_eq!(merge_segments(&content.reply), expected);
        evidence
            .record_json("complete-entries", &json!(current.received))
            .unwrap();
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
                &["--json", "session", "turn", &tid]
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
                    &tid
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
    let panic = result
        .err()
        .expect("crash start must reject the expired scenario budget");
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .expect("scenario budget panic must carry a text message");
    // 只允许实际耗时变化；场景身份、预算和完整诊断都必须来自预算检查。
    let prefix = format!(
        "scenario '{}' exceeded total timeout 0ns (elapsed ",
        spec.name
    );
    let elapsed = message
        .strip_prefix(&prefix)
        .and_then(|rest| {
            rest.strip_suffix(
                "); a multi-step sequence must terminate within the whole-scenario budget",
            )
        })
        .expect("crash start must panic specifically because the scenario budget expired");
    assert!(
        !elapsed.is_empty(),
        "budget panic must report the elapsed time"
    );
    assert!(world.daemon_pids().unwrap().is_empty());
}

/// 条目中这一轮的每个 Step 必须与详情逐槽位配对，不只比较调用数；未关闭的末端 Step 同样在条目中。
fn assert_paired_entries(entries: &[&Value], turn: &Value) {
    let steps = turn["steps"].as_array().unwrap();
    let mut ids = std::collections::BTreeSet::new();
    for (ordinal, step) in steps.iter().enumerate() {
        assert_eq!(step["key"]["ordinal"], ordinal);
        let node = step["node"].as_u64().unwrap();
        let entry = entries.iter().find(|entry| entry["node"] == node).unwrap();
        // 条目带的 StepDetail 就是 `session turn` 给出的那一步；结局只随最后一步给出。
        assert_eq!(entry["step"], *step);
        if ordinal + 1 < steps.len() {
            assert_eq!(step["progress"]["progress"], "closed");
            assert_eq!(step["end"], json!({"end":"continue"}));
            assert!(entry.get("summary").is_none(), "{entry}");
        } else {
            assert_eq!(entry["summary"], turn["summary"]);
        }
        let content = &entry["content"];
        assert_eq!(content["step_ordinal"], ordinal);
        assert_eq!(content["turn_id"], turn["turn_id"]);
        assert_eq!(content["progress"], step["progress"]);
        let calls: Vec<_> = content["reply"]["parts"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .filter_map(|(part, value)| {
                value
                    .pointer("/semantic/tool_call/call")
                    .map(|call| (part, call))
            })
            .collect();
        let stored = content["slots"].as_array().unwrap();
        let slots = step["slots"].as_array().unwrap();
        assert_eq!(calls.len(), stored.len());
        assert_eq!(calls.len(), slots.len());
        for (((part, call), raw), slot) in calls.iter().zip(stored).zip(slots) {
            assert!(ids.insert(call["invocation_id"].as_str().unwrap()));
            assert_eq!(call["invocation_id"], raw["binding"]["invocation_id"]);
            assert_eq!(call["invocation_id"], slot["invocation_id"]);
            assert_eq!(call["name"], slot["binding"]["method"]);
            assert_eq!(raw["binding"]["identity"], slot["binding"]);
            assert_eq!(call["address"]["part_ordinal"], *part);
            assert_eq!(
                call["address"]["message"],
                json!({"session_id":turn["session_id"],"turn_id":turn["turn_id"],"model_step_ordinal":ordinal})
            );
            assert_eq!(raw["progress"]["progress"], slot["progress"]["state"]);
            if raw["progress"]["progress"] == "terminal" {
                let result = &raw["progress"]["result"];
                assert_eq!(result["invocation_id"], call["invocation_id"]);
                assert_eq!(result["name"], call["name"]);
                assert_eq!(result["payload"]["outcome"], slot["outcome"]);
                assert_eq!(raw["progress"]["evidence"], slot["evidence"]);
            }
        }
    }
    let actual_nodes: Vec<_> = entries
        .iter()
        .filter(|entry| {
            entry["content"]["kind"] == "step" && entry["content"]["turn_id"] == turn["turn_id"]
        })
        .map(|entry| entry["node"].as_u64().unwrap())
        .collect();
    let expected_nodes: Vec<_> = steps
        .iter()
        .map(|step| step["node"].as_u64().unwrap())
        .collect();
    assert_eq!(actual_nodes, expected_nodes);
    let last = steps.last().unwrap();
    assert_eq!(turn["summary"]["progress"], last["progress"]);
    let end = &last["end"];
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
fn cli_recovers_accepted_receipt_after_backend_crash() {
    let (world, spec) = support::direct_world("cli-recovered-acceptance", "recover", "[]", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        provenance(
            &world,
            &mut evidence,
            json!({
                "test":"cli_recovers_accepted_receipt_after_backend_crash",
                "barrier":"turn_accepted", "recovery":"original CLI queries original TurnId after restart"
            }),
        );
        let mut provider =
            support::wire_fake_llm(&world, "recover", SseScript::plain("must not run"));
        let ack = world.root().join("accepted.json");
        world.set_env(BARRIER, "turn_accepted");
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
                        "recover this input",
                        "--agent",
                        "recover",
                    ]
                    .into_iter()
                    .map(String::from)
                    .collect(),
                    timeout: Some(budget.cap_timeout(spec.step_timeout())),
                },
            )
            .unwrap();
        let marker = wait_for_ack(&ack, budget.cap_timeout(spec.step_timeout()));
        assert_eq!(marker["barrier"], "turn_accepted");
        let sid = marker["session_id"].as_str().unwrap();
        let tid = marker["turn_id"].as_str().unwrap();
        evidence
            .record_json("accepted-before-receipt", &marker)
            .unwrap();
        // CLI 尚未收到回执；暂停它直到后台重启就绪，排除找回查询撞上重启空窗。
        let frontend_pgid = i32::try_from(frontend.pgid()).unwrap();
        assert_ne!(frontend.pgid(), daemon.pgid());
        // SAFETY: 驱动已核验这是场景自有前端的独立进程组。
        assert_eq!(unsafe { libc::killpg(frontend_pgid, libc::SIGSTOP) }, 0);
        let pause_deadline = Instant::now() + budget.cap_timeout(Duration::from_secs(2));
        loop {
            let stat = std::fs::read_to_string(format!("/proc/{frontend_pgid}/stat")).unwrap();
            if stat
                .rsplit_once(')')
                .unwrap()
                .1
                .trim_start()
                .starts_with('T')
            {
                break;
            }
            assert!(
                Instant::now() < pause_deadline,
                "frontend did not enter SIGSTOP before backend crash"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        evidence
            .record_json(
                "recovery-window",
                &json!({
                    "frontend_pgid":frontend_pgid,"backend_pgid":daemon.pgid(),
                    "frontend_state":"T","restart_before_frontend_continues":true
                }),
            )
            .unwrap();
        support::crash::kill_backend(&world, daemon, &mut evidence, &budget);
        world.unset_env(BARRIER);
        world.unset_env(ACK);
        let restarted = support::crash::start(&world, &spec, &budget);
        // SAFETY: 同一个场景自有前端仍由 RunningCommand 持有，尚未回收。
        assert_eq!(unsafe { libc::killpg(frontend_pgid, libc::SIGCONT) }, 0);
        let output = frontend.wait(None).unwrap();
        evidence.record_command("original CLI recovers acceptance", &output);
        assert!(output.is_clean_exit(1), "{}", output.summary());
        let parsed = parse_send_stdout(&output.stdout, output.exit_code());
        assert!(
            parsed.malformed_lines.is_empty(),
            "{:?}",
            parsed.malformed_lines
        );
        assert_eq!(parsed.sending(), Some((tid, sid, true)));
        let receipt = parsed.receipt().expect("one receipt");
        assert!(receipt.recovered);
        assert_eq!(parsed.accepted_turn_id(), Some(tid));
        assert_eq!(parsed.accepted_session_id(), Some(sid));
        assert_eq!(parsed.closed_result(), Some((sid, tid, "failed")));
        let reopened = accepted_detail(&world, &spec, tid);
        assert!(support::same_turn(&support::live_turn(&parsed), &reopened));
        let tree = read_json(
            &world,
            &spec,
            &mut evidence,
            &["--json", "session", "log", sid],
        );
        assert_eq!(tree["nodes"].as_array().unwrap().len(), 3);
        let sessions = read_json(&world, &spec, &mut evidence, &["--json", "session", "list"]);
        assert_eq!(sessions.as_array().unwrap().len(), 1);
        assert!(provider.captured_requests().is_empty());
        cli(
            &world,
            &spec,
            &mut evidence,
            &["--json", "daemon", "stop"],
            0,
        );
        world.daemon_departed().unwrap();
        let stopped = restarted.wait(None).unwrap();
        evidence.record_command("restarted backend drains", &stopped);
        assert!(stopped.is_clean_exit(0), "{}", stopped.summary());
        provider
            .shutdown(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
    }));
    support::finish_recorded(evidence, result);
}

#[test]
fn redb_commit_windows_read_conclusions_without_writes_and_preserve_facts() {
    for (name, barrier, mode, final_outcome, requests) in [
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
            "[\"observe__workspace\",\"mutate__create\",\"session__todo_write\"]",
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
                "nonretry" | "retry" => SseScript::Chunks {
                    chunks: vec![
                        json!({"choices":[],"usage":{"prompt_tokens":2,"completion_tokens":3}}),
                        json!({"error":{"type":if mode == "retry" {"server_error"} else {"invalid_request_error"},"message":mode}}),
                    ],
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
                json!({"test":"redb_commit_windows_read_conclusions_without_writes_and_preserve_facts",
                "case":name,"barrier":barrier,"script":format!("{script:?}")}),
            );
            let mut provider = support::wire_fake_llm(&world, "matrix", script);
            let configuration = world.home().join(".lattice/providers.toml");
            let source = std::fs::read_to_string(&configuration).unwrap();
            let set_price = |input, output| {
                std::fs::write(&configuration, format!("{source}\n[providers.local.models.matrix.cost]\ninput = {input}\noutput = {output}\n")).unwrap();
            };
            set_price(2, 10);
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
            let tid = marker["turn_id"].as_str().unwrap();
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
            set_price(200, 1000);
            let sid = marker["session_id"].as_str().unwrap();
            let before = stage(&world, &spec, &mut evidence, sid, tid);
            assert_eq!(provider.captured_requests().len(), requests);
            assert_eq!(
                before["summary"]["outcome"],
                final_outcome.unwrap_or("failed")
            );
            let observer = watch(&world, &spec, sid);
            let head = observer.view.head.as_ref().unwrap();
            assert!(!head.owner);
            assert_eq!(head.turn_id.as_deref(), Some(tid));
            let entries = observer.view.raw_entries();
            assert_paired_entries(&entries, &before);
            let usage = read_json(
                &world,
                &spec,
                &mut evidence,
                &["--json", "session", "usage", sid],
            );
            assert_eq!(usage["position"], head.position);
            assert_eq!(usage["turns"].as_array().unwrap().len(), 1);
            assert_eq!(usage["turns"][0]["turn_id"], tid);
            assert_eq!(usage["turns"][0]["usage"], before["usage"]);
            assert_eq!(usage["path_total"], before["usage"]);
            assert_eq!(usage["session_total"], before["usage"]);
            assert_eq!(accepted_detail(&world, &spec, tid), before);
            assert_eq!(
                watch(&world, &spec, sid).received,
                observer.received,
                "usage and observation preserve the Turn and node facts"
            );
            let prior = before["steps"].as_array().unwrap();
            for (index, step) in prior.iter().enumerate() {
                for attempt in step["attempts"].as_array().unwrap() {
                    if attempt["progress"]["progress"] == "sent" {
                        assert!(attempt["usage"].is_null());
                        assert_eq!(attempt["usage_kind"], "partial");
                        assert!(attempt["cost"].is_null());
                    } else {
                        let interrupted = index == 1 && matches!(mode, "retry" | "nonretry");
                        let expected = if interrupted {
                            0.000034
                        } else if index == 1 && mode == "invalid" {
                            0.000070
                        } else {
                            0.000012
                        };
                        assert_eq!(
                            attempt["usage_kind"],
                            if interrupted { "partial" } else { "complete" },
                            "{name}: {attempt}"
                        );
                        assert_eq!(attempt["cost"]["exact"], !interrupted, "{name}: {attempt}");
                        let fee = attempt["cost"]["usd"]
                            .as_f64()
                            .expect("committed attempt cost");
                        assert!((fee - expected).abs() < 1e-15, "{name}: {attempt}");
                    }
                }
            }
            let last = prior.last().unwrap();
            let expected_progress = match name {
                "accepted" | "continue" => "reserved",
                "requesting" | "requesting-second" | "retry" => "requesting",
                "nonretry" | "finish" | "invalid" => "closed",
                _ => "executing",
            };
            assert_eq!(last["progress"]["progress"], expected_progress);
            assert_eq!(before["summary"]["progress"], last["progress"]);
            for step in &prior[..prior.len() - 1] {
                assert_eq!(step["progress"]["progress"], "closed");
                assert_eq!(step["end"], json!({"end":"continue"}));
            }
            // 当前位置停在这一轮最后写入的 Step，无论它是否关闭。
            assert_eq!(head.position, last["node"]);
            if final_outcome.is_none() {
                assert_eq!(
                    last["end"],
                    json!({"end":"stop","cause":{"cause":"process_exit"}})
                );
            }
            if matches!(name, "accepted" | "continue") {
                assert!(last["attempts"].as_array().unwrap().is_empty());
            } else {
                let attempt = &last["attempts"][0];
                assert_eq!(last["attempts"].as_array().unwrap().len(), 1);
                assert_eq!(
                    attempt["progress"]["progress"],
                    if matches!(name, "requesting" | "requesting-second") {
                        "sent"
                    } else {
                        "ended"
                    }
                );
                if matches!(name, "requesting" | "requesting-second") {
                    assert_eq!(attempt["end"]["cause"]["cause"], "process_exit");
                } else if matches!(name, "retry" | "nonretry" | "invalid") {
                    assert_eq!(attempt["end"]["end"], "stopped");
                    assert_eq!(
                        attempt["end"]["cause"]["cause"],
                        if name == "invalid" {
                            "invalid_batch"
                        } else {
                            "provider_error"
                        }
                    );
                    if name != "invalid" {
                        assert_eq!(attempt["end"]["cause"]["retryable"], name == "retry");
                    }
                } else {
                    assert_eq!(attempt["end"]["end"], "completed");
                }
                if name == "retry" {
                    assert!(attempt["retry_delay_ms"].as_u64().is_some());
                } else {
                    assert!(attempt["retry_delay_ms"].is_null());
                }
            }
            if requests > 0 {
                assert_eq!(prior[0]["end"], json!({"end":"continue"}));
                assert_eq!(prior[0]["slots"].as_array().unwrap().len(), 3);
                assert!(
                    prior[0]["slots"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(
                            |s| s["progress"]["state"] == "terminal" && s["outcome"] == "succeeded"
                        )
                );
            }
            let slots = prior
                .get(1)
                .and_then(|s| s["slots"].as_array())
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
                    .map(|s| s["progress"]["state"].as_str().unwrap())
                    .collect();
                assert_eq!(
                    states,
                    match name {
                        "registered" => vec!["registered"; 3],
                        "started" => vec!["terminal", "started", "registered"],
                        "terminal" | "oversize" => vec!["terminal", "terminal", "registered"],
                        _ => vec!["terminal"; 3],
                    }
                );
                for slot in &slots {
                    match slot["progress"]["state"].as_str().unwrap() {
                        "registered" => {
                            assert_eq!(slot["outcome"], "not_executed");
                            assert!(slot["reason"].is_null());
                        }
                        "started" => {
                            assert_eq!(slot["outcome"], "indeterminate");
                            assert_eq!(
                                slot["reason"],
                                json!({"kind":"indeterminate","value":"no_result"})
                            );
                        }
                        "terminal" => assert_eq!(slot["outcome"], "succeeded"),
                        _ => unreachable!(),
                    }
                }
            }
            if name == "oversize" {
                assert_eq!(slots[1]["material"]["kind"], "output_too_large");
                assert!(slots[1]["material"]["byte_length"].as_u64().unwrap() > 65_536);
            }
            if name == "invalid" {
                assert_eq!(
                    last["end"],
                    json!({"end":"stop","cause":{"cause":"invalid_batch"}})
                );
                let mut usage = before["usage"].clone();
                let amount = usage["cost"]["known"]["usd"].take();
                let amount = amount.as_f64().expect("known aggregate cost");
                assert!(
                    (amount - 0.000082).abs() < 1e-15,
                    "invalid batch cost: {amount}"
                );
                assert_eq!(
                    usage,
                    json!({"attempts":2,"partial_attempts":0,"kind":"complete",
                        "input":{"known":11,"complete":true,"overflowed":false},
                        "cache_read":{"known":null,"complete":false,"overflowed":false},
                        "cache_write":{"known":null,"complete":false,"overflowed":false},
                        "output":{"known":6,"complete":true,"overflowed":false},
                        "reasoning":{"known":null,"complete":false,"overflowed":false},
                        "cost":{"known":{"usd":null,"exact":true},"unknown_attempts":0,"overflowed":false}})
                );
            }
            if name == "nonretry" {
                assert_eq!(
                    last["end"],
                    json!({"end":"stop","cause":{"cause":"provider_error","retryable":false}})
                );
            }
            let installed = world.project_dir().join("kept.txt").exists();
            assert_eq!(
                installed,
                matches!(name, "terminal" | "all-terminal" | "continue" | "finish")
            );
            assert_eq!(world.project_dir().join("first.txt").exists(), requests > 0);
            let tip = (requests > 0).then(|| history_tip(&world));
            // 先完整读取并核验，再进行任何会话写操作；旧节点与阶段不能被补写。
            let old_entries: Vec<Value> = entries.into_iter().cloned().collect();
            read_json(
                &world,
                &spec,
                &mut evidence,
                &[
                    "--json",
                    "session",
                    "rewind",
                    sid,
                    &head.position.to_string(),
                ],
            );
            assert_eq!(stage(&world, &spec, &mut evidence, sid, tid), before);
            let next_provider =
                support::wire_fake_llm(&world, "matrix", SseScript::plain("new reply"));
            cli(
                &world,
                &spec,
                &mut evidence,
                &["--json", "session", "send", "new input", "--session", sid],
                0,
            );
            assert_eq!(stage(&world, &spec, &mut evidence, sid, tid), before);
            assert_eq!(provider.captured_requests().len(), requests);
            assert_eq!(next_provider.captured_requests().len(), 1);
            let current = watch(&world, &spec, sid);
            let preserved: Vec<Value> = current
                .view
                .entries
                .values()
                .filter(|entry| entry.turn_id == tid)
                .map(|entry| entry.raw.clone())
                .collect();
            assert_eq!(preserved, old_entries);
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
            evidence.record_json("state-summary", &json!({"case":name,"before":before,"watch":observer.received,"old_entries":old_entries,"provider_requests_before":requests,"new_requests":1,"barrier":marker})).unwrap();
            evidence.record_captures(provider.captured_requests());
            evidence.record_captures(next_provider.captured_requests());
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
        "[\"observe__workspace\",\"mutate__create\"]",
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
        let (sid, tid) = running_turn(&world, &spec, &mut evidence);
        let (sid, tid) = (sid.as_str(), tid.as_str());
        let before = accepted_detail(&world, &spec, tid);
        assert_eq!(before["steps"][0]["slots"][0]["outcome"], "succeeded");
        let tip = history_tip(&world);
        let taken = cancel(&world, &spec, tid);
        assert_eq!(taken["value"]["owned"], true);
        evidence.record_json("cancel", &taken).unwrap();
        let marker = wait_for_ack(&ack, budget.cap_timeout(spec.step_timeout()));
        assert_eq!(marker["barrier"], "attempt_stopped");
        assert_eq!(marker["step"], 1);
        assert_eq!(marker["turn_id"], tid);
        evidence.record_json("marker", &marker).unwrap();
        support::crash::kill(&world, daemon, frontend, &mut evidence, &budget);
        world.unset_env(BARRIER);
        world.unset_env(ACK);
        let closed = stage(&world, &spec, &mut evidence, sid, tid);
        assert_eq!(closed["summary"]["outcome"], "cancelled");
        assert_eq!(closed["summary"]["notice"]["level"], "info");
        assert_eq!(closed["steps"][0], before["steps"][0]);
        assert_eq!(
            closed["steps"][1]["end"],
            json!({"end":"stop","cause":{"cause":"cancelled"}})
        );
        let observer = watch(&world, &spec, sid);
        assert_paired_entries(&observer.view.raw_entries(), &closed);
        assert_eq!(closed["steps"][0]["slots"].as_array().unwrap().len(), 3);
        evidence
            .record_json("paired-entries", &json!(observer.received))
            .unwrap();
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
                &["--json", "session", "turn", tid]
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
    for target in [2u64, 0] {
        let (world, spec) = support::direct_world(
            &format!("r2-rewind-crash-{target}"),
            "rewind",
            "[\"observe__workspace\",\"mutate__create\"]",
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
            let sent = cli(
                &world,
                &spec,
                &mut evidence,
                &["--json", "session", "send", "original", "--agent", "rewind"],
                0,
            );
            let sent = parse_send_stdout(&sent.stdout, sent.exit_code());
            let (sid, tid) = (
                sent.accepted_session_id().unwrap(),
                sent.accepted_turn_id().unwrap(),
            );
            let original = stage(&world, &spec, &mut evidence, sid, tid);
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
            let reopened = stage(&world, &spec, &mut evidence, sid, tid);
            assert_eq!(reopened, original);
            let log = read_json(
                &world,
                &spec,
                &mut evidence,
                &["--json", "session", "log", sid],
            );
            assert_eq!(log["session"]["current"], target);
            assert_eq!(log["nodes"].as_array().unwrap().len(), 4);
            // 退回后的路径是根到目标节点：节点 1..=target 各一条条目。
            let observer = watch(&world, &spec, sid);
            assert_eq!(observer.view.head.as_ref().unwrap().position, target);
            assert_eq!(
                observer.view.entries.keys().copied().collect::<Vec<_>>(),
                (1..=target).collect::<Vec<_>>()
            );
            evidence
                .record_json("rewound-entries", &json!(observer.received))
                .unwrap();
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
            assert_eq!(stage(&world, &spec, &mut evidence, sid, tid), original);
            let original_reader = watch(&world, &spec, sid);
            assert_paired_entries(&original_reader.view.raw_entries(), &original);
            assert_eq!(original["steps"][0]["slots"].as_array().unwrap().len(), 3);
            evidence
                .record_json("original-paired-entries", &json!(original_reader.received))
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
fn slow_window_and_closed_snapshot_deliver_exact_history() {
    let (world, spec) = support::direct_world(
        "r2-observation-windows",
        "windows",
        "[\"observe__workspace\",\"mutate__create\"]",
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
                SseScript::plain("next turn"),
            ]),
            budget.cap_timeout(spec.step_timeout()),
        )
        .unwrap();
        world
            .wire_provider(
                lattice_system_harness::ProviderFixture::chat_completions("windows"),
                &provider,
                FAKE_API_KEY,
            )
            .unwrap();
        let schedule = world.root().join("schedule");
        std::fs::create_dir(&schedule).unwrap();
        for name in ["turn_closed", "invocation_started"] {
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
        let (sid, tid) = running_turn(&world, &spec, &mut evidence);
        let (sid, tid) = (sid.as_str(), tid.as_str());
        let partial = vec![segment(SegmentKind::Text, "discarded partial")];
        let mut live = watch(&world, &spec, sid);
        assert!(live.view.head.as_ref().unwrap().owner);
        live.until(
            deadline(&world, &spec),
            "the discarded partial text",
            |view| view.texts.get(&(2, 0)) == Some(&partial),
        );
        for i in 2..=3 {
            gate.wait_until_stalled(i, budget.cap_timeout(spec.step_timeout()))
                .unwrap();
            gate.release_next(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
        }
        gate.wait_until_stalled(4, budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        // 重试提示是 Step 条目的一部分，随条目推送，没有单独的事件。
        live.until(
            deadline(&world, &spec),
            "a retry notice on step 0",
            |view| {
                view.entries
                    .get(&2)
                    .and_then(|entry| entry.step())
                    .is_some_and(|step| !step.retries.is_empty())
            },
        );
        let live_retry = live.view.entries[&2].step().unwrap().retries.clone();
        assert_eq!(live_retry.len(), 1);
        assert_eq!(live_retry[0].attempt, 0);
        let retry = accepted_detail(&world, &spec, tid);
        assert_eq!(
            retry["steps"][0]["attempts"][0]["end"]["cause"],
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
        // 工具执行暂停期间重新开始观察：条目说明这一步正在执行，回复已写入节点。
        let reconnect = watch(&world, &spec, sid);
        assert_eq!(
            step_progress(&reconnect.view, 2),
            Some(&StepProgress::Executing)
        );
        assert_eq!(
            merge_segments(&reconnect.view.entries[&2].step().unwrap().reply),
            vec![
                segment(SegmentKind::Text, "tools explanation"),
                segment(SegmentKind::Reasoning, "tools reason"),
            ]
        );
        evidence
            .record_json(
                "attempt-and-tool-windows",
                &json!({"retry":retry,"tool_pause":tool_pause,
            "live":live.received,"reconnected":reconnect.received}),
            )
            .unwrap();
        drop(reconnect);
        std::fs::write(schedule.join("invocation_started.release"), b"").unwrap();
        gate.wait_until_stalled(9, budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        // 慢观察者：从此不再读取，直到这一轮关闭；它不能阻塞执行，之后读到的条目收敛到关闭后的结果。
        let mut slow = Watcher::open(&world, timeout(&world, &spec), sid);
        for i in 9..=78 {
            gate.wait_until_stalled(i, budget.cap_timeout(spec.step_timeout()))
                .unwrap();
            gate.release_next(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
        }
        gate.wait_until_stalled(79, budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        let mut reconstructed: Vec<_> = (0..70)
            .map(|i| {
                if i % 2 == 0 {
                    segment(SegmentKind::Text, &format!("text-{i};"))
                } else {
                    segment(SegmentKind::Reasoning, &format!("reason-{i};"))
                }
            })
            .collect();
        live.until(deadline(&world, &spec), "70 text segments", |view| {
            view.texts.get(&(3, 0)).is_some_and(|text| text.len() == 70)
        });
        assert_eq!(live.view.texts[&(3, 0)], reconstructed);
        gate.release_next(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        gate.wait_until_stalled(80, budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        live.until(deadline(&world, &spec), "the 71st text segment", |view| {
            view.texts[&(3, 0)].len() == 71
        });
        reconstructed.push(segment(SegmentKind::Text, "text-70;"));
        assert_eq!(live.view.texts[&(3, 0)], reconstructed);
        gate.release(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        let closed_pause = wait_for_ack(
            &schedule.join("turn_closed.ready"),
            budget.cap_timeout(spec.step_timeout()),
        );
        assert_eq!(closed_pause["turn_id"], tid);
        let closed = accepted_detail(&world, &spec, tid);
        assert_eq!(closed["summary"]["outcome"], "completed");
        // 关闭事务已提交、变化尚未标记：新观察者开头的条目已带关闭的结局，Owner 仍在。
        assert!(!schedule.join("turn_closed.release").exists());
        let mut observer = watch(&world, &spec, sid);
        assert!(observer.view.head.as_ref().unwrap().owner);
        assert!(turn_closed(&observer.view, tid));
        assert_paired_entries(&observer.view.raw_entries(), &closed);
        let paused: Vec<Value> = observer.view.raw_entries().into_iter().cloned().collect();
        stage(&world, &spec, &mut evidence, sid, tid);
        std::fs::write(schedule.join("turn_closed.release"), b"").unwrap();
        let done = front.wait(None).unwrap();
        evidence.record_command("front finishes after notification release", &done);
        assert!(done.is_clean_exit(0), "{}", done.summary());
        let sent = parse_send_stdout(&done.stdout, done.exit_code());
        assert_eq!(sent.closed_result(), Some((sid, tid, "completed")));
        // 读事务可能提前看到尚未标记的提交，标记后对同一节点再推送同一个值：
        // 读完放行后到达的推送，持有的条目不变。
        observer.drain(Duration::from_millis(500));
        let finished: Vec<Value> = observer.view.raw_entries().into_iter().cloned().collect();
        assert_eq!(finished, paused);
        // 前端、实时观察者与慢观察者最后持有的条目都与关闭后的条目逐字段相同。
        assert!(
            sent.entries()
                .iter()
                .map(|entry| &entry.raw)
                .eq(finished.iter())
        );
        live.until(deadline(&world, &spec), "the closed entries", |view| {
            view.raw_entries().into_iter().eq(finished.iter())
        });
        slow.until(deadline(&world, &spec), "the closed entries", |view| {
            view.raw_entries().into_iter().eq(finished.iter())
        });
        let reply = merge_segments(&observer.view.entries[&3].step().unwrap().reply);
        assert_eq!(reply, reconstructed);
        evidence
            .record_json(
                "closed-before-push",
                &json!({"pause":closed_pause,"watch":observer.received,
            "live":live.received}),
            )
            .unwrap();
        evidence
            .record_json("slow-through-close", &json!(slow.received))
            .unwrap();
        evidence
            .record_json(
                "state-summary",
                &json!({"turn":closed,"daemon":daemon.pgid(),"provider_requests":3}),
            )
            .unwrap();
        let captures = provider.captured_requests();
        assert_eq!(captures.len(), 3);
        assert_eq!(captures[0].raw["messages"], captures[1].raw["messages"]);
        assert!(
            !captures[2].raw["messages"]
                .to_string()
                .contains("discarded partial")
        );
        assert!(
            finished.iter().all(|entry| !entry["content"]["reply"]
                .to_string()
                .contains("discarded partial")),
            "stopped partial text never becomes the conversation reply"
        );
        assert_eq!(closed["steps"][0]["attempts"].as_array().unwrap().len(), 2);
        evidence.record_captures(captures);
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
        // 重启后重新开始观察：条目与关闭时相同，重试提示与实时看到的一致，没有生成中的文字。
        let reopened = stage(&world, &spec, &mut evidence, sid, tid);
        assert_eq!(reopened, closed);
        let after_restart = watch(&world, &spec, sid);
        assert!(!after_restart.view.head.as_ref().unwrap().owner);
        assert!(after_restart.view.texts.is_empty());
        assert!(
            after_restart
                .view
                .raw_entries()
                .into_iter()
                .eq(finished.iter())
        );
        assert_eq!(
            after_restart.view.entries[&2].step().unwrap().retries,
            live_retry
        );
        assert_eq!(provider.captured_requests().len(), 3);
        evidence
            .record_json("restart-reconnect", &json!(after_restart.received))
            .unwrap();
        // `history` 读不到当前最后一轮：先提交下一轮，以它的 UserMessage 为 `before` 重读这一轮，
        // 与观察者最后持有的条目逐字段相同。
        let next = cli(
            &world,
            &spec,
            &mut evidence,
            &["--json", "session", "send", "next turn", "--session", sid],
            0,
        );
        let next = parse_send_stdout(&next.stdout, next.exit_code());
        let next_user = &next.entries()[0];
        assert_eq!(next_user.user_ordinal(), Some(2));
        let reread =
            support::history(&world, timeout(&world, &spec), sid, next_user.node, 1).unwrap();
        assert_eq!(reread, finished);
        evidence
            .record_json("history-reread", &json!(reread))
            .unwrap();
        assert_eq!(provider.captured_requests().len(), 4);
        provider
            .shutdown(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
    }));
    support::finish_recorded(evidence, result);
}

#[test]
fn cancelled_slot_live_and_crash_share_ordered_closure() {
    for crash in [false, true] {
        let (world, spec) = support::direct_world(
            &format!("r2-cancel-slot-{crash}"),
            "slot",
            "[\"observe__workspace\",\"mutate__create\",\"fetch__web\"]",
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
            let (sid, tid) = running_turn(&world, &spec, &mut evidence);
            let (sid, tid) = (sid.as_str(), tid.as_str());
            let running = accepted_detail(&world, &spec, tid);
            let tip = history_tip(&world);
            assert_eq!(running["steps"][1]["slots"][0]["outcome"], "succeeded");
            let taken = cancel(&world, &spec, tid);
            assert_eq!(taken["value"]["owned"], true);
            evidence.record_json("cancel-taken", &taken).unwrap();
            let mut orphan_before = None;
            if crash {
                let marker = wait_for_ack(&ack, budget.cap_timeout(spec.step_timeout()));
                assert_eq!(marker["barrier"], "invocation_finished");
                assert_eq!(marker["part"], 2);
                assert_eq!(marker["turn_id"], tid);
                evidence.record_json("marker", &marker).unwrap();
                support::crash::kill(&world, daemon, front, &mut evidence, &budget);
                world.unset_env(BARRIER);
                world.unset_env(ACK);
                let orphan = stage(&world, &spec, &mut evidence, sid, tid);
                orphan_before = Some(orphan.clone());
                assert_eq!(orphan["summary"]["outcome"], "cancelled");
                assert_eq!(orphan["summary"]["progress"]["progress"], "executing");
                assert_eq!(
                    orphan["steps"][1]["slots"][1]["reason"],
                    json!({"kind":"indeterminate","value":"cancelled"})
                );
                assert_eq!(
                    orphan["steps"][1]["slots"][2]["progress"]["state"],
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
            let closed = stage(&world, &spec, &mut evidence, sid, tid);
            if let Some(before) = orphan_before {
                assert_eq!(
                    closed, before,
                    "rewind must preserve the cancelled slot and unclosed Step"
                );
            }
            assert_eq!(closed["summary"]["outcome"], "cancelled");
            assert_eq!(closed["summary"]["notice"]["level"], "info");
            assert_eq!(closed["steps"][0], running["steps"][0]);
            let state = &closed["steps"][1];
            assert_eq!(
                state["end"],
                json!({"end":"stop","cause":{"cause":"cancelled"}})
            );
            let slots = state["slots"].as_array().unwrap();
            assert_eq!(slots.len(), 3);
            assert_eq!(slots[0], running["steps"][1]["slots"][0]);
            assert_eq!(slots[1]["outcome"], "indeterminate");
            assert_eq!(
                slots[1]["reason"],
                json!({"kind":"indeterminate","value":"cancelled"})
            );
            assert_eq!(slots[2]["outcome"], "not_executed");
            assert!(slots[2]["reason"].is_null());
            // 实时取消与崩溃后都停在第二步：用户消息与两个 Step 各一条条目，末端 Step 带结局。
            let observer = watch(&world, &spec, sid);
            let entries = observer.view.raw_entries();
            assert_eq!(entries.len(), 3);
            assert_paired_entries(&entries, &closed);
            evidence
                .record_json("paired-entries", &json!(observer.received))
                .unwrap();
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
        "[\"observe__workspace\",\"mutate__create\"]",
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
        let (sid, tid) = running_turn(&world, &spec, &mut evidence);
        let (sid, tid) = (sid.as_str(), tid.as_str());
        let first = accepted_detail(&world, &spec, tid);
        assert_eq!(first["steps"][0]["end"], json!({"end":"continue"}));
        let tip = history_tip(&world);
        std::fs::write(schedule.join("calls_registered.arm"), b"").unwrap();
        gate.release_next(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        let paused = wait_for_ack(
            &schedule.join("calls_registered.ready"),
            budget.cap_timeout(spec.step_timeout()),
        );
        assert_eq!(paused["turn_id"], tid);
        let before = accepted_detail(&world, &spec, tid);
        assert!(
            before["steps"][1]["slots"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s["progress"]["state"] == "registered")
        );
        // 从已命中的登记点再等满冻结预算，确定 deadline 已过；不是用耗时猜并发。
        std::thread::sleep(Duration::from_secs(2));
        std::fs::write(schedule.join("calls_registered.release"), b"").unwrap();
        let marker = wait_for_ack(&ack, budget.cap_timeout(spec.step_timeout()));
        assert_eq!(marker["barrier"], "step_closed");
        assert_eq!(marker["turn_id"], tid);
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
        let closed = stage(&world, &spec, &mut evidence, sid, tid);
        assert_eq!(closed["summary"]["outcome"], "failed");
        assert_eq!(
            closed["steps"][1]["end"],
            json!({"end":"stop","cause":{"cause":"deadline"}})
        );
        assert_eq!(closed["steps"][0], first["steps"][0]);
        let slots = closed["steps"][1]["slots"].as_array().unwrap();
        assert_eq!(slots.len(), 2);
        for slot in slots {
            assert_eq!(slot["outcome"], "not_executed");
            assert!(slot["reason"].is_null());
        }
        let observer = watch(&world, &spec, sid);
        assert_paired_entries(&observer.view.raw_entries(), &closed);
        evidence
            .record_json("paired-entries", &json!(observer.received))
            .unwrap();
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
        assert_eq!(stage(&world, &spec, &mut evidence, sid, tid), closed);
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
        let (sid, tid) = running_turn(&world, &spec, &mut evidence);
        let (sid, tid) = (sid.as_str(), tid.as_str());
        assert_eq!(pause["turn_id"], tid);
        let before = accepted_detail(&world, &spec, tid);
        let taken = cancel(&world, &spec, tid);
        assert_eq!(
            taken,
            json!({"response":"cancelled","value":{"owned":true}})
        );
        assert_eq!(accepted_detail(&world, &spec, tid), before);
        std::fs::write(schedule.join("request_started.release"), b"").unwrap();
        let marker = wait_for_ack(&ack, budget.cap_timeout(spec.step_timeout()));
        assert_eq!(marker["turn_id"], tid);
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
        let reopened = stage(&world, &spec, &mut evidence, sid, tid);
        assert_eq!(reopened, before);
        assert_eq!(reopened["summary"]["outcome"], "failed");
        assert!(provider.captured_requests().is_empty());
        let next = cli(
            &world,
            &spec,
            &mut evidence,
            &["--json", "session", "send", "new input", "--session", sid],
            0,
        );
        let next = parse_send_stdout(&next.stdout, next.exit_code());
        let old = stage(&world, &spec, &mut evidence, sid, tid);
        assert_eq!(old, reopened, "new send must not modify the old Turn");
        assert_eq!(old["summary"]["outcome"], "failed");
        assert_eq!(old["summary"]["notice"]["level"], "error");
        assert_eq!(
            old["steps"][0]["end"],
            json!({"end":"stop","cause":{"cause":"process_exit"}})
        );
        assert_eq!(
            old["steps"][0]["attempts"][0]["end"]["cause"]["cause"],
            "process_exit"
        );
        let new = stage(
            &world,
            &spec,
            &mut evidence,
            sid,
            next.accepted_turn_id().unwrap(),
        );
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
        assert_eq!(tree["session"]["current"], 4);
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

/// 准入窗口：后台已登记在途的 TurnId、尚未确认提交连接仍在并准入之前，由 `admission_registered`
/// 暂停这次提交的任务，后台照常服务其他连接。
#[test]
fn admission_window_reports_in_flight_and_a_departed_submitter_is_not_admitted() {
    let (world, spec) = support::direct_world("admission-window", "admission", "[]", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        provenance(
            &world,
            &mut evidence,
            json!({"test":"admission_window_reports_in_flight_and_a_departed_submitter_is_not_admitted",
            "pause":"admission_registered","script":"plain admitted later"}),
        );
        let mut provider =
            support::wire_fake_llm(&world, "admission", SseScript::plain("admitted later"));
        let schedule = world.root().join("schedule");
        std::fs::create_dir(&schedule).unwrap();
        std::fs::write(schedule.join("admission_registered.arm"), b"").unwrap();
        world.set_env("LATTICE_TEST_SCHEDULE_DIR", schedule.display().to_string());
        let daemon = support::crash::start(&world, &spec, &budget);
        // 先用只读的 `status` 确认后台说同一协议；协议不符时在这里直接失败。
        let status = ipc(
            &world,
            timeout(&world, &spec),
            json!({"operation":"status"}),
        );
        assert_eq!(status["response"], "status", "{status}");
        let request = submit_request(
            &world,
            TURN,
            json!({"new":SESSION}),
            "admission window",
            Some("admission"),
        );
        let mut pending = Wire::connect(&world, timeout(&world, &spec), &request);
        // 标记出现之前这次提交不能已有回执：已回复说明后台没有在准入前暂停。
        let ready = schedule.join("admission_registered.ready");
        let until = deadline(&world, &spec);
        let marker = loop {
            if let Ok(bytes) = std::fs::read(&ready)
                && let Ok(marker) = serde_json::from_slice::<Value>(&bytes)
            {
                break marker;
            }
            if let Some(reply) = pending.next_within(Duration::from_millis(5)) {
                panic!("submission answered without pausing at admission_registered: {reply}");
            }
            assert!(
                Instant::now() < until,
                "admission_registered marker not reached"
            );
        };
        assert_eq!(marker["turn_id"], TURN);
        assert_eq!(marker["session_id"], SESSION);
        evidence.record_json("admission-pause", &marker).unwrap();
        // 请求仍在准入：查询报告在途，CLI 输出在途并退出 0。
        assert_eq!(
            turn_lookup(&world, timeout(&world, &spec), TURN),
            json!({"response":"turn","value":{"status":"in_flight"}})
        );
        let shown = cli(
            &world,
            &spec,
            &mut evidence,
            &["--json", "session", "turn", TURN],
            0,
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&shown.stdout).unwrap(),
            json!({"status":"in_flight","turn_id":TURN})
        );
        // 同一 TurnId 再提交是冲突；另一 TurnId 以 New(S) 提交同一个 S 是 Busy。
        let reused = ipc(&world, timeout(&world, &spec), request.clone());
        assert_eq!(reused["value"]["kind"], "conflict", "{reused}");
        assert_eq!(reused["value"]["conflict"], "turn_id_used", "{reused}");
        let racing = ipc(
            &world,
            timeout(&world, &spec),
            submit_request(
                &world,
                OTHER,
                json!({"new":SESSION}),
                "racing",
                Some("admission"),
            ),
        );
        assert_eq!(racing["value"]["kind"], "busy", "{racing}");
        evidence
            .record_json("window-replies", &json!({"reused":reused,"racing":racing}))
            .unwrap();
        // 提交端断开后放行：后台确认连接已不在，不准入；在途登记随之结束。
        drop(pending);
        std::fs::write(schedule.join("admission_registered.release"), b"").unwrap();
        let until = deadline(&world, &spec);
        let settled = loop {
            let lookup = turn_lookup(&world, timeout(&world, &spec), TURN);
            if lookup["value"]["status"] != "in_flight" {
                break lookup;
            }
            assert!(
                Instant::now() < until,
                "TurnId stayed in flight after the submitter left"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(settled["response"], "error", "{settled}");
        assert_eq!(settled["value"]["kind"], "not_found", "{settled}");
        let absent = ipc(
            &world,
            timeout(&world, &spec),
            json!({"operation":"watch","session_id":SESSION}),
        );
        assert_eq!(absent["value"]["kind"], "not_found", "{absent}");
        assert!(provider.captured_requests().is_empty());
        // 之后用新的 TurnId 以 New(S) 提交，正常准入并执行。
        let admitted = ipc(
            &world,
            timeout(&world, &spec),
            submit_request(
                &world,
                THIRD,
                json!({"new":SESSION}),
                "admitted",
                Some("admission"),
            ),
        );
        assert_eq!(admitted["value"]["acceptance"], "accepted", "{admitted}");
        assert_eq!(admitted["value"]["turn"]["session_id"], SESSION);
        let mut watcher = watch(&world, &spec, SESSION);
        watcher.until(
            deadline(&world, &spec),
            "the admitted Turn closes",
            |view| turn_closed(view, THIRD),
        );
        assert_eq!(provider.captured_requests().len(), 1);
        evidence
            .record_json(
                "after-release",
                &json!({"lookup":settled,"absent":absent,"admitted":admitted,"watch":watcher.received}),
            )
            .unwrap();
        stage(&world, &spec, &mut evidence, SESSION, THIRD);
        evidence.record_captures(provider.captured_requests());
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
        evidence.record_command("admission window backend stop", &stopped);
        assert!(stopped.is_clean_exit(0));
    }));
    support::finish_recorded(evidence, result);
}
