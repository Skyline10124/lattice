// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 通过公开 CLI 验证工作区 `.env` 不提供凭据、模型、LATTICE 配置，
//! 也不能改变 endpoint、TLS 信任或状态根。

use lattice_system_harness::cli::run;
use lattice_system_harness::{
    AssertError, DaemonOptions, FAKE_API_KEY, FakeLlmServer, ProviderFixture, ProviderProtocol,
    ProviderTarget, ScenarioEvidence, ScenarioSpec, ScenarioWorld, SseScript, TlsIdentity,
    assert_exit_code, assert_exit_success,
};

const SECRET_SENTINEL: &str = "sk-ant-dotenv-sentinel-4b91";
const ATTACKER_BASE_URL: &str = "http://127.0.0.1:9/v1";

fn require(condition: bool, expected: impl Into<String>) -> Result<(), AssertError> {
    if condition {
        Ok(())
    } else {
        Err(AssertError::StdoutMissing {
            expected: expected.into(),
        })
    }
}

fn new_evidence(world: &ScenarioWorld, name: &str) -> ScenarioEvidence {
    ScenarioEvidence::new(
        world,
        name,
        [SECRET_SENTINEL.to_owned(), FAKE_API_KEY.to_owned()],
    )
    .expect("创建场景证据")
}

fn finish_evidence(evidence: ScenarioEvidence, result: Result<(), AssertError>) {
    evidence
        .finish_report(result.err().map(|error| error.to_string()))
        .unwrap_or_else(|report| panic!("{report}"));
}

fn unloaded_world(name: &str) -> ScenarioWorld {
    let world = ScenarioWorld::new(name).expect("创建场景世界");
    for key in [
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "XAI_API_KEY",
        "GOOGLE_API_KEY",
        "GEMINI_API_KEY",
        "OPENAI_COMPATIBLE_API_KEY",
        "ANTHROPIC_BASE_URL",
        "OPENAI_BASE_URL",
        "XAI_BASE_URL",
        "GOOGLE_BASE_URL",
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
        .expect("写入 .env");
    world
}

#[test]
fn process_environment_and_dotenv_cannot_supply_credentials_or_endpoints() {
    for source in ["process", "dotenv"] {
        for (id, protocol, key_env, url_env) in [
            (
                "anthropic",
                ProviderProtocol::AnthropicMessages,
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_BASE_URL",
            ),
            (
                "openai",
                ProviderProtocol::Responses,
                "OPENAI_API_KEY",
                "OPENAI_BASE_URL",
            ),
            (
                "xai",
                ProviderProtocol::Responses,
                "XAI_API_KEY",
                "XAI_BASE_URL",
            ),
            (
                "google",
                ProviderProtocol::Interactions,
                "GEMINI_API_KEY",
                "GEMINI_BASE_URL",
            ),
            (
                "local",
                ProviderProtocol::ChatCompletions,
                "OPENAI_COMPATIBLE_API_KEY",
                "OPENAI_COMPATIBLE_BASE_URL",
            ),
        ] {
            let spec = ScenarioSpec::fast(format!("provider-env-{source}-{id}"));
            let world = unloaded_world(&spec.name);
            world.budget(&spec);
            world.init_git_project().unwrap();
            let server = FakeLlmServer::start(SseScript::plain("formal provider")).unwrap();
            let hostile = FakeLlmServer::start(SseScript::plain("environment endpoint")).unwrap();
            let fixture = ProviderFixture {
                protocol,
                target: if id == "local" {
                    ProviderTarget::Custom(id)
                } else {
                    ProviderTarget::Builtin(id)
                },
                model: "boundary",
            };
            fixture.configure(&world, &server.base_url()).unwrap();
            world
                .write_agent_profile("boundary", &fixture.model_ref().unwrap())
                .unwrap();
            if source == "process" {
                world.set_env(key_env, SECRET_SENTINEL);
                world.set_env(url_env, hostile.base_url());
                if id == "google" {
                    world.set_env("GOOGLE_API_KEY", SECRET_SENTINEL);
                    world.set_env("GOOGLE_BASE_URL", hostile.base_url());
                }
                world.set_env("OPENAI_COMPATIBLE_MODEL", "hostile-model");
            } else {
                world
                    .write_project_file(
                        ".env",
                        &format!(
                            "{key_env}={SECRET_SENTINEL}\n{url_env}={}\nGOOGLE_API_KEY={SECRET_SENTINEL}\nGOOGLE_BASE_URL={}\nOPENAI_COMPATIBLE_MODEL=hostile-model\n",
                            hostile.base_url(),
                            hostile.base_url()
                        ),
                    )
                    .unwrap();
            }
            world.start_daemon().expect("显式启动后台");
            let mut evidence = new_evidence(&world, &spec.name);
            let result = (|| -> Result<(), AssertError> {
                let missing = run(
                    &world,
                    &spec,
                    &["--json", "run", "boundary", "--agent", "boundary"],
                );
                evidence.record_command("missing key rejection", &missing);
                assert_exit_code(&missing, 2)?;
                let sent =
                    lattice_system_harness::parse_send_stdout(&missing.stdout, missing.exit_code());
                require(
                    sent.rejection().is_some_and(|error| {
                        error.kind == "bad_request"
                            && error.message.contains(&format!("lattice auth set {id}"))
                    }),
                    "环境变量中的密钥不能通过轮次准入",
                )?;
                require(
                    server.captured_requests().is_empty(),
                    "准入拒绝不得发出上游请求",
                )?;
                world.register_key(fixture, Some(FAKE_API_KEY)).unwrap();
                let sent = run(
                    &world,
                    &spec,
                    &["--json", "run", "boundary", "--agent", "boundary"],
                );
                evidence.record_command("formal key accepted", &sent);
                assert_exit_success(&sent)?;
                let captures = server.captured_requests();
                require(captures.len() == 1, "正式凭据应恰好发出一笔请求")?;
                require(captures[0].model == "boundary", "模型名必须来自正式配置")?;
                let expected_provider = match id {
                    "openai" | "xai" => "responses",
                    "google" => "interactions",
                    "anthropic" => "anthropic",
                    _ => "openai",
                };
                require(
                    captures[0].provider == expected_provider,
                    "协议必须匹配正式配置",
                )?;
                require(
                    hostile.captured_requests().is_empty(),
                    "环境变量中的地址不得收到请求",
                )
            })();
            evidence.record_captures(server.captured_requests());
            finish_evidence(evidence, result);
        }
    }
}

#[test]
fn project_providers_are_ignored_and_old_model_syntax_is_configuration_rejection() {
    let spec = ScenarioSpec::fast("project-provider-boundary");
    let world = unloaded_world(&spec.name);
    world.budget(&spec);
    world.init_git_project().unwrap();
    world.start_daemon().expect("显式启动后台");
    let server = FakeLlmServer::start(SseScript::plain("project ignored")).unwrap();
    let hostile = FakeLlmServer::start(SseScript::plain("project endpoint")).unwrap();
    let fixture = ProviderFixture::chat_completions("boundary");
    world.wire_provider(fixture, &server, FAKE_API_KEY).unwrap();
    let project_config = format!(
        "[providers.local]\nprotocol='chat-completions'\nbase_url='{}'\nauth='bearer'\n[providers.project]\nprotocol='chat-completions'\nbase_url='{}'\nauth='none'\n",
        hostile.base_url(),
        hostile.base_url()
    );
    world
        .write_project_file("providers.toml", &project_config)
        .unwrap();
    world
        .write_project_file(".lattice/providers.toml", &project_config)
        .unwrap();
    let mut evidence = new_evidence(&world, &spec.name);
    let result = (|| -> Result<(), AssertError> {
        for model in ["project/model", "sonnet", "compat:boundary"] {
            world.write_agent_profile("boundary", model).unwrap();
            let out = run(
                &world,
                &spec,
                &["--json", "run", "boundary", "--agent", "boundary"],
            );
            evidence.record_command(format!("reject {model}"), &out);
            assert_exit_code(&out, 2)?;
            let sent = lattice_system_harness::parse_send_stdout(&out.stdout, out.exit_code());
            require(
                sent.rejection().is_some_and(|error| {
                    error.kind == "bad_request"
                        && if model.contains('/') {
                            error.message.contains("not registered")
                        } else {
                            error.message.contains("<provider>/<model>")
                        }
                }),
                "配置拒绝必须保留错误类型与原因",
            )?;
        }
        world
            .write_agent_profile("boundary", &fixture.model_ref().unwrap())
            .unwrap();
        let out = run(
            &world,
            &spec,
            &["--json", "run", "boundary", "--agent", "boundary"],
        );
        evidence.record_command("accept formal provider", &out);
        assert_exit_success(&out)?;
        require(
            server.captured_requests().len() == 1,
            "正式提供方应收到一笔请求",
        )?;
        require(
            hostile.captured_requests().is_empty(),
            "项目目录中的提供方配置不得生效",
        )
    })();
    evidence.record_captures(server.captured_requests());
    finish_evidence(evidence, result);
}

#[test]
fn dotenv_cannot_add_tls_trust_and_startup_environment_requires_restart() {
    let spec = ScenarioSpec::fast("dotenv-tls-startup-trust");
    let world = unloaded_world(&spec.name);
    world.budget(&spec);
    world.init_git_project().unwrap();
    let server = FakeLlmServer::start(SseScript::plain("startup trust accepted")).unwrap();
    let foreign = TlsIdentity::generate().unwrap();
    let startup_ca = world.root().join("startup-ca.pem");
    let target_ca = world.root().join("target-ca.pem");
    std::fs::write(&startup_ca, foreign.ca_pem()).unwrap();
    std::fs::write(&target_ca, TlsIdentity::shared().unwrap().ca_pem()).unwrap();
    // 后台以初始证书根启动；前端与工作区 `.env` 指向目标证书根，不能改变已运行后台的信任链。
    world
        .start_daemon_with(
            &DaemonOptions::new().env("SSL_CERT_FILE", startup_ca.display().to_string()),
        )
        .expect("以初始证书环境显式启动后台");
    world.set_env("SSL_CERT_FILE", target_ca.display().to_string());
    world
        .write_project_file(".env", &format!("SSL_CERT_FILE={}\n", target_ca.display()))
        .unwrap();
    world
        .write_agent_profile("boundary", "local/boundary")
        .unwrap();
    world
        .wire_provider(
            ProviderFixture::chat_completions("boundary"),
            &server,
            FAKE_API_KEY,
        )
        .unwrap();
    let mut evidence = new_evidence(&world, &spec.name);
    let result = (|| -> Result<(), AssertError> {
        let args = &["--json", "run", "TLS boundary", "--agent", "boundary"];
        let failed = run(&world, &spec, args);
        evidence.record_command("untrusted startup certificate", &failed);
        assert_exit_code(&failed, 1)?;
        let sent = lattice_system_harness::parse_send_stdout(&failed.stdout, failed.exit_code());
        require(sent.accepted_turn_id().is_some(), "轮次应在连接前已获接受")?;
        require(server.rejected_handshakes() > 0, "服务端应观察到握手被拒绝")?;
        require(
            server.captured_requests().is_empty(),
            "握手失败不得送达请求体",
        )?;
        world
            .restart_daemon_with(
                &DaemonOptions::new().env("SSL_CERT_FILE", target_ca.display().to_string()),
            )
            .expect("以目标证书环境显式重启后台");
        let accepted = run(&world, &spec, args);
        evidence.record_command("restarted target certificate", &accepted);
        assert_exit_success(&accepted)?;
        require(
            server.captured_requests().len() == 1,
            "显式重启加载新证书后应成功完成请求",
        )
    })();
    evidence.record_captures(server.captured_requests());
    finish_evidence(evidence, result);
}

#[test]
fn agents_use_backend_home_and_project_precedence() {
    let spec = ScenarioSpec::fast("agents-backend-home");
    let world = ScenarioWorld::new(&spec.name).expect("创建场景世界");
    world.budget(&spec);
    world.init_git_project().unwrap();
    let server = FakeLlmServer::start(SseScript::plain("profile accepted")).unwrap();
    let fixture = ProviderFixture::chat_completions("boundary");
    world
        .write_agent_profile("boundary", &fixture.model_ref().unwrap())
        .unwrap();
    let project_profile = world
        .project_dir()
        .join(".lattice/agents/boundary/agent.toml");
    let source = std::fs::read_to_string(&project_profile).unwrap();
    let global = world.home().join(".lattice/agents/boundary");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::write(
        global.join("agent.toml"),
        source.replace("harness agent", "global test agent"),
    )
    .unwrap();
    // 固定为 `umask 022` 的默认权限，不随机器 `umask` 变化：全局配置可被他人读取，
    // 只要不被同组或其他用户改写就应接受。
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &std::path::Path, mode: u32| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap()
        };
        mode(&world.home().join(".lattice/agents"), 0o755);
        mode(&global, 0o755);
        mode(&global.join("agent.toml"), 0o644);
    }
    std::fs::remove_file(&project_profile).unwrap();
    // 后台显式以场景 `HOME` 快照启动并录入凭据；前端随后切换为独立的 `HOME`，仍通过同一状态根连接后台。
    world.start_daemon().expect("以独立后台 HOME 显式启动后台");
    world.wire_provider(fixture, &server, FAKE_API_KEY).unwrap();
    let frontend_home = world.root().join("frontend-home");
    std::fs::create_dir_all(&frontend_home).unwrap();
    world.set_env("HOME", frontend_home.display().to_string());
    let mut evidence =
        ScenarioEvidence::new(&world, &spec.name, [FAKE_API_KEY.into()]).expect("创建场景证据");
    let result = (|| -> Result<(), AssertError> {
        let args = &["--json", "run", "profile sources", "--agent", "boundary"];
        let global_out = run(&world, &spec, args);
        evidence.record_command("global profile", &global_out);
        assert_exit_success(&global_out)?;
        std::fs::write(&project_profile, &source).unwrap();
        let project_out = run(&world, &spec, args);
        evidence.record_command("project profile", &project_out);
        assert_exit_success(&project_out)?;
        let captures = server.captured_requests();
        require(captures.len() == 2, "每次获接受的提交应各发出一笔请求")?;
        require(
            captures[0].raw["messages"][0]["content"] == "global test agent",
            "应读取后台 HOME 下的全局提示词",
        )?;
        require(
            captures[1].raw["messages"][0]["content"] == "harness agent",
            "项目级提示词优先级高于全局提示词",
        )?;
        std::fs::write(&project_profile, "schema_version=@invalid").unwrap();
        let rejected = run(&world, &spec, args);
        evidence.record_command("invalid project profile", &rejected);
        assert_exit_code(&rejected, 2)?;
        let sent =
            lattice_system_harness::parse_send_stdout(&rejected.stdout, rejected.exit_code());
        require(
            sent.rejection().is_some_and(|error| {
                error.kind == "bad_request" && error.message.contains("line 1")
            }),
            "损坏的项目配置不得回退到全局配置",
        )?;
        require(
            server.captured_requests().len() == 2,
            "被拒绝的配置不得发起请求",
        )
    })();
    evidence.record_captures(server.captured_requests());
    finish_evidence(evidence, result);
}

#[test]
fn fast_dotenv_cannot_redirect_state_root() {
    let spec = ScenarioSpec::fast("fast-dotenv-state-root");
    let world = ScenarioWorld::new(&spec.name).expect("创建场景世界");
    world.budget(&spec);
    world.start_daemon().expect("以场景状态根显式启动后台");
    let server = FakeLlmServer::start(SseScript::plain("state initialized")).unwrap();
    world
        .wire_provider(
            lattice_system_harness::ProviderFixture::chat_completions("boundary"),
            &server,
            FAKE_API_KEY,
        )
        .unwrap();
    world
        .write_agent_profile("boundary", "local/boundary")
        .unwrap();
    world.init_git_project().unwrap();
    let mut evidence = new_evidence(&world, &spec.name);
    let result = (|| -> Result<(), AssertError> {
        let started = run(
            &world,
            &spec,
            &["--json", "run", "initialize state", "--agent", "boundary"],
        );
        evidence.record_command("initialize state", &started);
        assert_exit_success(&started)?;
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
        // 前端环境移除 `XDG_STATE_HOME` 后应回退到前端 `HOME`，忽略工作区 `.env` 中的重定向并命中已运行的后台。
        world.unset_env("XDG_STATE_HOME");
        let status = run(&world, &spec, &["--json", "daemon", "status"]);
        evidence.record_command("daemon status without XDG_STATE_HOME", &status);
        assert_exit_success(&status)?;
        require(
            world.session_store_dir().join("lattice.redb").is_file(),
            "会话库必须保留在后台状态根下",
        )?;
        require(
            !hostile_home.exists(),
            "工作区 .env 中的 HOME 不得生成任何状态目录",
        )
    })();
    evidence.record_captures(server.captured_requests());
    finish_evidence(evidence, result);
}
