// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 模型请求传输层（#265）的真实二进制场景：请求头、`Retry-After`、错误诊断、
//! 断流重试、轮次内连接复用、HTTP/2 对未处理请求的重发，以及明文 HTTP/1.1 的本地 provider。断言落在
//! 假 provider 收到的请求与持久轮次记录上。

mod support;

#[cfg(feature = "fake-tools")]
use std::time::{Duration, Instant};

#[cfg(feature = "fake-tools")]
use lattice_system_harness::{AssertError, CapturedRequest, FAKE_API_KEY, FakeLlmServer};
use lattice_system_harness::{SendExecution, SessionSendDriver, SseScript};
use serde_json::Value;
use support::*;

#[cfg(feature = "fake-tools")]
const ACCEPT: &str = "text/event-stream, application/json";
const RETRY_FAST: &str = "[llm.retry]\nmax_retries = 3\ninitial_delay_ms = 1\nmax_delay_ms = 1\n";

fn final_turn(outcome: &SendExecution) -> Value {
    live_turn(outcome)
}

/// 第 `step` 个步骤的全部尝试记录。
fn attempts(turn: &Value, step: usize) -> Vec<Value> {
    turn["steps"][step]["attempts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn diagnostic(attempt: &Value) -> String {
    attempt["end"]["diagnostic"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[cfg(feature = "fake-tools")]
fn http(status: u16, content_type: &str, headers: &[(&str, &str)], body: &str) -> SseScript {
    SseScript::HttpResponse {
        status,
        content_type: content_type.into(),
        headers: headers
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect(),
        body: body.into(),
    }
}

/// 请求按冻结配置标识 LATTICE 与本会话，并声明可接受 SSE 与 JSON 错误正文。
#[cfg(feature = "fake-tools")]
fn assert_identity(captures: &[CapturedRequest], session: &str) -> Result<(), AssertError> {
    for (index, capture) in captures.iter().enumerate() {
        let agent = capture.headers.get("user-agent").map(String::as_str);
        require(
            agent.is_some_and(|ua| ua.starts_with("lattice/") && ua.contains("(linux ")),
            format!("第 {index} 次请求的 User-Agent 应标识 LATTICE：{agent:?}"),
        )?;
        require(
            capture.headers.get("accept").map(String::as_str) == Some(ACCEPT),
            format!(
                "第 {index} 次请求的 Accept 头不匹配：{:?}",
                capture.headers.get("accept")
            ),
        )?;
        require(
            capture
                .headers
                .get("x-client-request-id")
                .map(String::as_str)
                == Some(session),
            format!(
                "第 {index} 次请求的 x-client-request-id 应为会话标识 {session}：{:?}",
                capture.headers.get("x-client-request-id")
            ),
        )?;
    }
    Ok(())
}

#[cfg(feature = "fake-tools")]
#[test]
fn retry_after_is_honoured_and_requests_identify_lattice() {
    let driver = driver();
    let (world, spec) = direct_world("direct-transport-retry-after", "transport", RETRY_FAST);
    world.set_env("LATTICE_TEST_TOOLS", "read_note");
    world.budget(&spec);
    world.start_daemon().expect("显式启动带测试工具的后台");
    let server = wire_fake_llm(
        &world,
        "transport",
        // 第一步正常执行工具；限流落在第二步。
        sequence(vec![
            tool_call("call-1", "read_note", r#"{"action":"read"}"#),
            http(
                429,
                "application/json",
                &[("retry-after", "2")],
                r#"{"error":{"message":"Rate limit reached for requests","type":"requests","code":"rate_limit_exceeded"}}"#,
            ),
            SseScript::plain("done"),
        ]),
    );
    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "transport");
    let outcome = send.send("read notes after the rate limit").unwrap();
    let captures = server.captured_requests();
    let result = (|| {
        assert_clean_send(&outcome)?;
        require(outcome.has_completed(), "一次重试后轮次应正常完成")?;
        require(
            captures.len() == 3,
            format!("预期 3 次模型请求，实际为 {}", captures.len()),
        )?;
        let waited = captures[2].received_ms - captures[1].received_ms;
        require(
            waited >= 1_900,
            format!(
                "重试应遵守 provider 给出的 2 秒 retry-after 而非 1 毫秒退避，实际等待 {waited} 毫秒"
            ),
        )?;
        assert_identity(&captures, send.session_id().unwrap())?;
        require(
            captures[0].connection == captures[1].connection,
            format!(
                "第二步应复用第一步的连接：{} 与 {}",
                captures[0].connection, captures[1].connection
            ),
        )?;
        let turn = final_turn(&outcome);
        let second = attempts(&turn, 1);
        require(
            second.len() == 2,
            format!("第 1 步应包含 2 次尝试：{second:?}"),
        )?;
        let line = diagnostic(&second[0]);
        require(
            line == "HTTP 429 rate_limit_exceeded: Rate limit reached for requests (retry after 2s)",
            format!("解析后的限流诊断不匹配：{line:?}"),
        )
    })();
    finish_scenario(
        evidence,
        &[("遵循 retry-after 的轮次发送", &outcome.command)],
        captures,
        result,
    );
}

#[cfg(feature = "fake-tools")]
#[test]
fn retry_after_beyond_limits_fails_at_once_with_the_provider_reason() {
    let driver = driver();
    // 超过 60 秒上限，或在上限内但超过轮次剩余时间；两种情况都应立即以 provider 错误收束。
    for (case, label, retry_after, limits) in [
        ("over-cap", "超出上限的 retry-after 轮次发送", "120", ""),
        (
            "over-deadline",
            "超出轮次截止时间的 retry-after 轮次发送",
            "30",
            "[llm]\ntimeout_secs = 10\n",
        ),
    ] {
        let (world, spec) = direct_world(
            &format!("direct-transport-long-retry-{case}"),
            "transport",
            &format!("{limits}{RETRY_FAST}"),
        );
        world.set_env("LATTICE_TEST_TOOLS", "read_note");
        world.budget(&spec);
        world.start_daemon().expect("显式启动带测试工具的后台");
        // 第一步正常执行工具，限流落在第二步。
        let server = wire_fake_llm(
            &world,
            "transport",
            sequence(vec![
                tool_call("call-1", "read_note", r#"{"action":"read"}"#),
                http(
                    429,
                    "application/json",
                    &[("retry-after", retry_after)],
                    r#"{"error":{"message":"You exceeded your current quota","type":"requests","code":"rate_limit_exceeded"}}"#,
                ),
            ]),
        );
        let evidence = evidence(&world, &spec);
        let send = SessionSendDriver::new(&driver, &world, &spec, "transport");
        let started = Instant::now();
        let outcome = send.send("trigger long retry-after").unwrap();
        let elapsed = started.elapsed();
        let captures = server.captured_requests();
        let result = (|| {
            assert_clean_send(&outcome)?;
            assert_accepted_and_closed(&outcome)?;
            let turn = final_turn(&outcome);
            let end = &turn["steps"][1]["end"];
            require(
                outcome.command.is_clean_exit(1)
                    && turn["summary"]["outcome"] == "failed"
                    && end["cause"]["cause"] == "provider_error",
                format!("{case}：第二步应以 provider_error 而非截止时间超时失败：{end}"),
            )?;
            require(
                captures.len() == 2,
                format!(
                    "{case}：超限的限流请求不得重试，实际收到 {} 次请求",
                    captures.len()
                ),
            )?;
            require(
                elapsed < Duration::from_secs(8),
                format!("{case}：应当立即收束而非等待，实际耗时 {elapsed:?}"),
            )?;
            let notice = turn["summary"]["notice"]["text"]
                .as_str()
                .unwrap_or_default();
            require(
                notice.starts_with("HTTP 429 rate_limit_exceeded: You exceeded your current quota")
                    && notice.contains(&format!("retry after {retry_after}s")),
                format!("{case}：终态提示应保留 provider 错误原因：{notice:?}"),
            )
        })();
        finish_scenario(evidence, &[(label, &outcome.command)], captures, result);
    }
}

#[cfg(feature = "fake-tools")]
#[test]
fn overload_and_gateway_errors_retry_with_bounded_diagnostics() {
    let driver = driver();
    let spec = lattice_system_harness::ScenarioSpec::direct_local("direct-transport-overload");
    let world = lattice_system_harness::ScenarioWorld::new(&spec.name).expect("创建场景世界");
    world.set_env("LATTICE_TEST_TOOLS", "read_note");
    world.budget(&spec);
    world.start_daemon().expect("显式启动带测试工具的后台");
    let provider = lattice_system_harness::ProviderFixture {
        protocol: lattice_system_harness::ProviderProtocol::AnthropicMessages,
        target: lattice_system_harness::ProviderTarget::Builtin("anthropic"),
        model: "claude-sonnet-4-6",
    };
    let model = provider.model_ref().unwrap();
    world
        .write_project_file(
            ".lattice/agents/overload/agent.toml",
            &format!(
                "schema_version = 1\n\n[agent]\nname = \"overload\"\nmodel = \"{model}\"\n\n\
             [prompt]\nsystem = \"You are a concise test agent.\"\n\n\
             [llm.retry]\nmax_retries = 1\ninitial_delay_ms = 1\nmax_delay_ms = 1\n"
            ),
        )
        .unwrap();
    world.init_git_project().unwrap();
    let gateway_page = format!(
        "<html>\r\n<head><title>502 Bad Gateway</title></head>\r\n<body>{}</body></html>",
        "x".repeat(5_000)
    );
    // 第一步正常执行工具；第二步先遇 529 再遇 502，用尽一次重试后以 502 结束轮次。
    let server = FakeLlmServer::start(sequence(vec![
        tool_call("call-1", "read_note", r#"{"action":"read"}"#),
        http(
            529,
            "application/json",
            &[],
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"},"request_id":"req_011CTest"}"#,
        ),
        http(502, "text/html", &[], &gateway_page),
    ]))
    .unwrap();
    let key = "anthropic-transport-key";
    server.expect_api_key(key);
    world.wire_provider(provider, &server, key).unwrap();
    let evidence =
        lattice_system_harness::ScenarioEvidence::new(&world, &spec.name, [key.to_string()])
            .unwrap();
    let send = SessionSendDriver::new(&driver, &world, &spec, "overload");
    let outcome = send.send("survive overload").unwrap();
    let captures = server.captured_requests();
    let result = (|| {
        assert_clean_send(&outcome)?;
        assert_accepted_and_closed(&outcome)?;
        require(
            captures.len() == 3,
            format!("预期 3 次模型请求，实际为 {}", captures.len()),
        )?;
        let turn = final_turn(&outcome);
        require(
            outcome.command.is_clean_exit(1) && turn["summary"]["outcome"] == "failed",
            format!("用尽重试后轮次应失败：{}", turn["summary"]),
        )?;
        let step = attempts(&turn, 1);
        require(step.len() == 2, format!("第二步应有 2 次尝试：{step:?}"))?;
        let overloaded = diagnostic(&step[0]);
        require(
            overloaded == "HTTP 529 overloaded_error: Overloaded",
            format!("应解析 Anthropic 错误包络并排除请求标识：{overloaded:?}"),
        )?;
        require(
            step[1]["end"]["cause"]
                == serde_json::json!({"cause":"provider_error","retryable":true}),
            format!(
                "502 应标为可重试，仅因重试预算耗尽而结束轮次：{}",
                step[1]["end"]
            ),
        )?;
        let gateway = diagnostic(&step[1]);
        require(
            gateway.starts_with("HTTP 502: <html> <head><title>502 Bad Gateway</title></head>")
                && gateway.chars().count() == 256
                && gateway.ends_with('…'),
            format!("非 JSON 响应体应截断为有界的单行摘要：{gateway:?}"),
        )?;
        let notice = &turn["summary"]["notice"];
        require(
            notice["level"] == "error" && notice["text"] == gateway.as_str(),
            format!("失败轮次的提示应等于最后一次尝试的诊断：{notice}"),
        )
    })();
    finish_scenario(
        evidence,
        &[("过载与网关错误轮次发送", &outcome.command)],
        captures,
        result,
    );
}

#[test]
fn unauthorized_request_fails_without_retry_notice() {
    let driver = driver();
    let (world, spec) = direct_world("direct-transport-unauthorized", "transport", RETRY_FAST);
    world.budget(&spec);
    world.start_daemon().expect("显式启动后台");
    let server = wire_fake_llm(
        &world,
        "transport",
        SseScript::HttpError {
            status: 401,
            body: r#"{"error":{"message":"invalid credential"}}"#.into(),
        },
    );
    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "transport");
    let outcome = send.send("reject unauthorized request").unwrap();
    let captures = server.captured_requests();
    let result = (|| {
        assert_clean_send(&outcome)?;
        require(outcome.command.is_clean_exit(1), "未授权轮次的退出码应为 1")?;
        require(captures.len() == 1, "401 错误绝不能重试")?;
        require(
            outcome
                .entries()
                .iter()
                .filter_map(|entry| entry.step())
                .all(|step| step.retries.is_empty()),
            "401 错误不得附带重试提示",
        )?;
        let turn = final_turn(&outcome);
        let attempts = attempts(&turn, 0);
        let notice = &turn["summary"]["notice"];
        require(
            attempts.len() == 1
                && notice["level"] == "error"
                && notice["text"] == diagnostic(&attempts[0])
                && diagnostic(&attempts[0]).starts_with("HTTP 401"),
            "终态错误提示必须保留 401 诊断信息",
        )
    })();
    finish_scenario(
        evidence,
        &[("未授权请求轮次发送", &outcome.command)],
        captures,
        result,
    );
}

#[cfg(feature = "fake-tools")]
#[test]
fn stream_cut_before_completion_is_retried() {
    let driver = driver();
    let (world, spec) = direct_world("direct-transport-cut", "transport", RETRY_FAST);
    world.set_env("LATTICE_TEST_TOOLS", "read_note");
    world.budget(&spec);
    world.start_daemon().expect("显式启动带测试工具的后台");
    // 第二步先遇到正常结束却缺少结束事件的流，再遇到读到一半被
    // `RST_STREAM(INTERNAL_ERROR)` 重置的流，第三次成功。
    let server = wire_fake_llm(
        &world,
        "transport",
        sequence(vec![
            tool_call("call-1", "read_note", r#"{"action":"read"}"#),
            SseScript::Disconnect {
                chunks_before_drop: vec!["partial".into()],
            },
            SseScript::Truncated {
                chunks_before_drop: vec!["half".into()],
            },
            SseScript::plain("whole answer"),
        ]),
    );
    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "transport");
    let outcome = send.send("answer despite a cut").unwrap();
    let captures = server.captured_requests();
    let result = (|| {
        assert_clean_send(&outcome)?;
        require(outcome.has_completed(), "中途断流应在重试后顺利完成")?;
        require(
            captures.len() == 4,
            format!("预期 4 次模型请求，实际为 {}", captures.len()),
        )?;
        // 两次断流都发生在响应体中途，两次重试提示都在这一步的条目中。
        let entries = outcome.entries();
        let cut = entries
            .iter()
            .find(|entry| entry.step().is_some_and(|step| step.ordinal == 1))
            .expect("应存在第二步条目");
        let retried: Vec<u64> = cut
            .step()
            .unwrap()
            .retries
            .iter()
            .map(|r| r.attempt)
            .collect();
        require(
            retried == [0, 1],
            format!("两次中止的尝试都应附带重试提示：{retried:?}"),
        )?;
        let turn = final_turn(&outcome);
        let step = attempts(&turn, 1);
        let retryable = serde_json::json!({"cause":"provider_error","retryable":true});
        require(
            step.len() == 3
                && step[0]["end"]["cause"] == retryable
                && step[1]["end"]["cause"] == retryable,
            format!("两次断流都应记录为可重试的 provider 错误：{step:?}"),
        )
    })();
    finish_scenario(
        evidence,
        &[("断流重试轮次发送", &outcome.command)],
        captures,
        result,
    );
}

#[cfg(feature = "fake-tools")]
#[test]
fn multi_step_turn_reuses_one_connection_and_survives_server_close() {
    let driver = driver();
    let (world, spec) = direct_world("direct-transport-reuse", "transport", RETRY_FAST);
    world.set_env("LATTICE_TEST_TOOLS", "read_note");
    world.budget(&spec);
    world.start_daemon().expect("显式启动带测试工具的后台");
    let server = wire_fake_llm(
        &world,
        "transport",
        sequence(vec![
            tool_call(
                "call-1",
                "read_note",
                r#"{"action":"read","value":{"item":"alpha"}}"#,
            ),
            SseScript::CloseAfter {
                script: Box::new(tool_call(
                    "call-2",
                    "read_note",
                    r#"{"action":"read","value":{"item":"beta"}}"#,
                )),
            },
            SseScript::plain("both read"),
        ]),
    );
    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "transport");
    let outcome = send.send("read both files").unwrap();
    let captures = server.captured_requests();
    let result = (|| {
        assert_clean_send(&outcome)?;
        require(outcome.has_completed(), "三步轮次应正常完成")?;
        require(
            captures.len() == 3,
            format!("预期 3 次模型请求，实际为 {}", captures.len()),
        )?;
        let connections: Vec<_> = captures.iter().map(|c| c.connection).collect();
        require(
            connections[0] == connections[1],
            format!("第 0 步与第 1 步应复用同一条连接：{connections:?}"),
        )?;
        require(
            connections[2] != connections[1],
            format!("服务端发送 GOAWAY 后，第 2 步应建立新连接：{connections:?}"),
        )?;
        let turn = final_turn(&outcome);
        for step in 0..3 {
            let records = attempts(&turn, step);
            require(
                records.len() == 1,
                format!("第 {step} 步应当恰好只有一次尝试：{records:?}"),
            )?;
        }
        Ok(())
    })();
    finish_scenario(
        evidence,
        &[("连接复用轮次发送", &outcome.command)],
        captures,
        result,
    );
}

/// HTTP/2 下 provider 明确未处理的请求（`GOAWAY` 的最后流号小于它，或 `REFUSED_STREAM`）
/// 由传输层重发，归在同一次尝试里：provider 收到两次，持久记录只有一条尝试。
#[cfg(feature = "fake-tools")]
#[test]
fn unprocessed_requests_are_resent_inside_one_attempt() {
    for (case, refusal) in [
        ("goaway", SseScript::GoawayUnprocessed),
        ("refused", SseScript::RefusedStream),
    ] {
        let driver = driver();
        // 本地重试预算为 0：若重发没有发生在传输层，第二步只能失败。
        let (world, spec) = direct_world(
            &format!("direct-transport-unprocessed-{case}"),
            "transport",
            "[llm.retry]\nmax_retries = 0\n",
        );
        world.set_env("LATTICE_TEST_TOOLS", "read_note");
        world.budget(&spec);
        world.start_daemon().expect("显式启动带测试工具的后台");
        // 第一步正常执行工具；第二步的请求先被拒绝，紧接着的重发得到回答。
        let server = wire_fake_llm(
            &world,
            "transport",
            sequence(vec![
                tool_call("call-1", "read_note", r#"{"action":"read"}"#),
                refusal,
                SseScript::plain("answered after resend"),
            ]),
        );
        let evidence = evidence(&world, &spec);
        let send = SessionSendDriver::new(&driver, &world, &spec, "transport");
        let outcome = send.send("answer after a refusal").unwrap();
        let captures = server.captured_requests();
        let result = (|| {
            assert_clean_send(&outcome)?;
            require(outcome.has_completed(), format!("{case}：轮次应正常完成"))?;
            require(
                captures.len() == 3,
                format!(
                    "{case}：应当有 3 次请求到达 provider，实际为 {}",
                    captures.len()
                ),
            )?;
            require(
                captures[1].raw == captures[2].raw,
                format!("{case}：重发请求应携带相同的请求体"),
            )?;
            let connections: Vec<_> = captures.iter().map(|c| c.connection).collect();
            require(
                connections[0] == connections[1],
                format!("{case}：被拒请求应沿用第 0 步的连接：{connections:?}"),
            )?;
            if case == "goaway" {
                require(
                    connections[2] != connections[1],
                    format!("goaway：重发请求应建立新连接：{connections:?}"),
                )?;
            }
            let turn = final_turn(&outcome);
            for step in 0..2 {
                let records = attempts(&turn, step);
                require(
                    records.len() == 1,
                    format!("{case}：第 {step} 步应当恰好只有一次尝试：{records:?}"),
                )?;
            }
            Ok(())
        })();
        finish_scenario(
            evidence,
            &[("未处理请求重发轮次发送", &outcome.command)],
            captures,
            result,
        );
    }
}

/// 本地 provider（如 Ollama）走明文 HTTP/1.1：多步轮次完成，服务繁忙的 503 按可重试处理，
/// 请求头与 HTTPS provider 相同。分块响应的结尾晚于结束事件到达，整个轮次仍只用一条连接：
/// 产品交付结束事件前读完了响应体，连接才能回到连接池。
#[cfg(feature = "fake-tools")]
#[test]
fn plaintext_local_provider_over_http1() {
    let driver = driver();
    let (world, spec) = direct_world("direct-transport-local-http1", "transport", RETRY_FAST);
    world.set_env("LATTICE_TEST_TOOLS", "read_note");
    world.budget(&spec);
    world.start_daemon().expect("显式启动带测试工具的后台");
    let server = FakeLlmServer::start_plain_http1(sequence(vec![
        tool_call(
            "call-1",
            "read_note",
            r#"{"action":"read","value":{"item":"alpha"}}"#,
        ),
        SseScript::HttpError {
            status: 503,
            body: r#"{"error":"server busy, please try again"}"#.into(),
        },
        tool_call(
            "call-2",
            "read_note",
            r#"{"action":"read","value":{"item":"beta"}}"#,
        ),
        SseScript::plain("local answer"),
    ]))
    .unwrap();
    let base_url = server.base_url();
    world
        .wire_provider(
            lattice_system_harness::ProviderFixture::chat_completions("transport"),
            &server,
            FAKE_API_KEY,
        )
        .unwrap();
    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "transport");
    let outcome = send.send("read both files locally").unwrap();
    let captures = server.captured_requests();
    let result = (|| {
        require(
            base_url.starts_with("http://localhost:"),
            format!("本地明文回环基础地址不匹配：{base_url}"),
        )?;
        assert_clean_send(&outcome)?;
        require(outcome.has_completed(), "本地明文轮次应正常完成")?;
        require(
            captures.len() == 4,
            format!("预期 4 次模型请求，实际为 {}", captures.len()),
        )?;
        assert_identity(&captures, send.session_id().unwrap())?;
        let connections: Vec<_> = captures.iter().map(|c| c.connection).collect();
        require(
            connections == [0, 0, 0, 0],
            format!("本轮所有请求都应复用同一条连接：{connections:?}"),
        )?;
        let turn = final_turn(&outcome);
        let second = attempts(&turn, 1);
        require(
            second.len() == 2
                && second[0]["end"]["cause"]
                    == serde_json::json!({"cause":"provider_error","retryable":true}),
            format!("503 应记录为成功前的一次可重试尝试：{second:?}"),
        )?;
        let line = diagnostic(&second[0]);
        require(
            line == "HTTP 503: server busy, please try again",
            format!("503 诊断信息不匹配：{line:?}"),
        )
    })();
    finish_scenario(
        evidence,
        &[("本地 HTTP/1.1 明文轮次发送", &outcome.command)],
        captures,
        result,
    );
}
