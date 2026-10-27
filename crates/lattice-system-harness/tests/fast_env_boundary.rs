// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 通过公开 CLI 验证工作区 `.env` 不提供凭据、模型、LATTICE 配置，
//! 也不能改变 endpoint、TLS 信任或状态根。

use lattice_system_harness::cli::run;
use lattice_system_harness::{
    AssertError, CommandOutcome, FAKE_API_KEY, FakeLlmServer, ScenarioSpec, ScenarioWorld,
    SseScript, assert_exit_code, assert_exit_success, assert_stdout_contains,
};
use serde_json::Value;

const SECRET_SENTINEL: &str = "sk-ant-dotenv-sentinel-4b91";
const ATTACKER_BASE_URL: &str = "http://127.0.0.1:9/v1";

fn parse_json(output: &CommandOutcome) -> Result<Value, AssertError> {
    serde_json::from_slice(&output.stdout).map_err(|error| AssertError::StdoutMissing {
        expected: format!("valid JSON output: {error}"),
    })
}

fn require(condition: bool, expected: impl Into<String>) -> Result<(), AssertError> {
    if condition {
        Ok(())
    } else {
        Err(AssertError::StdoutMissing {
            expected: expected.into(),
        })
    }
}

fn unloaded_world(name: &str) -> ScenarioWorld {
    let world = ScenarioWorld::new(name).expect("world");
    for key in [
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "GEMINI_API_KEY",
        "OPENAI_COMPATIBLE_API_KEY",
        "ANTHROPIC_BASE_URL",
        "GEMINI_BASE_URL",
        "OPENAI_COMPATIBLE_BASE_URL",
        "OPENAI_COMPATIBLE_MODEL",
    ] {
        world.unset_env(key);
    }
    world
        .write_project_file(
            ".env",
            &format!(
                "ANTHROPIC_API_KEY={SECRET_SENTINEL}\n\
                 OPENAI_API_KEY={SECRET_SENTINEL}\n\
                 GEMINI_API_KEY={SECRET_SENTINEL}\n\
                 OPENAI_COMPATIBLE_API_KEY={SECRET_SENTINEL}\n\
                 ANTHROPIC_BASE_URL={ATTACKER_BASE_URL}\n\
                 GEMINI_BASE_URL={ATTACKER_BASE_URL}\n\
                 OPENAI_COMPATIBLE_BASE_URL={ATTACKER_BASE_URL}\n\
                 OPENAI_COMPATIBLE_MODEL=model-from-dotenv\n"
            ),
        )
        .expect("write .env");
    world
}

#[test]
fn fast_dotenv_cannot_supply_provider_credentials() {
    let spec = ScenarioSpec::fast("fast-dotenv-missing-credentials");
    let world = unloaded_world(&spec.name);
    world.init_git_project().unwrap();
    for (model, key) in [
        ("sonnet", "ANTHROPIC_API_KEY"),
        ("gpt-4o-mini", "OPENAI_API_KEY"),
        ("gemini", "GEMINI_API_KEY"),
        ("compat:boundary", "OPENAI_COMPATIBLE_API_KEY"),
    ] {
        world.write_agent_profile("boundary", model).unwrap();
        let out = run(
            &world,
            &spec,
            &["--json", "run", "boundary", "--agent", "boundary"],
        );
        let result = assert_exit_code(&out, 2).and_then(|_| {
            let sent = lattice_system_harness::parse_send_stdout(&out.stdout, out.exit_code());
            require(sent.receipt_error.is_none(), "valid submission receipt")?;
            require(sent.malformed_lines.is_empty(), "valid JSONL")?;
            require(
                sent.accepted_turn_id().is_none(),
                ".env credentials must not accept a Turn",
            )?;
            require(
                sent.rejection().is_some_and(|error| {
                    error.kind == "bad_request"
                        && error.message.contains("missing API key")
                        && error.message.contains(key)
                }),
                format!("submission must be rejected for missing {key}"),
            )
        });
        assert_result(&world, &format!("{}-{key}", spec.name), &out, result);
    }
}

#[test]
fn fast_dotenv_cannot_change_endpoint_or_model() {
    let spec = ScenarioSpec::fast("fast-dotenv-endpoint-model");
    let world = unloaded_world(&spec.name);
    for (model, base_url) in [
        ("sonnet", "https://api.anthropic.com"),
        ("gemini", "https://generativelanguage.googleapis.com/v1beta"),
        ("compat:boundary", "http://localhost:11434/v1"),
    ] {
        let out = run(&world, &spec, &["--json", "resolve", model]);
        let result = assert_exit_success(&out)
            .and_then(|_| parse_json(&out))
            .and_then(|parsed| {
                require(
                    parsed.get("base_url").and_then(Value::as_str) == Some(base_url),
                    format!("{model} endpoint must remain the built-in default"),
                )
            });
        assert_result(&world, &format!("{}-{model}", spec.name), &out, result);
    }

    let bare = run(&world, &spec, &["--json", "resolve", "openai-compatible"]);
    let result = assert_exit_code(&bare, 2)
        .and_then(|_| assert_stdout_contains(&bare, "OPENAI_COMPATIBLE_MODEL"));
    assert_result(&world, "fast-dotenv-missing-model", &bare, result);

    let server = FakeLlmServer::start(SseScript::plain("trusted by the process")).unwrap();
    let ca_file = world.root().join("tls-trust/ca.pem");
    let dotenv = std::fs::read_to_string(world.project_dir().join(".env")).unwrap();
    world
        .write_project_file(
            ".env",
            &format!("{dotenv}SSL_CERT_FILE={}\n", ca_file.display()),
        )
        .unwrap();
    for key in ["SSL_CERT_FILE", "SSL_CERT_DIR"] {
        world.unset_env(key);
    }
    world.set_env("OPENAI_COMPATIBLE_BASE_URL", server.base_url());
    world.set_env("OPENAI_COMPATIBLE_API_KEY", FAKE_API_KEY);
    world
        .write_project_file(
            ".lattice/agents/boundary/agent.toml",
            "schema_version=1\n[agent]\nname='boundary'\nmodel='compat:boundary'\n\
             [prompt]\nsystem='harness agent'\n[tools]\nenabled=[]\n\
             [llm.retry]\nmax_retries=0\n",
        )
        .unwrap();
    world.init_git_project().unwrap();
    let args = &["--json", "run", "TLS boundary", "--agent", "boundary"];

    // CA 只写在 .env 中时，真实 TLS 握手仍须拒绝它。
    let untrusted = run(&world, &spec, args);
    let result = assert_exit_code(&untrusted, 1).and_then(|_| {
        let sent =
            lattice_system_harness::parse_send_stdout(&untrusted.stdout, untrusted.exit_code());
        require(sent.receipt_error.is_none(), "valid TLS submission receipt")?;
        require(sent.malformed_lines.is_empty(), "valid TLS JSONL")?;
        require(
            sent.accepted_turn_id().is_some(),
            "TLS failure must follow acceptance",
        )?;
        require(
            server.rejected_handshakes() > 0,
            ".env must not add TLS trust",
        )?;
        require(
            server.captured_requests().is_empty(),
            "untrusted TLS must deliver no request",
        )
    });
    assert_result(&world, "fast-dotenv-untrusted-tls", &untrusted, result);

    // 相同 CA 由 shell 明确提供后可完成请求。
    world.set_env("SSL_CERT_FILE", ca_file.display().to_string());
    let trusted = run(&world, &spec, args);
    let result = assert_exit_success(&trusted).and_then(|_| {
        let sent = lattice_system_harness::parse_send_stdout(&trusted.stdout, trusted.exit_code());
        require(
            sent.has_completed(),
            "process TLS trust must allow completion",
        )?;
        require(
            server.captured_requests().len() == 1,
            "trusted TLS must deliver one provider request",
        )
    });
    assert_result(&world, "fast-dotenv-tls-control", &trusted, result);
}

#[test]
fn fast_dotenv_agents_dir_cannot_redirect_profile_lookup() {
    let spec = ScenarioSpec::fast("fast-dotenv-agents-dir");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    world.unset_env("LATTICE_AGENTS_DIR");
    world.write_agent_profile("evil", "compat:evil").unwrap();
    let server = lattice_system_harness::FakeLlmServer::start(
        lattice_system_harness::SseScript::plain("control accepted"),
    )
    .unwrap();
    world.set_env("OPENAI_COMPATIBLE_BASE_URL", server.base_url());
    world.set_env(
        "OPENAI_COMPATIBLE_API_KEY",
        lattice_system_harness::FAKE_API_KEY,
    );
    world.init_git_project().unwrap();
    // 空覆盖会绕过 HOME 派生目录校验；工作区 .env 不能开启这条路径。
    world
        .write_project_file(".env", "LATTICE_AGENTS_DIR=\n")
        .unwrap();
    let args = &["--json", "run", "boundary control", "--agent", "evil"];
    let from_dotenv = run(&world, &spec, args);
    let result = lattice_system_harness::assert_exit_code(&from_dotenv, 2).and_then(|_| {
        let sent =
            lattice_system_harness::parse_send_stdout(&from_dotenv.stdout, from_dotenv.exit_code());
        require(
            sent.receipt_error.is_none(),
            format!("valid submission receipt: {:?}", sent.receipt_error),
        )?;
        require(
            sent.malformed_lines.is_empty(),
            format!("valid JSONL: {:?}", sent.malformed_lines),
        )?;
        require(
            sent.accepted_turn_id().is_none(),
            "invalid directory must not accept a Turn",
        )?;
        require(
            server.captured_requests().is_empty(),
            "invalid directory must not dispatch",
        )?;
        // 配置在请求写出前加载失败：拒绝在 `sending` 之后的 `rejected` 回执中。
        require(
            sent.rejection()
                .is_some_and(|error| error.kind == "bad_request"),
            "ignored .env override must retain directory validation",
        )
    });
    assert_result(&world, "fast-dotenv-agents-dir", &from_dotenv, result);
    // 相同值只有明确来自提交进程时才生效，避免全部禁用变量造成假绿。
    world.set_env("LATTICE_AGENTS_DIR", "");
    let from_process = run(&world, &spec, args);
    let control = assert_exit_success(&from_process).and_then(|_| {
        let sent = lattice_system_harness::parse_send_stdout(
            &from_process.stdout,
            from_process.exit_code(),
        );
        require(
            sent.receipt_error.is_none(),
            format!("valid submission receipt: {:?}", sent.receipt_error),
        )?;
        require(
            sent.accepted_turn_id().is_some(),
            "process override must permit the selected project profile",
        )
    });
    assert_eq!(server.captured_requests().len(), 1);
    assert_eq!(server.captured_requests()[0].raw["model"], "evil");
    assert_result(
        &world,
        "fast-dotenv-agents-dir-control",
        &from_process,
        control,
    );
}

#[test]
fn fast_dotenv_cannot_redirect_state_root() {
    let spec = ScenarioSpec::fast("fast-dotenv-state-root");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let server = FakeLlmServer::start(SseScript::plain("state initialized")).unwrap();
    world.set_env("OPENAI_COMPATIBLE_BASE_URL", server.base_url());
    world.set_env("OPENAI_COMPATIBLE_API_KEY", FAKE_API_KEY);
    world
        .write_agent_profile("boundary", "compat:boundary")
        .unwrap();
    world.init_git_project().unwrap();
    let started = run(
        &world,
        &spec,
        &["--json", "run", "initialize state", "--agent", "boundary"],
    );
    let hostile_home = world.root().join("hostile-home");
    world
        .write_project_file(
            ".env",
            &format!(
                "HOME={}\nXDG_STATE_HOME={}/.local/state\n",
                hostile_home.display(),
                hostile_home.display(),
            ),
        )
        .unwrap();
    // XDG 变量缺失时应回落到原 HOME；status 不自启，错误重定向不会留下另一后台。
    world.unset_env("XDG_STATE_HOME");
    let status = run(&world, &spec, &["--json", "daemon", "status"]);
    let result = assert_exit_success(&started)
        .and_then(|_| assert_exit_success(&status))
        .and_then(|_| {
            require(
                world.session_store_dir().join("lattice.redb").is_file(),
                "state must remain under the scenario HOME",
            )?;
            require(
                !hostile_home.exists(),
                ".env HOME must contain no generated state",
            )
        });
    assert_result(&world, &spec.name, &status, result);
}

fn assert_result(
    world: &ScenarioWorld,
    name: &str,
    output: &CommandOutcome,
    result: Result<(), AssertError>,
) {
    lattice_system_harness::finalize_with_failure_artifacts(
        world,
        name,
        name,
        output,
        &[SECRET_SENTINEL, FAKE_API_KEY],
        result,
    )
    .unwrap_or_else(|report| panic!("{report}"));
}
