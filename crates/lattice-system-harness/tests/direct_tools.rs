// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct-Local fetch, todo, and generated-catalog scenarios (issue #46 slice 4,
//! Codeberg #24).
//!
//! Completes built-binary coverage of the default five-Tool distribution.
//!
//! `fetch.web` is driven against a harness-only localhost HTTP fixture
//! (not the OpenAI-compatible fake LLM). Tool results are inspected in the
//! next captured provider request.

mod support;

use std::time::Duration;

use lattice_system_harness::{
    AssertError, CapturedRequest, FixtureHttpServer, HttpFixture, SendOutcome, SessionSendDriver,
    SlotOutcome, SseScript,
};
use serde_json::Value;
use support::{
    assert_clean_send, assert_completed_after_tools, cli, direct_world, driver, evidence,
    finish_scenario, require, sequence, tool_call, tool_failed, tool_failure, tool_ok,
    tool_result_value, wire_fake_llm,
};

fn fetch_network_profile(max_bytes: u64) -> String {
    format!(
        "\n[lancet.network]\nallow_localhost = true\nallowed_hosts = [\"localhost\"]\nmax_web_response_bytes = {max_bytes}\n\n[lancet.capabilities]\nmax_boundary = \"ex\"\n"
    )
}

/// `method` 第一次调用在 Step 条目中的原始槽位详情，以及回复里同一调用的 InvocationId。
fn slot_detail<'a>(outcome: &'a SendOutcome, method: &str) -> Option<(&'a Value, &'a Value)> {
    outcome.entries().into_iter().find_map(|entry| {
        let slot = entry.raw["step"]["slots"]
            .as_array()?
            .iter()
            .find(|slot| slot["binding"]["method"] == method)?;
        let call = entry.raw["content"]["reply"]["parts"]
            .as_array()?
            .iter()
            .find(|part| part["semantic"]["tool_call"]["call"]["name"] == method)?;
        Some((
            slot,
            &call["semantic"]["tool_call"]["call"]["invocation_id"],
        ))
    })
}

/// Encoded tool names as advertised on the Chat Completions wire (`a.b` → `a__b`).
fn wire_tool_names(capture: &CapturedRequest) -> Vec<String> {
    capture.tool_names.clone()
}

/// 两次真实本地 GET 分别证明完整响应和按字节截断；类型必须原样保留。
#[test]
fn direct_tools_fetch_web_localhost_success_and_cap() {
    let driver = driver();
    let web = FixtureHttpServer::start(
        HttpFixture::plain("fixture body for fetch")
            .with_content_type(Some("text/plain; charset=utf-8")),
    )
    .expect("fixture http");
    // 第 64 字节落在多字节字符内，产品须退回 UTF-8 边界，不加省略标记。
    let capped_web = FixtureHttpServer::start(
        HttpFixture::plain(format!("{}界z", "a".repeat(63)))
            .with_content_type(Some("text/plain; charset=utf-8")),
    )
    .expect("capped fixture http");
    let url = web.url_path("/fixture-path");
    let capped_url = capped_web.url_path("/capped-path");
    let (world, spec) = direct_world(
        "direct-tools-fetch-ok",
        "dl-fetch",
        "[\"fetch.web\"]",
        &fetch_network_profile(64),
    );
    let args = format!(r#"{{"url":"{url}"}}"#);
    let capped_args = format!(r#"{{"url":"{capped_url}"}}"#);
    let server = wire_fake_llm(
        &world,
        "dl-fetch",
        sequence(vec![
            tool_call("f1", "fetch.web", &args),
            SseScript::plain("fetched"),
            tool_call("f2", "fetch.web", &capped_args),
            SseScript::plain("capped"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-fetch");
    let outcome = send.send("fetch the fixture").expect("turn");
    let capped = send.send("fetch the capped fixture").expect("capped turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        assert_clean_send(&capped)?;
        require(tool_ok(&outcome.parsed, "fetch.web"), "fetch.web ok")?;
        require(tool_ok(&capped.parsed, "fetch.web"), "capped fetch.web ok")?;
        require(
            outcome.has_completed() && capped.has_completed(),
            "both fetch turns completed",
        )?;
        for (fixture, path) in [(&web, "/fixture-path"), (&capped_web, "/capped-path")] {
            fixture
                .wait_for_requests(1, Duration::from_secs(5))
                .map_err(|e| AssertError::StdoutMissing {
                    expected: format!("fixture request: {e}"),
                })?;
            let http = fixture.captured_requests();
            require(
                http.len() == 1,
                format!("expected one HTTP request: {http:?}"),
            )?;
            require(
                http[0].method == "GET" && http[0].path == path,
                format!("unexpected fixture request: {http:?}"),
            )?;
            require(
                fixture.first_error().is_none(),
                format!("HTTP fixture failed: {:?}", fixture.first_error()),
            )?;
        }
        require(
            captures.len() == 4,
            "two fetch rounds and two recovery requests required",
        )?;
        for (index, expected_body, truncated) in [
            (1, "fixture body for fetch".to_string(), false),
            (3, "a".repeat(63), true),
        ] {
            let fetched = tool_result_value(&captures[index]).expect("fetch result JSON");
            require(
                fetched["status"] == 200,
                format!("status not 200: {fetched}"),
            )?;
            require(
                fetched["content_type"] == "text/plain; charset=utf-8",
                format!("exact Content-Type required: {fetched}"),
            )?;
            require(
                fetched["truncated"].as_bool() == Some(truncated),
                format!("fetch {index} truncation must be {truncated}: {fetched}"),
            )?;
            require(
                fetched["body"].as_str() == Some(expected_body.as_str()),
                format!("fetch {index} exact body required: {fetched}"),
            )?;
        }
        Ok(())
    })();
    finish_scenario(
        scenario_evidence,
        &[
            ("fetch.web complete", &outcome.command),
            ("fetch.web capped", &capped.command),
        ],
        captures,
        result,
    );
}

/// `fetch.web` omitted from enabled tools: forced model call fails before CEP
/// dispatch and the fixture receives no request.
#[test]
fn direct_tools_fetch_disabled_no_dispatch() {
    let driver = driver();
    let web = FixtureHttpServer::start(HttpFixture::plain("should-not-be-hit")).expect("http");
    let url = web.url_path("/nope");
    // tools.enabled empty — fetch not advertised / not executable.
    let (world, spec) = direct_world(
        "direct-tools-fetch-off",
        "dl-foff",
        "[]",
        &fetch_network_profile(1024),
    );
    let args = format!(r#"{{"url":"{url}"}}"#);
    let server = wire_fake_llm(
        &world,
        "dl-foff",
        sequence(vec![
            tool_call("f1", "fetch.web", &args),
            SseScript::plain("could not fetch"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-foff");
    let outcome = send.send("try fetch").expect("turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        // The provider event announces the requested ToolCall, but disabled
        // methods fail closed before binding/CEP dispatch and need not emit a
        // synthetic ToolCallFinished event.
        require(
            !tool_ok(&outcome.parsed, "fetch.web"),
            "disabled fetch.web must not succeed",
        )?;
        require(
            web.captured_requests().is_empty(),
            format!(
                "fixture must receive no request when fetch disabled: {:?}",
                web.captured_requests()
            ),
        )?;
        // 未启用的工具不得进入 CEP 分发：fetch.web 槽位都没有开始执行。
        require(
            outcome
                .slots()
                .iter()
                .filter(|slot| slot.method == "fetch.web")
                .all(|slot| slot.outcome == SlotOutcome::NotExecuted),
            "disabled fetch.web must not be dispatched",
        )?;
        // 目录外调用整批拒绝，保存类型化原因并将 Turn 正常关闭为失败，
        // 不留下未关闭任务，也不为该批次追加模型请求。
        require(
            outcome
                .closed_result()
                .is_some_and(|(_, _, result)| result == "failed"),
            format!("disabled fetch.web must close the Turn failed: {outcome:?}"),
        )?;
        require(
            !outcome.has_completed(),
            "a rejected batch must not commit a completed Turn",
        )?;
        require(
            captures.len() == 1,
            format!("expected exactly one model request, got {}", captures.len()),
        )?;
        let turn_id = outcome
            .accepted_turn_id()
            .expect("accepted TurnId")
            .to_string();
        let view = cli(&world, &spec, &["--json", "session", "turn", &turn_id]);
        require(view.is_clean_exit(0), "turn detail clean read")?;
        let detail: Value = serde_json::from_slice(&view.stdout).expect("turn detail");
        require(
            detail["summary"]["outcome"] == "failed",
            "rejected batch closes Failed",
        )?;
        require(
            detail["steps"][0]["end"]["cause"]["cause"] == "invalid_batch",
            "durable invalid batch",
        )?;
        require(
            detail["steps"][0]["slots"]
                .as_array()
                .is_some_and(Vec::is_empty),
            "no slots for rejected batch",
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("fetch disabled", &outcome.command)],
        captures,
        result,
    );
}

/// Enabled fetch rejected by host policy: CEP denial, fixture receives no request.
#[test]
fn direct_tools_fetch_host_policy_denial() {
    let driver = driver();
    let web = FixtureHttpServer::start(HttpFixture::plain("blocked")).expect("http");
    let url = web.url_path("/secret");
    // allow_localhost false / empty allowlist — host denied.
    let (world, spec) = direct_world(
        "direct-tools-fetch-deny",
        "dl-fdeny",
        "[\"fetch.web\"]",
        "\n[lancet.network]\nallow_localhost = false\nallowed_hosts = []\n\n[lancet.capabilities]\nmax_boundary = \"ex\"\n",
    );
    let args = format!(r#"{{"url":"{url}"}}"#);
    let server = wire_fake_llm(
        &world,
        "dl-fdeny",
        sequence(vec![
            tool_call("f1", "fetch.web", &args),
            SseScript::plain("denied"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-fdeny");
    let outcome = send.send("fetch denied host").expect("turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        require(
            tool_failed(&outcome.parsed, "fetch.web"),
            "policy-denied fetch must fail tool",
        )?;
        require(
            web.captured_requests().is_empty(),
            format!(
                "policy denial must not hit fixture: {:?}",
                web.captured_requests()
            ),
        )?;
        let accepts = web
            .accepted_connections_after_probe(Duration::from_secs(5))
            .map_err(|error| AssertError::StdoutMissing {
                expected: format!("accept barrier failed: {error}"),
            })?;
        require(
            accepts == 0,
            format!("host denial must have zero accepts, got {accepts}"),
        )?;
        require(
            web.first_error().is_none(),
            format!("HTTP fixture failed: {:?}", web.first_error()),
        )?;
        require(
            captures.len() == 2,
            format!(
                "host denial requires exactly two provider requests, got {}",
                captures.len()
            ),
        )?;
        let error = tool_result_value(&captures[1]).expect("host denial tool-role JSON");
        require(
            error["error"]["code"] == -32012,
            format!("host policy must return AmbientAuthorityDenied in tool result: {error}"),
        )?;
        let failure = tool_failure(&outcome.parsed, "fetch.web");
        require(
            failure.is_some_and(|(_, error)| {
                error["code"] == "ambient_authority_denied"
                    && error["data"]["kind"] == "permission"
                    && error["data"]["required_capability"] == "network.fetch"
            }),
            format!("typed host-policy denial required: {failure:?}"),
        )?;
        assert_completed_after_tools(&outcome.parsed)?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("fetch host denial", &outcome.command)],
        captures,
        result,
    );
}

/// Explicit `network.fetch` denial rejects the Component at Cell admission before outbound I/O.
/// Distinct from host-policy denial (`allow_localhost=false` against a localhost URL).
#[test]
fn direct_tools_fetch_capability_denial_without_outbound_io() {
    let driver = driver();
    let web = FixtureHttpServer::start(HttpFixture::plain("must not be fetched")).expect("http");
    let url = web.url_path("/capability-denied");
    let args = format!(r#"{{"url":"{url}"}}"#);
    let (world, spec) = direct_world(
        "direct-tools-fetch-cap",
        "dl-fcap",
        "[\"fetch.web\"]",
        "\n[lancet.network]\nallow_localhost = true\nallowed_hosts = [\"localhost\"]\n\n[lancet.capabilities]\ndeny = [\"network.fetch\"]\nmax_boundary = \"ex\"\n",
    );
    let server = wire_fake_llm(
        &world,
        "dl-fcap",
        sequence(vec![
            tool_call("f1", "fetch.web", &args),
            SseScript::plain("denied without fetch capability"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-fcap");
    let outcome = send.send("fetch the local fixture").expect("turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        let accepts = web
            .accepted_connections_after_probe(Duration::from_secs(5))
            .map_err(|error| AssertError::StdoutMissing {
                expected: format!("accept barrier failed: {error}"),
            })?;
        require(
            accepts == 0,
            format!("capability denial must have zero accepts, got {accepts}"),
        )?;
        require(
            web.captured_requests().is_empty(),
            format!(
                "capability denial must have zero requests: {:?}",
                web.captured_requests()
            ),
        )?;
        require(
            web.first_error().is_none(),
            format!("HTTP fixture failed: {:?}", web.first_error()),
        )?;
        require(
            tool_failed(&outcome.parsed, "fetch.web"),
            "fetch.web must fail when network.fetch is denied",
        )?;
        // 准入证据随槽位保存：调用身份是类型化 InvocationId，准入移除了唯一的能力。
        let slot = slot_detail(&outcome, "fetch.web");
        require(
            slot.is_some_and(|(slot, call_id)| {
                slot["invocation_id"]
                    .as_str()
                    .is_some_and(|id| id.len() == 36)
                    && slot["invocation_id"] == *call_id
                    && slot["evidence"]["kind"] == "recorded"
                    && slot["evidence"]["value"]["admission"]["tool"] == "fetch.web"
                    && slot["evidence"]["value"]["admission"]["approved_capabilities"]
                        .as_array()
                        .is_some_and(|approved| {
                            !approved
                                .iter()
                                .any(|capability| capability == "network.fetch")
                        })
            }),
            format!("fetch.web slot must carry typed identity and admission evidence: {slot:?}"),
        )?;
        // The Component never reaches the network host import when admission removes
        // its sole capability.
        require(
            captures.len() == 2,
            "capability denial needs tool result capture",
        )?;
        let error = tool_result_value(&captures[1]).expect("capability denial tool-role JSON");
        require(
            error["error"]["code"] == -32010,
            format!("network.fetch admission must return CapabilityNotGranted: {error}"),
        )?;
        let failure = tool_failure(&outcome.parsed, "fetch.web");
        require(
            failure.is_some_and(|(slot, error)| {
                error["code"] == "capability_not_granted"
                    && error["data"]["kind"] == "permission"
                    && slot["progress"]["evidence"]["value"]["admission"]["denied_capabilities"]
                        == serde_json::json!(["network.fetch"])
            }),
            format!("typed network.fetch admission refusal required: {failure:?}"),
        )?;
        require(
            !tool_ok(&outcome.parsed, "fetch.web"),
            "capability denial must not succeed fetch.web",
        )?;
        // 最后一个 Step 条目核验本次 Turn 的真实结局，不依赖瞬时终结事件。
        assert_completed_after_tools(&outcome.parsed)?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("fetch capability denial", &outcome.command)],
        captures,
        result,
    );
}

/// `session.todo_write` success echoes canonical todo JSON into the next model
/// request — executed by the **default binary** through its embedded WASM
/// Component (#29 criterion 9).
///
/// The lifecycle stages prove the call traversed the Cell-governed Component
/// path and the generated catalog embedded the Tool.
#[test]
fn direct_tools_todo_write_success() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-tools-todo-ok",
        "dl-todo",
        "[\"session.todo_write\"]",
        "",
    );
    let server = wire_fake_llm(
        &world,
        "dl-todo",
        sequence(vec![
            tool_call(
                "t1",
                "session.todo_write",
                r#"{"todos":[{"content":"step one","status":"pending"},{"content":"step two","status":"in_progress"}]}"#,
            ),
            SseScript::plain("todos recorded"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-todo");
    let outcome = send.send("write todos").expect("turn");
    let status = send
        .run_command(&["--json", "lancet", "status"])
        .expect("lancet status after todo");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        require(
            tool_ok(&outcome.parsed, "session.todo_write"),
            "todo_write ok",
        )?;
        require(outcome.has_completed(), "turn completed")?;
        // 公开的调用身份是类型化 InvocationId：槽位与回复中的调用一致，
        // 槽位停在 Terminal 并带有经 Cell 准入的证据。
        let slot = slot_detail(&outcome, "session.todo_write");
        require(
            slot.is_some_and(|(slot, call_id)| {
                slot["invocation_id"]
                    .as_str()
                    .is_some_and(|id| id.len() == 36)
                    && slot["invocation_id"] == *call_id
                    && slot["progress"]["state"] == "terminal"
                    && slot["evidence"]["kind"] == "recorded"
                    && slot["evidence"]["value"]["admission"]["tool"] == "session.todo_write"
            }),
            format!("todo_write slot must carry typed identity and admission evidence: {slot:?}"),
        )?;
        require(captures.len() >= 2, "need tool result in next request")?;
        let todos = tool_result_value(&captures[1]).expect("todo result JSON");
        let list = todos
            .get("todos")
            .or_else(|| todos.pointer("/result/todos"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        require(list.len() == 2, format!("expected 2 todos: {todos}"))?;
        let contents: Vec<&str> = list
            .iter()
            .filter_map(|t| t.get("content").and_then(Value::as_str))
            .collect();
        require(
            contents.contains(&"step one") && contents.contains(&"step two"),
            format!("todo contents missing: {contents:?}"),
        )?;
        // Next model request body should carry the tool result (already asserted via captures[1]).
        let body = captures[1].raw.to_string();
        require(
            body.contains("step one") && body.contains("in_progress"),
            format!("next request lacked todo JSON: {body}"),
        )?;
        require(
            status.is_clean_exit(0),
            format!(
                "lancet status failed: {}",
                String::from_utf8_lossy(&status.stderr)
            ),
        )?;
        let status_json: Value =
            serde_json::from_slice(&status.stdout).map_err(|error| AssertError::StdoutMissing {
                expected: format!("lancet status --json not JSON: {error}"),
            })?;
        let recent_calls = status_json
            .get("recent")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        require(
            recent_calls.iter().any(|call| {
                call.pointer("/binding/method").and_then(Value::as_str)
                    == Some("session.todo_write")
                    && call.pointer("/state/outcome").and_then(Value::as_str) == Some("succeeded")
            }) && status_json["recent"][0]["state"]["outcome"] == "succeeded",
            format!("lancet status omitted typed todo terminal: {status_json}"),
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[
            ("todo_write success", &outcome.command),
            ("lancet status after todo", &status),
        ],
        captures,
        result,
    );
}

/// Todo validation rejection (two `in_progress`) produces failed tool and no
/// successful commit of hidden state claims.
#[test]
fn direct_tools_todo_write_validation_rejection() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-tools-todo-bad",
        "dl-todobad",
        "[\"session.todo_write\"]",
        "",
    );
    let server = wire_fake_llm(
        &world,
        "dl-todobad",
        sequence(vec![
            tool_call(
                "t1",
                "session.todo_write",
                r#"{"todos":[{"content":"a","status":"in_progress"},{"content":"b","status":"in_progress"}]}"#,
            ),
            SseScript::plain("rejected"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-todobad");
    let outcome = send.send("bad todos").expect("turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        require(
            tool_failed(&outcome.parsed, "session.todo_write"),
            "two in_progress must fail validation",
        )?;
        require(
            captures.len() == 2,
            format!(
                "todo rejection requires exactly two provider requests, got {}",
                captures.len()
            ),
        )?;
        let error = tool_result_value(&captures[1]).expect("todo validation tool-role JSON");
        require(
            error["error"]["code"] == -32001,
            format!("todo schema error code required: {error}"),
        )?;
        let failure = tool_failure(&outcome.parsed, "session.todo_write");
        require(
            failure.is_some_and(|(_, error)| {
                error["code"] == "schema_mismatch"
                    && error["data"]["kind"] == "validation"
                    && error["data"]["field"] == "params"
                    && error["data"]["reason"] == "invalid params"
            }),
            format!("typed todo schema mismatch required: {failure:?}"),
        )?;
        // validation 数据只有通用参数分类；两个位置才区分双 in_progress 与其他 schema 拒绝。
        let locations = error["error"]["message"].as_str().and_then(|message| {
            let (instance, schema) = message.split_once(" at ")?.1.split_once(" (schema ")?;
            Some((instance, schema.strip_suffix(')')?))
        });
        require(
            locations == Some(("/todos", "/properties/todos/not")),
            format!("two in_progress schema locations required: {locations:?}; {error}"),
        )?;
        assert_completed_after_tools(&outcome.parsed)?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("todo validation reject", &outcome.command)],
        captures,
        result,
    );
}

/// #29 criterion 8: the Component Tool's result survives into the next turn's
/// transcript after the session is reopened.
///
/// The Component result reaches the model through the persisted tool-role
/// message. Reopening the session proves the result did not live only in an
/// in-process channel and, because this Tool owns no store, the transcript is
/// the only place the plan is remembered.
#[test]
fn direct_tools_todo_write_result_survives_session_reopen() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-tools-todo-reopen",
        "dl-todoreopen",
        "[\"session.todo_write\"]",
        "",
    );
    let server = wire_fake_llm(
        &world,
        "dl-todoreopen",
        sequence(vec![
            tool_call(
                "t1",
                "session.todo_write",
                r#"{"todos":[{"content":"remember me","status":"in_progress"}]}"#,
            ),
            SseScript::plain("plan recorded"),
            SseScript::plain("second turn reply"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-todoreopen");

    let first = send.send("write the plan").expect("first turn");
    let second = send.send("what was the plan").expect("second turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&first)?;
        assert_clean_send(&second)?;
        require(
            tool_ok(&first.parsed, "session.todo_write"),
            "the Component call must succeed on the first turn",
        )?;
        require(
            first.has_completed() && second.has_completed(),
            "both turns must commit",
        )?;
        // Request 1: the tool call. Request 2: the tool result. Request 3: the
        // reopened turn, which must still carry the tool-role message.
        require(
            captures.len() == 3,
            format!("expected three provider requests, got {}", captures.len()),
        )?;
        let messages = captures[2].raw["messages"]
            .as_array()
            .expect("reopened messages");
        require(
            messages
                .iter()
                .rev()
                .find(|message| message["role"] == "user")
                .is_some_and(|message| message["content"] == "what was the plan"),
            "reopened request must carry the second Turn user input",
        )?;
        let calls: Vec<_> = messages
            .iter()
            .filter(|message| message["role"] == "assistant")
            .filter_map(|message| message["tool_calls"].as_array())
            .flatten()
            .filter(|call| call["function"]["name"] == "session__todo_write")
            .collect();
        require(
            calls.len() == 1,
            format!("one persisted todo call required: {calls:?}"),
        )?;
        let call_id = calls[0]["id"].as_str().expect("persisted tool call id");
        let results: Vec<_> = messages
            .iter()
            .filter(|message| message["role"] == "tool" && message["tool_call_id"] == call_id)
            .collect();
        require(
            results.len() == 1,
            format!("one matching persisted tool result required: {results:?}"),
        )?;
        let reopened: Value =
            serde_json::from_str(results[0]["content"].as_str().expect("tool content"))
                .expect("persisted todo JSON");
        require(
            reopened
                == serde_json::json!({"todos":[{"content":"remember me","status":"in_progress"}]}),
            format!("reopened tool-role result lost exact todo content/status: {reopened}"),
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[
            ("todo first turn", &first.command),
            ("todo reopened turn", &second.command),
        ],
        captures,
        result,
    );
}

/// Provider request advertises exactly the profile-enabled encoded tool names.
#[test]
fn direct_tools_catalog_exposure_matches_enabled() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-tools-catalog",
        "dl-cat",
        "[\"observe.workspace\", \"mutate.create\", \"session.todo_write\"]",
        "",
    );
    let server = wire_fake_llm(&world, "dl-cat", SseScript::plain("no tools needed"));
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-cat");
    let outcome = send.send("hello").expect("turn");
    let captures = server.captured_requests();
    // 保留禁用子集检查，再通过同一后台验证完整发行目录与 manifest 字段投影。
    let profile_path = world
        .project_dir()
        .join(".lattice/agents/dl-cat/agent.toml");
    let profile = std::fs::read_to_string(&profile_path).expect("read profile");
    world
        .write_project_file(
            ".lattice/agents/dl-cat/agent.toml",
            &profile.replace(
                "[\"observe.workspace\", \"mutate.create\", \"session.todo_write\"]",
                "[\"observe.workspace\", \"mutate.create\", \"mutate.edit\", \"fetch.web\", \"session.todo_write\"]",
            ),
        )
        .expect("enable all five tools");
    let full = send
        .send("show the full catalog")
        .expect("full catalog turn");
    let full_captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        require(!captures.is_empty(), "need provider request")?;
        let mut names = wire_tool_names(&captures[0]);
        names.sort();
        let expected = vec![
            "mutate__create".to_string(),
            "observe__workspace".to_string(),
            "session__todo_write".to_string(),
        ];
        require(
            names == expected,
            format!("tool names on wire mismatch: got {names:?} expected {expected:?}"),
        )?;
        // Disabled default tools must not appear.
        require(
            !names.iter().any(|n| n.contains("fetch")),
            format!("fetch must not be advertised when disabled: {names:?}"),
        )?;
        assert_clean_send(&full)?;
        let full_request = &full_captures[1];
        let mut full_names = wire_tool_names(full_request);
        full_names.sort();
        require(
            full_names
                == [
                    "fetch__web",
                    "mutate__create",
                    "mutate__edit",
                    "observe__workspace",
                    "session__todo_write",
                ],
            format!("full catalog must be exactly five tools: {full_names:?}"),
        )?;
        let tools = full_request.raw["tools"]
            .as_array()
            .expect("wire tools array");
        let todo = &tools
            .iter()
            .find(|tool| tool["function"]["name"] == "session__todo_write")
            .expect("todo in full catalog")["function"];
        require(
            todo["description"]
                .as_str()
                .is_some_and(|description| description.contains("todo list")),
            format!("todo projection must carry manifest description: {todo}"),
        )?;
        require(
            todo["parameters"]["properties"]["todos"]["type"] == "array",
            format!("todo projection must retain todos array: {todo}"),
        )?;
        let observe = &tools
            .iter()
            .find(|tool| tool["function"]["name"] == "observe__workspace")
            .expect("observe in full catalog")["function"];
        require(
            observe["description"]
                .as_str()
                .is_some_and(|description| description.contains("content_hash")),
            format!("observe projection must carry manifest description: {observe}"),
        )?;
        require(
            observe["parameters"]["properties"]["request"]["oneOf"]
                .as_array()
                .map(Vec::len)
                == Some(4),
            format!("observe projection must retain four modes: {observe}"),
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[
            ("catalog exposure", &outcome.command),
            ("full catalog projection", &full.command),
        ],
        full_captures,
        result,
    );
}
