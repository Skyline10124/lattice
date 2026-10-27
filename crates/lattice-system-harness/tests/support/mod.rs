// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

#![allow(dead_code)]

#[cfg(feature = "crash-barriers")]
pub(crate) mod crash;
use lattice_system_harness::{
    AssertError, CapturedRequest, CommandOutcome, FAKE_API_KEY, FakeLlmServer, HarnessIntervention,
    LatticeCommand, LatticeDriver, ProcessTermination, ReceiptResult, ResponseGate,
    ScenarioEvidence, ScenarioSpec, ScenarioWorld, SendExecution, SendOutcome, SendRecord,
    SlotOutcome, SseScript, Submit, ViewState, parse_record,
};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};

pub(crate) fn driver() -> LatticeDriver {
    LatticeDriver::from_env().expect("LATTICE_BIN must point to a built lattice binary")
}

/// 调用者先记录原始命令，再核验 JSON 提交回执，避免失败时丢失证据。
pub(crate) fn parse_send_stdout(stdout: &[u8], exit_code: Option<i32>) -> SendOutcome {
    let parsed = lattice_system_harness::parse_send_stdout(stdout, exit_code);
    assert_eq!(parsed.receipt_error, None);
    parsed
}

pub(crate) fn assert_clean_send(execution: &SendExecution) -> Result<(), AssertError> {
    if execution.clean_exit_code().is_none() {
        return Err(AssertError::StdoutMissing {
            expected: format!("session send did not exit cleanly: {}", execution.summary()),
        });
    }
    if !execution.malformed_lines.is_empty() {
        return Err(AssertError::StdoutMissing {
            expected: format!(
                "session send emitted malformed JSONL: {:?}",
                execution.malformed_lines
            ),
        });
    }
    Ok(())
}

/// 第一条记录写明身份，回执接受同一身份，结局属于这一轮，且输出只含这一轮的条目。
pub(crate) fn assert_accepted_and_closed(outcome: &SendOutcome) -> Result<(), AssertError> {
    require(
        outcome.receipt_error.is_none(),
        format!("invalid submission receipt: {:?}", outcome.receipt_error),
    )?;
    let Some(SendRecord::Submit(Submit::Sending {
        turn_id,
        session_id,
        ..
    })) = outcome.records.first()
    else {
        return require(false, "first record must be the sending submit");
    };
    require(
        outcome.accepted_turn_id() == Some(turn_id.as_str())
            && outcome.accepted_session_id() == Some(session_id.as_str()),
        "acceptance must confirm the identity written before sending",
    )?;
    require(
        outcome
            .closed_result()
            .is_some_and(|(sid, tid, _)| sid == session_id && tid == turn_id),
        "closed result must belong to the submitted Turn",
    )?;
    // 逐条看原始记录：每条 `entry` 属于这一轮，每条 `text` 的节点是这一轮的条目节点。
    let mut nodes = std::collections::BTreeSet::new();
    for record in &outcome.records {
        if let SendRecord::Entry(entry) = record {
            require(
                entry.turn_id == *turn_id,
                format!("entry {} belongs to another Turn", entry.node),
            )?;
            nodes.insert(entry.node);
        }
    }
    require(
        outcome.records.iter().all(|record| match record {
            SendRecord::Text(text) => nodes.contains(&text.node),
            _ => true,
        }),
        "session send --json outputs only this Turn's text",
    )
}

/// 带 Tool 调用的步骤都在完成结局的最后一步之前，结局来自 Step 条目而不是瞬时事件。
pub(crate) fn assert_completed_after_tools(outcome: &SendOutcome) -> Result<(), AssertError> {
    let entries = outcome.entries();
    let last_tool = entries
        .iter()
        .rposition(|entry| entry.step().is_some_and(|step| !step.slots.is_empty()));
    let completed = entries.iter().rposition(|entry| {
        entry.step().is_some_and(|step| {
            step.summary
                .as_ref()
                .is_some_and(|summary| summary.outcome.as_str() == "completed")
        })
    });
    require(
        last_tool.is_some() && completed.is_some_and(|completed| last_tool < Some(completed)),
        "completed step must follow the last step with Tool calls",
    )?;
    require(outcome.has_completed(), "durable completed summary")
}

/// CLI 输出中这一轮的 `TurnSummary` 与各 Step 详情（按 `node` 覆盖后的最新值），
/// 与 `session turn` 详情中同名字段同形，用于核验实时观察与重新打开看到的相同。
pub(crate) fn live_turn(outcome: &SendOutcome) -> Value {
    let entries = outcome.entries();
    json!({
        "summary": entries.iter().rev().find_map(|entry| entry.raw.get("summary")),
        "steps": entries.iter().filter(|entry| entry.step().is_some())
            .map(|entry| &entry.raw["step"]).collect::<Vec<_>>(),
    })
}

/// 实时条目与重新读取的 `session turn` 详情给出同一结局与同一组 Step。
pub(crate) fn same_turn(live: &Value, detail: &Value) -> bool {
    live["summary"] == detail["summary"] && live["steps"] == detail["steps"]
}

/// 提交被后台明确拒绝时的错误类别。
pub(crate) fn rejected_kind(outcome: &SendOutcome) -> Option<&str> {
    match &outcome.receipt()?.result {
        ReceiptResult::Rejected(error) => Some(error.kind.as_str()),
        _ => None,
    }
}

pub(crate) fn assert_forced_kill(outcome: &CommandOutcome) {
    assert_eq!(
        outcome.termination,
        Some(ProcessTermination::Signaled(libc::SIGKILL))
    );
    assert_eq!(outcome.interventions, vec![HarnessIntervention::ForcedKill]);
    assert!(outcome.cleanup.forced);
    assert!(outcome.cleanup.succeeded(), "{:?}", outcome.cleanup);
}

/// 直接经 IPC 提交时的信任根：harness CA 的 DER，与提交端从 `SSL_CERT_FILE` 读到的相同。
pub(crate) fn harness_roots_der() -> Value {
    serde_json::json!([lattice_system_harness::TlsIdentity::shared()
        .expect("harness TLS identity")
        .ca_der()])
}

pub(crate) fn require(condition: bool, expected: impl Into<String>) -> Result<(), AssertError> {
    if condition {
        Ok(())
    } else {
        Err(AssertError::StdoutMissing {
            expected: expected.into(),
        })
    }
}

pub(crate) fn evidence(world: &ScenarioWorld, spec: &ScenarioSpec) -> ScenarioEvidence {
    ScenarioEvidence::new(world, &spec.name, [FAKE_API_KEY.to_string()]).expect("scenario evidence")
}

pub(crate) fn finish_scenario(
    mut evidence: ScenarioEvidence,
    commands: &[(&str, &CommandOutcome)],
    captures: Vec<CapturedRequest>,
    result: Result<(), AssertError>,
) {
    for (summary, outcome) in commands {
        evidence.record_command(*summary, outcome);
    }
    evidence.record_captures(captures);
    let primary = result.err().map(|error| error.to_string());
    evidence
        .finish_report(primary)
        .unwrap_or_else(|report| panic!("{report}"));
}

/// 复用 PTY 的公开读取采集，保留整棵树及其中每个 Turn 的操作员投影。
pub(crate) fn record_session(
    session: &lattice_system_harness::SessionSendDriver<'_>,
    evidence: &mut ScenarioEvidence,
    sid: &str,
    parameters: Value,
) {
    let log = session
        .run_command(&["--json", "session", "log", sid])
        .unwrap();
    evidence.record_command("--json session log", &log);
    assert!(log.is_clean_exit(0));
    let tree: Value = serde_json::from_slice(&log.stdout).unwrap();
    for args in [
        vec!["session", "log", sid],
        vec!["--json", "session", "list"],
        vec!["--json", "audit", "--tail"],
        vec!["--json", "lancet", "status"],
    ] {
        let out = session.run_command(&args).unwrap();
        evidence.record_command(args.join(" "), &out);
        assert!(out.is_clean_exit(0));
    }
    for node in tree["nodes"].as_array().unwrap() {
        if let Some(tid) = node["turn_start"]["turn_id"].as_str() {
            for args in [
                vec!["--json", "session", "turn", tid],
                vec!["--json", "audit", "replay", "--session", sid, "--turn", tid],
            ] {
                let out = session.run_command(&args).unwrap();
                evidence.record_command(args.join(" "), &out);
                assert!(out.is_clean_exit(0));
            }
        }
    }
    evidence.record_json("state-summary", &tree).unwrap();
    evidence.record_json("parameters", &parameters).unwrap();
}

/// 失败时写入失败报告并 panic；成功时只排空场景后台，不写产物。
pub(crate) fn finish_recorded(evidence: ScenarioEvidence, result: std::thread::Result<()>) {
    let primary = result.err().map(|error| {
        error
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| error.downcast_ref::<&str>().map(|text| text.to_string()))
            .unwrap_or_else(|| "scenario panicked with a non-text payload".into())
    });
    evidence
        .finish_report(primary)
        .unwrap_or_else(|report| panic!("{report}"));
}
pub(crate) fn wire_fake_llm(
    world: &ScenarioWorld,
    agent: &str,
    script: SseScript,
) -> FakeLlmServer {
    let server = FakeLlmServer::start(script).expect("start fake llm");
    world.set_env("OPENAI_COMPATIBLE_BASE_URL", server.base_url());
    world.set_env("OPENAI_COMPATIBLE_MODEL", agent);
    world.set_env("OPENAI_COMPATIBLE_API_KEY", FAKE_API_KEY);
    server
}

pub(crate) fn wire_gated_fake_llm(
    world: &ScenarioWorld,
    agent: &str,
    script: SseScript,
) -> (FakeLlmServer, ResponseGate) {
    let (server, gate) = FakeLlmServer::start_gated(script).expect("start gated fake llm");
    world.set_env("OPENAI_COMPATIBLE_BASE_URL", server.base_url());
    world.set_env("OPENAI_COMPATIBLE_MODEL", agent);
    world.set_env("OPENAI_COMPATIBLE_API_KEY", FAKE_API_KEY);
    (server, gate)
}

pub(crate) fn direct_world(
    prefix: &str,
    agent: &str,
    tools: &str,
    extra_profile: &str,
) -> (ScenarioWorld, ScenarioSpec) {
    let spec = ScenarioSpec::direct_local(prefix);
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let toml = format!(
        "schema_version = 1\n\n\
         [agent]\nname = \"{agent}\"\nmodel = \"compat:{agent}\"\n\n\
         [prompt]\nsystem = \"You are a concise test agent.\"\n\n\
         [tools]\nenabled = {tools}\n{extra_profile}"
    );
    world
        .write_project_file(&format!(".lattice/agents/{agent}/agent.toml"), &toml)
        .expect("write agent.toml");
    world.init_git_project().expect("init git");
    (world, spec)
}

pub(crate) fn last_tool_content(capture: &CapturedRequest) -> Option<String> {
    capture
        .raw
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages
                .iter()
                .rev()
                .find(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
                .and_then(|message| message.get("content").and_then(Value::as_str))
                .map(str::to_string)
        })
}

pub(crate) fn tool_result_value(capture: &CapturedRequest) -> Option<Value> {
    let content = last_tool_content(capture)?;
    if content.starts_with('{') {
        serde_json::from_str(&content).ok()
    } else {
        None
    }
}

/// 这一轮有名为 `name` 的调用且结论不是 Succeeded。
pub(crate) fn tool_failed(outcome: &SendOutcome, name: &str) -> bool {
    outcome
        .slots()
        .iter()
        .any(|slot| slot.method == name && slot.outcome != SlotOutcome::Succeeded)
}

pub(crate) fn tool_ok(outcome: &SendOutcome, name: &str) -> bool {
    outcome
        .slots()
        .iter()
        .any(|slot| slot.method == name && slot.outcome == SlotOutcome::Succeeded)
}

/// `method` 的某次调用以 Failed 结束，原因中的 CEP 错误码为 `code`（snake_case）。
pub(crate) fn finished_with_code(outcome: &SendOutcome, method: &str, code: &str) -> bool {
    outcome.slots().iter().any(|slot| {
        slot.method == method
            && slot.outcome == SlotOutcome::Failed
            && slot.error_code.as_deref() == Some(code)
    })
}

/// 公开会话条目中同一失败调用的原始槽位和完整 CEP 错误。
pub(crate) fn tool_failure<'a>(
    outcome: &'a SendOutcome,
    method: &str,
) -> Option<(&'a Value, &'a Value)> {
    outcome.entries().into_iter().find_map(|entry| {
        entry
            .raw
            .pointer("/content/slots")?
            .as_array()?
            .iter()
            .find_map(|slot| {
                let result = slot.pointer("/progress/result")?;
                (slot.pointer("/binding/identity/method")?.as_str()? == method
                    && result["name"] == method
                    && slot["progress"]["progress"] == "terminal"
                    && result["payload"]["outcome"] == "failed")
                    .then(|| (slot, &result["payload"]["payload"]))
            })
    })
}

pub(crate) fn tool_call(id: &str, name: &str, args: &str) -> SseScript {
    SseScript::ToolCall {
        call_id: id.into(),
        tool_name: name.into(),
        arguments_json: args.into(),
    }
}

pub(crate) fn sequence(scripts: Vec<SseScript>) -> SseScript {
    SseScript::Sequence { scripts }
}

pub(crate) fn cli(world: &ScenarioWorld, spec: &ScenarioSpec, args: &[&str]) -> CommandOutcome {
    driver()
        .run(
            world,
            &LatticeCommand {
                args: args.iter().map(|arg| arg.to_string()).collect(),
                timeout: Some(spec.step_timeout()),
            },
        )
        .expect("drive lattice")
}

/// 公开 Local IPC 的一条连接：请求写成一帧，回复按 `LTLOCAL5` 帧重组为逻辑消息。
pub(crate) struct Wire {
    stream: UnixStream,
    message: u64,
    timeout: Duration,
}
impl Wire {
    pub(crate) fn connect(world: &ScenarioWorld, timeout: Duration, request: &Value) -> Self {
        let mut stream = UnixStream::connect(world.session_store_dir().join("local/api.sock"))
            .expect("connect Local IPC");
        stream.set_read_timeout(Some(timeout)).unwrap();
        stream.set_write_timeout(Some(timeout)).unwrap();
        let body = serde_json::to_vec(request).unwrap();
        stream.write_all(b"LTLOCAL5").unwrap();
        stream
            .write_all(&(body.len() as u32).to_be_bytes())
            .unwrap();
        stream.write_all(&body).unwrap();
        Self {
            stream,
            message: 0,
            timeout,
        }
    }
    pub(crate) fn next(&mut self) -> Value {
        self.message(None)
    }
    /// `wait` 内没有新消息时返回 None；一旦开始收到就读完整条消息，不留半帧。
    pub(crate) fn next_within(&mut self, wait: Duration) -> Option<Value> {
        self.stream.set_read_timeout(Some(wait)).unwrap();
        let mut first = [0; 1];
        let read = self.stream.read(&mut first);
        self.stream.set_read_timeout(Some(self.timeout)).unwrap();
        match read {
            Ok(1) => Some(self.message(Some(first[0]))),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                None
            }
            other => panic!("read an LTLOCAL5 frame header: {other:?}"),
        }
    }
    /// 读一条逻辑消息；`first` 是已读出的帧头首字节。
    fn message(&mut self, mut first: Option<u8>) -> Value {
        let mut logical = String::new();
        loop {
            let mut header = [0; 12];
            let rest = match first.take() {
                Some(byte) => {
                    header[0] = byte;
                    &mut header[1..]
                }
                None => &mut header[..],
            };
            self.stream
                .read_exact(rest)
                .expect("read an LTLOCAL5 frame header");
            assert_eq!(&header[..8], b"LTLOCAL5");
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

pub(crate) fn ipc(world: &ScenarioWorld, timeout: Duration, request: Value) -> Value {
    Wire::connect(world, timeout, &request).next()
}

/// 提交端加载好的冻结配置：项目中的 profile 原文与指向 fake provider 的连接材料。
pub(crate) fn configuration(world: &ScenarioWorld, agent: &str, base_url: &str) -> Value {
    json!({"profile_toml":std::fs::read_to_string(world.project_dir()
        .join(format!(".lattice/agents/{agent}/agent.toml"))).unwrap(),
        "system_prompt":"You are a concise test agent.","connection":{"alias":format!("compat:{agent}"),
        "provider":"openai_compatible","provider_model":agent,"base_url":base_url,
        "api_key":FAKE_API_KEY,"roots_der":harness_roots_der()}})
}

/// `session` 写作 `{"new": S}` 或 `{"existing": S}`；身份都由客户端生成。
pub(crate) fn submit_request(
    world: &ScenarioWorld,
    turn_id: &str,
    session: Value,
    input: &str,
    agent: Option<&str>,
    configuration: Value,
) -> Value {
    json!({"operation":"submit","request":{"turn_id":turn_id,"session":session,
        "workspace":world.project_dir(),"input":input,"agent":agent},"configuration":configuration})
}

/// 按 TurnId 查询：已接受、在途，或 `not_found` 错误。
pub(crate) fn turn_lookup(world: &ScenarioWorld, timeout: Duration, turn_id: &str) -> Value {
    ipc(
        world,
        timeout,
        json!({"operation":"turn","turn_id":turn_id}),
    )
}

/// `history` 查询；成功时返回条目数组，否则返回整条回复供场景断言错误。
pub(crate) fn history(
    world: &ScenarioWorld,
    timeout: Duration,
    session_id: &str,
    before: u64,
    turns: u32,
) -> Result<Vec<Value>, Value> {
    let response = ipc(
        world,
        timeout,
        json!({"operation":"history","session_id":session_id,"before":before,"turns":turns}),
    );
    match response["value"]["entries"].as_array() {
        Some(entries) if response["response"] == "history" => Ok(entries.clone()),
        _ => Err(response),
    }
}

/// 一个 `watch` 观察者：逐条读取 `view` 消息，维护它持有的视图并保留原始消息作证据。
pub(crate) struct Watcher {
    wire: Wire,
    pub(crate) view: ViewState,
    pub(crate) received: Vec<Value>,
}
impl Watcher {
    pub(crate) fn open(world: &ScenarioWorld, timeout: Duration, session_id: &str) -> Self {
        Self {
            wire: Wire::connect(
                world,
                timeout,
                &json!({"operation":"watch","session_id":session_id}),
            ),
            view: ViewState::default(),
            received: Vec::new(),
        }
    }
    /// 应用一条视图消息；同一节点不延续旧值或文字偏移不接续即失败。
    fn accept(&mut self, message: Value) -> SendRecord {
        assert_eq!(message["response"], "view", "{message}");
        let record = parse_record(&message["value"]).unwrap_or_else(|e| panic!("{e}: {message}"));
        self.view
            .apply(&record)
            .unwrap_or_else(|e| panic!("{e}: {message}"));
        self.received.push(message);
        record
    }
    /// 读完开头的会话头与最近几轮。开头这批条目之后没有结束标记，
    /// 以收到 `head.position` 对应的条目为准；整批受一个截止时间约束。
    pub(crate) fn initial(&mut self) -> &ViewState {
        let deadline = Instant::now() + self.wire.timeout;
        self.until(
            deadline,
            "the initial head and its position entry",
            |view| {
                view.head.as_ref().is_some_and(|head| {
                    head.position == 0 || view.entries.contains_key(&head.position)
                })
            },
        )
    }
    /// 读到持有的视图满足条件为止；超过截止时间即失败，并报告等待的条件与最后持有的视图。
    pub(crate) fn until(
        &mut self,
        deadline: Instant,
        condition: &str,
        predicate: impl Fn(&ViewState) -> bool,
    ) -> &ViewState {
        while !predicate(&self.view) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match (!remaining.is_zero())
                .then(|| self.wire.next_within(remaining))
                .flatten()
            {
                Some(message) => {
                    self.accept(message);
                }
                None => {
                    let held = describe(&self.view);
                    panic!("watch did not reach `{condition}` before the deadline; held {held}")
                }
            }
        }
        &self.view
    }
    /// 读完 `wait` 内到达的消息；可以没有消息，收到的每条仍须延续已持有的值。
    pub(crate) fn drain(&mut self, wait: Duration) -> &ViewState {
        while let Some(message) = self.wire.next_within(wait) {
            self.accept(message);
        }
        &self.view
    }
}

/// 超时报告用的视图摘要：会话头、各条目的阶段与结局、各尝试已收到的文字字节数。
fn describe(view: &ViewState) -> String {
    let entries: Vec<String> = view
        .entries
        .values()
        .map(|entry| {
            let node = entry.node;
            match entry.step() {
                Some(step) => {
                    let progress = &step.progress;
                    let outcome = step.summary.as_ref().map(|summary| summary.outcome);
                    format!("{node}:step {progress:?} summary={outcome:?}")
                }
                None => format!("{node}:user"),
            }
        })
        .collect();
    let texts: Vec<String> = view
        .texts
        .iter()
        .map(|((node, attempt), segments)| {
            let bytes: usize = segments.iter().map(|segment| segment.text.len()).sum();
            format!("{node}/{attempt}:{bytes}B")
        })
        .collect();
    let head = &view.head;
    format!("head={head:?} entries={entries:?} texts={texts:?}")
}

/// 视图中这一轮的最后一个 Step 条目已带关闭的结局。
pub(crate) fn turn_closed(view: &ViewState, turn_id: &str) -> bool {
    view.entries.values().any(|entry| {
        entry.turn_id == turn_id
            && entry.step().is_some_and(|step| {
                step.summary
                    .as_ref()
                    .is_some_and(|summary| summary.progress.is_closed())
            })
    })
}
