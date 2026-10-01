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
    AssertError, CapturedRequest, FixtureHttpServer, HttpFixture, SessionSendDriver, SseScript,
    TurnEventLine,
};
use serde_json::Value;
use support::{
    assert_clean_send, assert_committed_view_after, cli, direct_world, driver, evidence,
    finish_scenario, last_tool_content, require, sequence, tool_call, tool_failed, tool_ok,
    tool_result_value, wire_fake_llm,
};

fn fetch_network_profile(max_bytes: u64) -> String {
    format!(
        "\n[lancet.network]\nallow_localhost = true\nallowed_hosts = [\"localhost\"]\nmax_web_response_bytes = {max_bytes}\n\n[lancet.capabilities]\nmax_boundary = \"ex\"\n"
    )
}

/// Encoded tool names as advertised on the Chat Completions wire (`a.b` → `a__b`).
fn wire_tool_names(capture: &CapturedRequest) -> Vec<String> {
    capture.tool_names.clone()
}

/// `fetch.web` success against localhost fixture: request path, status/body/type,
/// optional truncation, and no public Internet (fixture only).
#[test]
fn direct_tools_fetch_web_localhost_success_and_cap() {
    let driver = driver();
    let web = FixtureHttpServer::start(HttpFixture::plain("fixture body for fetch"))
        .expect("fixture http");
    let url = web.url_path("/fixture-path");
    let (world, spec) = direct_world(
        "direct-tools-fetch-ok",
        "dl-fetch",
        "[\"fetch.web\"]",
        &fetch_network_profile(64),
    );
    let args = format!(r#"{{"url":"{url}"}}"#);
    let server = wire_fake_llm(
        &world,
        "dl-fetch",
        sequence(vec![
            tool_call("f1", "fetch.web", &args),
            SseScript::plain("fetched"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-fetch");
    let outcome = send.send("fetch the fixture").expect("turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        require(tool_ok(&outcome.parsed, "fetch.web"), "fetch.web ok")?;
        require(outcome.has_completed(), "turn completed")?;
        web.wait_for_requests(1, Duration::from_secs(5))
            .map_err(|e| AssertError::StdoutMissing {
                expected: format!("fixture did not receive request: {e}"),
            })?;
        let http = web.captured_requests();
        require(
            http.len() == 1,
            format!("expected 1 http capture, got {http:?}"),
        )?;
        require(
            http[0].method == "GET" && http[0].path == "/fixture-path",
            format!("unexpected fixture request: {:?}", http[0]),
        )?;
        require(captures.len() >= 2, "need tool result capture")?;
        let fetched = tool_result_value(&captures[1]).expect("fetch result JSON");
        let status = fetched
            .get("status")
            .or_else(|| fetched.pointer("/result/status"))
            .and_then(Value::as_u64);
        let body = fetched
            .get("body")
            .or_else(|| fetched.pointer("/result/body"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let content_type = fetched
            .get("content_type")
            .or_else(|| fetched.pointer("/result/content_type"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let truncated = fetched
            .get("truncated")
            .or_else(|| fetched.pointer("/result/truncated"))
            .and_then(Value::as_bool);
        require(status == Some(200), format!("status not 200: {fetched}"))?;
        // Body is longer than max_web_response_bytes=64 → truncated.
        require(
            body.contains("fixture") || truncated == Some(true),
            format!("body/truncation missing: body={body:?} truncated={truncated:?} raw={fetched}"),
        )?;
        require(
            content_type.contains("text/plain") || content_type.is_empty(),
            format!("content_type unexpected: {content_type}"),
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("fetch.web success", &outcome.command)],
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
        // Disabled tool must not reach CEP dispatch (no cep_call_started for fetch.web).
        let cep_fetch = outcome.events.iter().any(|e| match e {
            TurnEventLine::Unknown { type_name, raw }
                if type_name == "cep_call_started" || type_name == "cep.call_started" =>
            {
                raw.get("method").and_then(Value::as_str) == Some("fetch.web")
            }
            _ => false,
        });
        require(
            !cep_fetch,
            "disabled fetch.web must not emit cep_call_started",
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
            detail["steps"][0]["state"]["end"]["cause"]["cause"] == "invalid_batch",
            "durable invalid batch",
        )?;
        require(
            detail["steps"][0]["state"]["slots"]
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
        if captures.len() >= 2 {
            let msg = last_tool_content(&captures[1]).unwrap_or_default();
            require(
                msg.to_lowercase().contains("allow")
                    || msg.to_lowercase().contains("denied")
                    || msg.to_lowercase().contains("not allowed")
                    || msg.to_lowercase().contains("host")
                    || msg.to_lowercase().contains("policy")
                    || msg.to_lowercase().contains("localhost"),
                format!("denial message weak: {msg}"),
            )?;
        }
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
fn direct_tools_fetch_capability_denial_with_public_https() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-tools-fetch-cap",
        "dl-fcap",
        "[\"fetch.web\"]",
        "\n[lancet.network]\nallow_public_https = true\nallow_localhost = false\nallowed_hosts = []\n\n[lancet.capabilities]\ndeny = [\"network.fetch\"]\nmax_boundary = \"ex\"\n",
    );
    let server = wire_fake_llm(
        &world,
        "dl-fcap",
        sequence(vec![
            tool_call("f1", "fetch.web", r#"{"url":"https://example.com/"}"#),
            SseScript::plain("denied without fetch capability"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-fcap");
    let outcome = send.send("fetch example.com").expect("turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        require(
            tool_failed(&outcome.parsed, "fetch.web"),
            "fetch.web must fail when network.fetch is denied",
        )?;
        let cep_started_raw = outcome.events.iter().find_map(|event| match event {
            TurnEventLine::Unknown { type_name, raw }
                if type_name == "cep_call_started"
                    && raw.get("method").and_then(Value::as_str) == Some("fetch.web") =>
            {
                Some(raw)
            }
            _ => None,
        });
        require(
            cep_started_raw.is_some_and(|raw| {
                raw.get("invocation_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id.len() == 36)
                    && raw.get("semantic_stage").and_then(Value::as_str) == Some("cell_component")
                    && raw.get("guard_stage").and_then(Value::as_str) == Some("cell_admission")
            }),
            format!(
                "fetch.web cep_call_started must carry typed identity and Component stages: {cep_started_raw:?}"
            ),
        )?;
        // The Component never reaches the network host import when admission removes
        // its sole capability.
        require(
            captures.len() >= 2,
            "capability denial needs tool result capture",
        )?;
        let err_text = last_tool_content(&captures[1]).unwrap_or_default();
        let err_lower = err_text.to_lowercase();
        require(
            err_lower.contains("fetch.web") && err_lower.contains("has no admitted capability"),
            format!("network.fetch admission denial mismatch: {err_text}"),
        )?;
        require(
            !tool_ok(&outcome.parsed, "fetch.web"),
            "capability denial must not succeed fetch.web",
        )?;
        // 关闭快照核验本次 Turn 的真实结局，不依赖必须收到瞬时终结事件。
        assert_committed_view_after(&outcome.parsed, "tool_call_finished")?;
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
        let cep_started = outcome.events.iter().position(|event| match event {
            TurnEventLine::Unknown { type_name, raw } if type_name == "cep_call_started" => {
                raw.get("method").and_then(Value::as_str) == Some("session.todo_write")
            }
            _ => false,
        });
        let cep_finished = outcome.events.iter().position(|event| match event {
            TurnEventLine::Unknown { type_name, raw } if type_name == "cep_call_finished" => {
                raw.get("method").and_then(Value::as_str) == Some("session.todo_write")
            }
            _ => false,
        });
        let tool_finished = outcome.events.iter().position(|event| {
            matches!(
                event,
                TurnEventLine::ToolCallFinished { name, .. } if name == "session.todo_write"
            )
        });
        // The ordered emitter holds public completion until both host-bound
        // runtime lifecycle events have been observed for the same invocation.
        require(
            cep_started
                .zip(cep_finished)
                .zip(tool_finished)
                .is_some_and(|((started, finished), tool)| started < finished && finished < tool),
            format!(
                "todo CEP/tool lifecycle ordering missing: {:?}",
                outcome.events
            ),
        )?;
        let cep_started_raw = outcome.events.iter().find_map(|event| match event {
            TurnEventLine::Unknown { type_name, raw }
                if type_name == "cep_call_started"
                    && raw.get("method").and_then(Value::as_str) == Some("session.todo_write") =>
            {
                Some(raw)
            }
            _ => None,
        });
        let public_started_id = outcome.events.iter().find_map(|event| match event {
            TurnEventLine::ToolCallStarted { id, name } if name == "session.todo_write" => {
                Some(id.as_str())
            }
            _ => None,
        });
        let public_finished_id = outcome.events.iter().find_map(|event| match event {
            TurnEventLine::ToolCallFinished { id, name, .. } if name == "session.todo_write" => {
                Some(id.as_str())
            }
            _ => None,
        });
        let cep_invocation_id = cep_started_raw
            .and_then(|raw| raw.get("invocation_id"))
            .and_then(Value::as_str);
        require(
            cep_invocation_id.is_some()
                && public_started_id == cep_invocation_id
                && public_finished_id == cep_invocation_id,
            format!(
                "public Tool events must use the typed InvocationId: \
                 started={public_started_id:?} finished={public_finished_id:?} \
                 cep={cep_invocation_id:?}"
            ),
        )?;

        require(
            cep_started_raw.is_some_and(|raw| {
                raw.get("invocation_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id.len() == 36)
                    && raw.get("semantic_stage").and_then(Value::as_str) == Some("cell_component")
                    && raw.get("guard_stage").and_then(Value::as_str) == Some("cell_admission")
            }),
            format!(
                "todo_write must be dispatched to the embedded Component \
                 (cell_component/cell_admission), got: {cep_started_raw:?}"
            ),
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
        // Turn may still complete after recovery reply.
        if captures.len() >= 2 {
            let msg = last_tool_content(&captures[1]).unwrap_or_default();
            require(
                msg.to_lowercase().contains("in_progress")
                    || msg.to_lowercase().contains("todo")
                    || msg.to_lowercase().contains("invalid")
                    || msg.to_lowercase().contains("denied")
                    || msg.to_lowercase().contains("error")
                    || !msg.starts_with('{'),
                format!("validation error message weak: {msg}"),
            )?;
        }
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
            captures.len() >= 3,
            format!("expected three provider requests, got {}", captures.len()),
        )?;
        let reopened = captures[2].raw.to_string();
        require(
            reopened.contains("remember me"),
            format!("the reopened turn lost the Component result: {reopened}"),
        )?;
        require(
            reopened.contains("in_progress"),
            format!("the reopened transcript lost the todo status: {reopened}"),
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
            observe["parameters"]["oneOf"].as_array().map(Vec::len) == Some(4),
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
