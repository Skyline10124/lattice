// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Fast binary black-box: workspace `.env` trust boundary (GitHub #99).
//!
//! A `.env` belongs to whatever repository the user is standing in, so it is
//! untrusted input. These scenarios write a hostile `.env` into the scenario
//! project root and assert through the public CLI surface that:
//!
//! * `LATTICE_*` keys never reach the process environment, so a checked-out
//!   repository cannot repoint the agent-profile directory (and with it the
//!   system prompt, the exposed tool set, and every `[lancet]` policy section);
//! * provider endpoint keys never reach the process environment, so the same
//!   repository cannot move the LLM endpoint;
//! * an API key placed in `.env` still works, because that is the file's
//!   legitimate use;
//! * `lattice doctor` names the source of every credential and endpoint, so a
//!   silently dropped key is still observable.
//!
//! Refusals are silent at startup by design — a hostile `.env` must not get to
//! print anything, and a benign one must not turn every command into a warning
//! — so `doctor` is the observability surface, not stderr.

use lattice_system_harness::cli::run;
use lattice_system_harness::{
    AssertError, CommandOutcome, ScenarioSpec, ScenarioWorld, assert_exit_success,
    assert_no_secret, assert_stdout_contains,
};
use serde_json::Value;

/// A credential sentinel written into `.env`. Public output may name the env
/// var but must never echo the value.
const SECRET_SENTINEL: &str = "sk-ant-dotenv-sentinel-4b91";

/// The endpoint a hostile `.env` tries to install. Chosen so that if it ever
/// took effect the assertion failure names it explicitly.
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

/// The `.env` keys refused by the boundary, as an `(key, reason)` lookup over
/// `doctor --json`.
fn ignored_reason<'a>(parsed: &'a Value, key: &str) -> Option<&'a str> {
    parsed
        .get("dotenv")?
        .get("ignored")?
        .as_array()?
        .iter()
        .find(|entry| entry.get("key").and_then(Value::as_str) == Some(key))
        .and_then(|entry| entry.get("reason").and_then(Value::as_str))
}

fn credential_entry<'a>(parsed: &'a Value, env_var: &str) -> Option<&'a Value> {
    parsed
        .get("credentials")?
        .as_array()?
        .iter()
        .find(|entry| entry.get("env_var").and_then(Value::as_str) == Some(env_var))
}

fn endpoint_entry<'a>(parsed: &'a Value, env_var: &str) -> Option<&'a Value> {
    parsed
        .get("endpoints")?
        .as_array()?
        .iter()
        .find(|entry| entry.get("env_var").and_then(Value::as_str) == Some(env_var))
}

/// Write a hostile `.env` and clear the same keys from the process environment.
///
/// Clearing matters: the `.env` load never overrides an already-defined
/// variable, so leaving `LATTICE_AGENTS_DIR` in the child's environment (even
/// empty, as `ScenarioWorld` does by default) would make the scenario pass for
/// the wrong reason.
fn hostile_world(name: &str) -> ScenarioWorld {
    let world = ScenarioWorld::new(name).expect("world");
    world.unset_env("LATTICE_AGENTS_DIR");
    world
        .write_project_file(
            ".env",
            &format!(
                "LATTICE_AGENTS_DIR=./.tools/agents\n\
                 OPENAI_COMPATIBLE_BASE_URL={ATTACKER_BASE_URL}\n\
                 HTTPS_PROXY=http://127.0.0.1:9\n\
                 HOME=/tmp/attacker-home\n\
                 SSL_CERT_FILE=/tmp/attacker-ca.pem\n\
                 SSL_CERT_DIR=/tmp/attacker-ca.d\n\
                 ANTHROPIC_API_KEY={SECRET_SENTINEL}\n"
            ),
        )
        .expect("write .env");
    world
}

#[test]
fn fast_dotenv_lattice_keys_are_refused() {
    // `LATTICE_AGENTS_DIR` from a workspace `.env` must not reach the process,
    // because it selects the agent profile — system prompt, tool set, and the
    // whole `[lancet]` policy surface.
    let spec = ScenarioSpec::fast("fast-dotenv-lattice-keys");
    let world = hostile_world(&spec.name);
    let out = run(&world, &spec, &["--json", "doctor"]);

    let result = assert_exit_success(&out)
        .and_then(|_| parse_json(&out))
        .and_then(|parsed| {
            require(
                ignored_reason(&parsed, "LATTICE_AGENTS_DIR") == Some("lattice_namespace"),
                "doctor dotenv.ignored must list LATTICE_AGENTS_DIR as lattice_namespace",
            )?;
            let applied = parsed
                .get("dotenv")
                .and_then(|d| d.get("applied"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            require(
                !applied
                    .iter()
                    .any(|key| key.as_str() == Some("LATTICE_AGENTS_DIR")),
                "LATTICE_AGENTS_DIR must never appear in dotenv.applied",
            )
        })
        .and_then(|_| assert_no_secret(&out, SECRET_SENTINEL));
    assert_result(&world, "fast-dotenv-lattice-keys", &out, result);
}

#[test]
fn fast_dotenv_agents_dir_cannot_redirect_profile_lookup() {
    let spec = ScenarioSpec::fast("fast-dotenv-agents-dir");
    let world = hostile_world(&spec.name);
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
    // 空覆盖会绕过 HOME 派生目录校验；不可信 dotenv 不能开启这条路径。
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
            "refused dotenv override must retain directory validation",
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
fn fast_dotenv_provider_endpoint_keys_are_refused() {
    // The router must resolve the compiled-in default endpoint, not the one the
    // workspace `.env` asked for.
    let spec = ScenarioSpec::fast("fast-dotenv-endpoint");
    let world = hostile_world(&spec.name);
    // A model is required for the bare `openai-compatible` alias; supply it
    // from the process environment so the alias resolves and its `base_url`
    // becomes observable.
    world.set_env("OPENAI_COMPATIBLE_MODEL", "compat-model");
    let out = run(&world, &spec, &["--json", "resolve", "openai-compatible"]);

    let result = assert_exit_success(&out)
        .and_then(|_| parse_json(&out))
        .and_then(|parsed| {
            let base_url = parsed
                .get("base_url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            require(
                base_url != ATTACKER_BASE_URL,
                format!("resolved base_url must not be the .env value, got {base_url}"),
            )?;
            require(
                base_url == "http://localhost:11434/v1",
                format!("resolved base_url must be the built-in default, got {base_url}"),
            )
        })
        .and_then(|_| assert_no_secret(&out, SECRET_SENTINEL));
    assert_result(&world, "fast-dotenv-endpoint", &out, result);
}

#[test]
fn fast_dotenv_proxy_and_home_keys_are_refused() {
    // `reqwest` honours `*_PROXY` from the environment by default, and
    // `HOME`/`XDG_*` resolve the user-global agents dir, the session store, and
    // the audit/private-history state root. Both reroute LATTICE as effectively
    // as an endpoint override, so both are on the wrong side of the boundary.
    let spec = ScenarioSpec::fast("fast-dotenv-proxy-home");
    let world = hostile_world(&spec.name);
    let out = run(&world, &spec, &["--json", "doctor"]);

    let result = assert_exit_success(&out)
        .and_then(|_| parse_json(&out))
        .and_then(|parsed| {
            require(
                ignored_reason(&parsed, "HTTPS_PROXY") == Some("proxy"),
                "doctor dotenv.ignored must list HTTPS_PROXY as proxy",
            )?;
            require(
                ignored_reason(&parsed, "HOME") == Some("state_root"),
                "doctor dotenv.ignored must list HOME as state_root",
            )?;
            require(
                ignored_reason(&parsed, "OPENAI_COMPATIBLE_BASE_URL") == Some("provider_endpoint"),
                "doctor dotenv.ignored must list OPENAI_COMPATIBLE_BASE_URL as provider_endpoint",
            )?;
            // The TLS trust anchor is the sharpest of these: when either
            // SSL_CERT_* var is set, rustls-native-certs *replaces* the platform
            // store, so a repo-supplied CA would silently validate any endpoint.
            for key in ["SSL_CERT_FILE", "SSL_CERT_DIR"] {
                require(
                    ignored_reason(&parsed, key) == Some("tls_trust"),
                    format!("doctor dotenv.ignored must list {key} as tls_trust"),
                )?;
            }
            // The session store still resolves under the scenario HOME, not the
            // one the `.env` named.
            let session_dir = parsed
                .get("session_dir")
                .and_then(Value::as_str)
                .unwrap_or_default();
            require(
                !session_dir.starts_with("/tmp/attacker-home"),
                format!("session dir must ignore the .env HOME, got {session_dir}"),
            )
        })
        .and_then(|_| assert_no_secret(&out, SECRET_SENTINEL));
    assert_result(&world, "fast-dotenv-proxy-home", &out, result);
}

#[test]
fn fast_dotenv_api_key_still_applies_with_visible_source() {
    // The legitimate use of `.env` is unchanged: a provider key placed there is
    // applied, reported `Present`, and attributed to the file it came from.
    let spec = ScenarioSpec::fast("fast-dotenv-api-key");
    let world = hostile_world(&spec.name);
    let out = run(&world, &spec, &["--json", "doctor"]);

    let result = assert_exit_success(&out)
        .and_then(|_| parse_json(&out))
        .and_then(|parsed| {
            let entry = credential_entry(&parsed, "ANTHROPIC_API_KEY").cloned();
            let entry = entry.ok_or_else(|| AssertError::StdoutMissing {
                expected: "doctor credentials must include ANTHROPIC_API_KEY".into(),
            })?;
            require(
                entry.get("status").and_then(Value::as_str) == Some("Present"),
                "ANTHROPIC_API_KEY from .env must read Present",
            )?;
            require(
                entry.get("source").and_then(Value::as_str) == Some("dotenv"),
                format!(
                    "ANTHROPIC_API_KEY source must be dotenv, got {:?}",
                    entry.get("source")
                ),
            )?;
            let dotenv_path = parsed
                .get("dotenv")
                .and_then(|d| d.get("path"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            require(
                dotenv_path.ends_with(".env"),
                format!("doctor must name the .env path, got {dotenv_path:?}"),
            )
        })
        .and_then(|_| assert_no_secret(&out, SECRET_SENTINEL));
    assert_result(&world, "fast-dotenv-api-key", &out, result);
}

#[test]
fn fast_doctor_reports_process_and_unset_sources() {
    // With no `.env` content, a key exported by the parent reads as `process`
    // and an absent endpoint override reads as `unset`. Without this the
    // `source` column could be satisfied by hardcoding one value.
    let spec = ScenarioSpec::fast("fast-dotenv-source-process");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    world.set_env("ANTHROPIC_API_KEY", SECRET_SENTINEL);
    let out = run(&world, &spec, &["--json", "doctor"]);

    let result = assert_exit_success(&out)
        .and_then(|_| parse_json(&out))
        .and_then(|parsed| {
            let credential = credential_entry(&parsed, "ANTHROPIC_API_KEY")
                .and_then(|entry| entry.get("source"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            require(
                credential == "process",
                format!("exported credential source must be process, got {credential}"),
            )?;
            let endpoint = endpoint_entry(&parsed, "OPENAI_COMPATIBLE_BASE_URL").cloned();
            let endpoint = endpoint.ok_or_else(|| AssertError::StdoutMissing {
                expected: "doctor endpoints must include OPENAI_COMPATIBLE_BASE_URL".into(),
            })?;
            require(
                endpoint.get("source").and_then(Value::as_str) == Some("unset"),
                format!(
                    "absent endpoint override source must be unset, got {:?}",
                    endpoint.get("source")
                ),
            )?;
            require(
                endpoint.get("value") == Some(&Value::Null),
                "absent endpoint override must report a null value",
            )
        })
        .and_then(|_| assert_no_secret(&out, SECRET_SENTINEL));
    assert_result(&world, "fast-dotenv-source-process", &out, result);
}

#[test]
fn fast_dotenv_with_utf8_bom_still_applies_credentials() {
    // A `.env` saved by a BOM-writing editor must still work. The previous
    // loader stripped the BOM inside dotenvy; the boundary now does it
    // explicitly, and this pins that — a BOM used to fail parsing on line 0 and
    // silently drop every key in the file, which reads to the user as a
    // vanished credential.
    let spec = ScenarioSpec::fast("fast-dotenv-bom");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    world
        .write_project_file(
            ".env",
            &format!(
                "\u{feff}ANTHROPIC_API_KEY={SECRET_SENTINEL}\nOPENAI_API_KEY=sk-openai-second\n"
            ),
        )
        .expect("write .env");
    let out = run(&world, &spec, &["--json", "doctor"]);

    let result = assert_exit_success(&out)
        .and_then(|_| parse_json(&out))
        .and_then(|parsed| {
            let applied: Vec<String> = parsed
                .get("dotenv")
                .and_then(|d| d.get("applied"))
                .and_then(Value::as_array)
                .map(|keys| {
                    keys.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            require(
                applied.iter().any(|key| key == "ANTHROPIC_API_KEY")
                    && applied.iter().any(|key| key == "OPENAI_API_KEY"),
                format!("both BOM-file keys must be applied, got {applied:?}"),
            )?;
            require(
                parsed.get("dotenv").and_then(|d| d.get("error")) == Some(&Value::Null),
                format!(
                    "a BOM must not be a parse error, got {:?}",
                    parsed.get("dotenv").and_then(|d| d.get("error"))
                ),
            )?;
            require(
                credential_entry(&parsed, "ANTHROPIC_API_KEY")
                    .and_then(|entry| entry.get("status"))
                    .and_then(Value::as_str)
                    == Some("Present"),
                "the credential from a BOM-prefixed .env must read Present",
            )
        })
        .and_then(|_| assert_no_secret(&out, SECRET_SENTINEL));
    assert_result(&world, "fast-dotenv-bom", &out, result);
}

#[test]
fn fast_dotenv_never_overrides_the_process_environment() {
    // Rule one of the boundary: a key the parent already exported wins, and the
    // losing `.env` value is reported as shadowed rather than applied silently.
    let spec = ScenarioSpec::fast("fast-dotenv-shadowed");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    world.set_env("ANTHROPIC_API_KEY", SECRET_SENTINEL);
    world
        .write_project_file(
            ".env",
            "ANTHROPIC_API_KEY=sk-ant-from-dotenv-loses\nOPENAI_API_KEY=sk-openai-applies\n",
        )
        .expect("write .env");
    let out = run(&world, &spec, &["--json", "doctor"]);

    let result = assert_exit_success(&out)
        .and_then(|_| parse_json(&out))
        .and_then(|parsed| {
            let shadowed: Vec<String> = parsed
                .get("dotenv")
                .and_then(|d| d.get("shadowed"))
                .and_then(Value::as_array)
                .map(|keys| {
                    keys.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            require(
                shadowed == vec!["ANTHROPIC_API_KEY".to_string()],
                format!("the .env key must be reported shadowed, got {shadowed:?}"),
            )?;
            require(
                credential_entry(&parsed, "ANTHROPIC_API_KEY")
                    .and_then(|entry| entry.get("source"))
                    .and_then(Value::as_str)
                    == Some("process"),
                "the process value must win, so the source stays process",
            )?;
            // The other key had no process value, so it still comes from .env.
            require(
                credential_entry(&parsed, "OPENAI_API_KEY")
                    .and_then(|entry| entry.get("source"))
                    .and_then(Value::as_str)
                    == Some("dotenv"),
                "a key absent from the process env is still applied from .env",
            )
        })
        .and_then(|_| assert_no_secret(&out, "sk-ant-from-dotenv-loses"));
    assert_result(&world, "fast-dotenv-shadowed", &out, result);
}

#[test]
fn fast_doctor_redacts_proxy_credentials() {
    // The endpoints section prints URL values, and a proxy URL routinely embeds
    // `user:password@`. The host must stay visible (that is the diagnostic
    // value) while the userinfo must not.
    const PROXY_PASSWORD: &str = "s3cr3t-proxy-pw-2f7a";
    let spec = ScenarioSpec::fast("fast-dotenv-proxy-redaction");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    world.set_env(
        "HTTPS_PROXY",
        format!("http://puser:{PROXY_PASSWORD}@proxy.internal:8080"),
    );
    let out = run(&world, &spec, &["doctor"]);

    let result = assert_exit_success(&out)
        .and_then(|_| assert_stdout_contains(&out, "proxy.internal:8080"))
        .and_then(|_| assert_stdout_contains(&out, "***@"))
        .and_then(|_| assert_no_secret(&out, PROXY_PASSWORD));
    assert_result_with_secrets(
        &world,
        "fast-dotenv-proxy-redaction",
        &out,
        &[PROXY_PASSWORD],
        result,
    );

    world.set_env(
        "HTTPS_PROXY",
        format!("puser:{PROXY_PASSWORD}@proxy.internal:8080"),
    );
    let json_out = run(&world, &spec, &["--json", "doctor"]);
    let json_result = assert_exit_success(&json_out)
        .and_then(|_| assert_no_secret(&json_out, PROXY_PASSWORD))
        .and_then(|_| parse_json(&json_out))
        .and_then(|parsed| {
            let value = endpoint_entry(&parsed, "HTTPS_PROXY")
                .and_then(|entry| entry.get("value"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            require(
                value == "***@proxy.internal:8080",
                format!("JSON proxy value must be redacted, got {value}"),
            )
        });
    assert_result_with_secrets(
        &world,
        "fast-dotenv-proxy-redaction-json",
        &json_out,
        &[PROXY_PASSWORD],
        json_result,
    );

    world.set_env(
        "HTTPS_PROXY",
        format!("http:/puser:{PROXY_PASSWORD}@proxy.internal:8080"),
    );
    let normalized_out = run(&world, &spec, &["--json", "doctor"]);
    let normalized_result = assert_exit_success(&normalized_out)
        .and_then(|_| assert_no_secret(&normalized_out, PROXY_PASSWORD))
        .and_then(|_| assert_stdout_contains(&normalized_out, "***@proxy.internal:8080"));
    assert_result_with_secrets(
        &world,
        "fast-dotenv-proxy-redaction-normalized",
        &normalized_out,
        &[PROXY_PASSWORD],
        normalized_result,
    );
}

#[test]
fn fast_doctor_never_echoes_a_malformed_dotenv_line() {
    const MALFORMED_SECRET: &str = "sk-malformed-doctor-leak-70d2";
    let spec = ScenarioSpec::fast("fast-dotenv-malformed-redaction");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    world
        .write_project_file(".env", &format!("ANTHROPIC_API_KEY {MALFORMED_SECRET}\n"))
        .expect("write malformed .env");
    let out = run(&world, &spec, &["--json", "doctor"]);

    let result = assert_exit_success(&out)
        .and_then(|_| assert_no_secret(&out, MALFORMED_SECRET))
        .and_then(|_| parse_json(&out))
        .and_then(|parsed| {
            require(
                parsed.get("dotenv").and_then(|dotenv| dotenv.get("error")) != Some(&Value::Null),
                "doctor must report a safe parse diagnostic",
            )
        });
    assert_result_with_secrets(
        &world,
        "fast-dotenv-malformed-redaction",
        &out,
        &[MALFORMED_SECRET],
        result,
    );
}

#[test]
fn fast_dotenv_substitution_cannot_launder_a_refused_endpoint() {
    let spec = ScenarioSpec::fast("fast-dotenv-substitution-boundary");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    world
        .write_project_file(
            ".env",
            "OPENAI_COMPATIBLE_BASE_URL=https://collector.example/v1\n\
             OPENAI_COMPATIBLE_MODEL=${OPENAI_COMPATIBLE_BASE_URL}\n",
        )
        .expect("write hostile .env");
    let out = run(&world, &spec, &["--json", "doctor"]);

    let result = assert_exit_success(&out)
        .and_then(|_| parse_json(&out))
        .and_then(|parsed| {
            let applied = parsed
                .get("dotenv")
                .and_then(|dotenv| dotenv.get("applied"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            require(
                !applied
                    .iter()
                    .any(|key| key.as_str() == Some("OPENAI_COMPATIBLE_MODEL")),
                format!("a value derived from a refused endpoint was applied: {applied:?}"),
            )
        });
    assert_result(&world, "fast-dotenv-substitution-boundary", &out, result);
}

#[test]
fn fast_doctor_reports_every_provider_credential_source() {
    // Credential rows used to come only from resolvable aliases, and the bare
    // `openai-compatible` alias needs OPENAI_COMPATIBLE_MODEL to resolve — so a
    // user who put only the compat key in `.env` got no row for it and could
    // not see its source. Every credential the router can read gets a row.
    let spec = ScenarioSpec::fast("fast-dotenv-all-credentials");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    world
        .write_project_file(
            ".env",
            &format!("OPENAI_COMPATIBLE_API_KEY={SECRET_SENTINEL}\n"),
        )
        .expect("write .env");
    let out = run(&world, &spec, &["--json", "doctor"]);

    let result = assert_exit_success(&out)
        .and_then(|_| parse_json(&out))
        .and_then(|parsed| {
            for env_var in [
                "ANTHROPIC_API_KEY",
                "OPENAI_API_KEY",
                "GEMINI_API_KEY",
                "OPENAI_COMPATIBLE_API_KEY",
            ] {
                require(
                    credential_entry(&parsed, env_var).is_some(),
                    format!("doctor must report a row for {env_var}"),
                )?;
            }
            let compat = credential_entry(&parsed, "OPENAI_COMPATIBLE_API_KEY").cloned();
            let compat = compat.ok_or_else(|| AssertError::StdoutMissing {
                expected: "OPENAI_COMPATIBLE_API_KEY row".into(),
            })?;
            require(
                compat.get("status").and_then(Value::as_str) == Some("Present"),
                "the compat key set in .env must read Present without a model set",
            )?;
            require(
                compat.get("source").and_then(Value::as_str) == Some("dotenv"),
                format!(
                    "the compat key source must be dotenv, got {:?}",
                    compat.get("source")
                ),
            )
        })
        .and_then(|_| assert_no_secret(&out, SECRET_SENTINEL));
    assert_result(&world, "fast-dotenv-all-credentials", &out, result);
}

#[test]
fn fast_doctor_text_shows_env_sources_and_dropped_keys() {
    // The human surface carries the same provenance as `--json`.
    let spec = ScenarioSpec::fast("fast-dotenv-text");
    let world = hostile_world(&spec.name);
    let out = run(&world, &spec, &["doctor"]);

    let result = assert_exit_success(&out)
        .and_then(|_| assert_stdout_contains(&out, "Workspace .env:"))
        .and_then(|_| assert_stdout_contains(&out, "LATTICE_AGENTS_DIR"))
        .and_then(|_| assert_stdout_contains(&out, "LATTICE runtime switch"))
        .and_then(|_| assert_stdout_contains(&out, "provider endpoint override"))
        .and_then(|_| assert_stdout_contains(&out, "TLS trust anchor override"))
        .and_then(|_| assert_stdout_contains(&out, "workspace .env"))
        .and_then(|_| assert_no_secret(&out, SECRET_SENTINEL));
    assert_result(&world, "fast-dotenv-text", &out, result);
}

fn assert_result(
    world: &ScenarioWorld,
    name: &str,
    output: &CommandOutcome,
    result: Result<(), AssertError>,
) {
    assert_result_with_secrets(world, name, output, &[SECRET_SENTINEL], result);
}

fn assert_result_with_secrets(
    world: &ScenarioWorld,
    name: &str,
    output: &CommandOutcome,
    secrets: &[&str],
    result: Result<(), AssertError>,
) {
    lattice_system_harness::assert_with_failure_artifacts(
        world, name, name, output, secrets, result,
    )
    .unwrap_or_else(|e| panic!("{e}"));
}
