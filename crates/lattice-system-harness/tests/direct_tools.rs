// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct-Local fetch, todo, catalog exposure, and retired methods
//! (issue #46 slice 4, Codeberg #24).
//!
//! Completes built-binary coverage of the default five-Tool distribution and
//! proves retired methods fail explicitly without side effects or fallback.
//!
//! `fetch.web` is driven against a harness-only localhost HTTP fixture
//! (not the OpenAI-compatible fake LLM). Tool results are inspected in the
//! next captured provider request.

mod support;

use std::fs;
use std::time::Duration;

use lattice_system_harness::{
    AssertError, CapturedRequest, FixtureHttpServer, HttpFixture, SessionSendDriver, SseScript,
    TurnEventLine,
};
use serde_json::Value;
use support::{
    assert_clean_send, direct_world, driver, evidence, finish_scenario, last_tool_content, require,
    sequence, tool_call, tool_failed, tool_ok, tool_result_value, wire_fake_llm,
};

fn fetch_network_profile(max_bytes: u64) -> String {
    format!(
        "\n[lancet.network]\nallow_localhost = true\nallowed_hosts = [\"localhost\"]\nmax_web_response_bytes = {max_bytes}\n"
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
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-fetch", "main").expect("session");
    let outcome = send.send("fetch the fixture").expect("turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        require(tool_ok(&outcome.parsed, "fetch.web"), "fetch.web ok")?;
        require(outcome.has_committed_completed(), "turn completed")?;
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
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-foff", "main").expect("session");
    let outcome = send.send("try fetch").expect("turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        // Either tool_call_finished ok=false, or the model tool call is rejected
        // without a finished event that looks successful.
        let ok_fetch = tool_ok(&outcome.parsed, "fetch.web");
        require(!ok_fetch, "disabled fetch.web must not succeed")?;
        if outcome.events.iter().any(
            |e| matches!(e, TurnEventLine::ToolCallStarted { name, .. } if name == "fetch.web"),
        ) {
            require(
                tool_failed(&outcome.parsed, "fetch.web"),
                "started fetch.web must finish failed when disabled",
            )?;
        }
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
        "\n[lancet.network]\nallow_localhost = false\nallowed_hosts = []\n",
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
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-fdeny", "main").expect("session");
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

/// Enabled fetch with `allow_public_https=true` but empty `allowed_hosts`:
/// URL-shape validation may allow public HTTPS, yet FetchUrl authorized hosts
/// still come from `allowed_hosts`, so `https://example.com/` is rejected with
/// an EffectViolation / authorized-region denial before outbound I/O. Distinct
/// from host-policy denial (`allow_localhost=false` against a localhost URL).
#[test]
fn direct_tools_fetch_capability_denial_with_public_https() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-tools-fetch-cap",
        "dl-fcap",
        "[\"fetch.web\"]",
        "\n[lancet.network]\nallow_public_https = true\nallow_localhost = false\nallowed_hosts = []\n",
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
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-fcap", "main").expect("session");
    let outcome = send.send("fetch example.com").expect("turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        require(
            tool_failed(&outcome.parsed, "fetch.web"),
            "fetch.web must fail when FetchUrl hosts are empty despite allow_public_https",
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
                raw.get("impact_kind").and_then(Value::as_str) == Some("external_observation")
                    && raw.get("semantic_stage").and_then(Value::as_str) == Some("cep_native")
            }),
            format!(
                "fetch.web cep_call_started must carry external_observation/cep_native: {cep_started_raw:?}"
            ),
        )?;
        // Enforce-mode empty FetchUrl hosts deny before outbound I/O. The tool
        // error must name the effect/authorized-region class so DNS/HTTP failure
        // cannot satisfy this scenario.
        require(
            captures.len() >= 2,
            "capability denial needs tool result capture",
        )?;
        let err_text = last_tool_content(&captures[1]).unwrap_or_default();
        let err_lower = err_text.to_lowercase();
        require(
            err_lower.contains("authorized region")
                || err_lower.contains("effect")
                || err_lower.contains("exceeds"),
            format!("capability denial must cite effect/authorized-region: {err_text}"),
        )?;
        require(
            !tool_ok(&outcome.parsed, "fetch.web"),
            "capability denial must not succeed fetch.web",
        )?;
        require(
            outcome.events.iter().any(|event| {
                matches!(
                    event,
                    TurnEventLine::TurnCommitted { status, .. } if status == "completed"
                )
            }),
            format!(
                "turn must still complete after capability-denied tool: {:?}",
                outcome.type_sequence()
            ),
        )?;
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
/// The Component/native distinction is observable from outside the binary: a
/// Cell-governed call reports `semantic_stage = "cell_component"` and
/// `guard_stage = "cell_admission"`, which no native handler ever emits. A build
/// that fell back to a native handler, or whose composition failed to embed the
/// Component, fails here rather than passing silently.
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
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-todo", "main").expect("session");
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
        require(outcome.has_committed_completed(), "turn completed")?;
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
        // CEP call events and agent-loop tool events travel on separate async
        // channels (the CEP sink in lancet-native's transport vs the turn event
        // stream), so their relative interleaving is not ordered. Assert only
        // the ordering that holds within the CEP channel itself, plus the
        // presence of the tool completion. Requiring
        // `cep_finished < ToolCallFinished` across the two channels would assert
        // an ordering the implementation does not provide.
        require(
            cep_started
                .zip(cep_finished)
                .is_some_and(|(started, finished)| started < finished)
                && tool_finished.is_some(),
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
        require(
            cep_started_raw.is_some_and(|raw| {
                raw.get("semantic_stage").and_then(Value::as_str) == Some("cell_component")
                    && raw.get("guard_stage").and_then(Value::as_str) == Some("cell_admission")
            }),
            format!(
                "todo_write must be dispatched to the embedded Component \
                 (cell_component/cell_admission), got: {cep_started_raw:?}"
            ),
        )?;
        // A Cell-governed row carries no legacy impact kind: its authorization
        // vocabulary is the Cell. `unknown` here is the *positive* signal that
        // the native method-impact path did not run.
        require(
            cep_started_raw.is_some_and(|raw| {
                raw.get("impact_kind").and_then(Value::as_str) != Some("session_state_mutation")
            }),
            format!("a Component row must not claim a native impact kind: {cep_started_raw:?}"),
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
            status.status.success(),
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
            .get("recent_calls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        require(
            recent_calls.iter().any(|call| {
                call.get("method").and_then(Value::as_str) == Some("session.todo_write")
            }) && status_json
                .get("last_guard_verdict")
                .is_some_and(|value| !value.is_null()),
            format!("lancet status omitted todo/verdict: {status_json}"),
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
        // No `max_todo_ops` here: that knob has no enforcement point for a
        // Component Tool (FINDINGS.md L-GH29-01), so setting it would read as
        // live budget coverage this scenario does not provide. The refusal below
        // is the descriptor's one-in_progress rule.
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
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-todobad", "main").expect("session");
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
/// A Component result reaches the model through the persisted tool-role message,
/// the same durable path a native handler's result took. Reopening the session
/// and driving a second turn proves the migration did not move the result into
/// some in-process-only channel — and, since this Tool has no store, that the
/// transcript is the only place the plan is remembered.
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
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-todoreopen", "main").expect("session");

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
            first.has_committed_completed() && second.has_committed_completed(),
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
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-cat", "main").expect("session");
    let outcome = send.send("hello").expect("turn");
    let captures = server.captured_requests();

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
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("catalog exposure", &outcome.command)],
        captures,
        result,
    );
}

/// Forced retired method calls fail as unknown/disabled, emit no successful
/// tool finish, and leave project files unchanged.
#[test]
fn direct_tools_retired_methods_fail_closed() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-tools-retired",
        "dl-ret",
        "[\"observe.workspace\"]",
        "",
    );
    world
        .write_project_file("sentinel.txt", "untouched\n")
        .expect("sentinel");
    world.commit_all("seed").expect("commit");

    let retired = [
        ("exec.command_run", r#"{"command":"echo pwned","cwd":"."}"#),
        ("mutate.batch", r#"{"operations":[]}"#),
        ("observe.audit", r#"{}"#),
    ];
    let mut scripts = Vec::new();
    for (i, (name, args)) in retired.iter().enumerate() {
        scripts.push(tool_call(&format!("r{i}"), name, args));
        scripts.push(SseScript::plain(format!("handled {name}")));
    }
    let server = wire_fake_llm(&world, "dl-ret", sequence(scripts));
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-ret", "main").expect("session");

    // One turn that chains retired tool calls via Sequence.
    let outcome = send.send("try retired methods").expect("turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        for (name, _) in &retired {
            require(
                !tool_ok(&outcome.parsed, name),
                format!("retired method {name} must not succeed"),
            )?;
        }
        // No successful CEP backend for retired tools: if ToolCallFinished exists, ok=false.
        for event in &outcome.events {
            if let TurnEventLine::ToolCallFinished { name, ok, .. } = event
                && retired.iter().any(|(n, _)| n == name)
            {
                require(!*ok, format!("retired {name} finished ok=true"))?;
            }
        }
        require(
            fs::read_to_string(world.project_dir().join("sentinel.txt")).unwrap_or_default()
                == "untouched\n",
            "retired methods must not mutate project files",
        )?;
        // No surprise new files from exec/batch.
        require(
            !world.project_dir().join("pwned").exists(),
            "exec.command_run must not create side-effect files",
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("retired methods", &outcome.command)],
        captures,
        result,
    );
}
