// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Fast binary black-box: model resolution, config discovery, and secret
//! redaction (issue #40, Codeberg #18, "fast binary black-box" matrix; triage
//! replacement target `planned_config_cli`).
//!
//! These scenarios drive the built `lattice` binary for the no-turn config
//! surface (`models`, `resolve`, `doctor`). They assert public stdout/JSON
//! fields and that no credential value ever appears in output - only env-var
//! names. No turn is executed, so these need no turn runtime at all. They supersede
//! the crate-internal `cli_diagnostics_models_*`, `cli_diagnostics_resolve_*`,
//! and `doctor_*` unit tests in `lattice-cli` (see `docs/TEST-TRIAGE.md`).

use std::process::Output;

use lattice_system_harness::{
    AssertError, LatticeCommand, LatticeDriver, ScenarioSpec, ScenarioWorld, assert_exit_failure,
    assert_exit_success, assert_no_secret, assert_stderr_contains, assert_stdout_contains,
    assert_stdout_contains_all, assert_with_failure_artifacts,
};
use serde_json::Value;

fn driver() -> LatticeDriver {
    LatticeDriver::from_env().expect("LATTICE_BIN must point to a built lattice binary")
}

/// A recognisable credential sentinel. Scenarios set it as an API key value
/// and then assert it never appears in any command output: the public surface
/// may name the env var (`ANTHROPIC_API_KEY`) but must never echo the value.
const SECRET_SENTINEL: &str = "sk-ant-harness-sentinel-7c3f";

fn run(world: &ScenarioWorld, spec: &ScenarioSpec, args: &[&str]) -> Output {
    driver()
        .run(
            world,
            &LatticeCommand {
                args: args.iter().map(|s| s.to_string()).collect(),
                timeout: Some(spec.step_timeout()),
            },
        )
        .expect("drive lattice")
        .output
}

fn ok_or_artifacts(
    world: &ScenarioWorld,
    name: &str,
    summary: &str,
    output: &Output,
    result: Result<(), AssertError>,
) {
    assert_with_failure_artifacts(world, name, summary, output, &[SECRET_SENTINEL], result)
        .unwrap_or_else(|e| panic!("{e}"));
}

fn parse_json<T: serde::de::DeserializeOwned>(output: &Output) -> Result<T, AssertError> {
    serde_json::from_slice(&output.stdout).map_err(|error| AssertError::StdoutMissing {
        expected: format!("valid JSON output: {error}"),
    })
}

#[test]
fn fast_models_lists_all_aliases_one_per_line() {
    let spec = ScenarioSpec::fast("fast-models-list");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(&world, &spec, &["models"]);

    let result = assert_exit_success(&out).and_then(|_| {
        assert_stdout_contains_all(&out, &["sonnet", "opus", "gpt-5.5", "gemini", "haiku"])
    });
    ok_or_artifacts(&world, "fast-models-list", "lattice models", &out, result);
}

#[test]
fn fast_models_json_is_valid_array_with_credential_status() {
    let spec = ScenarioSpec::fast("fast-models-json");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(&world, &spec, &["models", "--json"]);

    let result = assert_exit_success(&out)
        .and_then(|_| parse_json::<Vec<Value>>(&out))
        .and_then(|parsed| {
            if parsed.is_empty() {
                return Err(AssertError::StdoutMissing {
                    expected: "non-empty alias array".into(),
                });
            }
            for item in &parsed {
                if item.get("alias").and_then(|v| v.as_str()).is_none()
                    || item
                        .get("credential_status")
                        .and_then(|v| v.as_str())
                        .is_none()
                {
                    return Err(AssertError::StdoutMissing {
                        expected: "alias + credential_status on every entry".into(),
                    });
                }
            }
            Ok(())
        })
        .and_then(|_| assert_no_secret(&out, SECRET_SENTINEL));
    ok_or_artifacts(
        &world,
        "fast-models-json",
        "lattice models --json",
        &out,
        result,
    );
}

#[test]
fn fast_models_auth_with_no_credentials_lists_nothing() {
    let spec = ScenarioSpec::fast("fast-models-auth-empty");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(&world, &spec, &["models", "--auth"]);

    // Env is cleared by the world; no API keys are present, so `--auth` lists
    // no credentialed aliases. Assert success and that no alias leaks through.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let result = assert_exit_success(&out).and_then(|_| {
        if stdout.lines().any(|line| !line.trim().is_empty()) {
            return Err(AssertError::StdoutMissing {
                expected: "no aliases when no credentials are present".into(),
            });
        }
        Ok(())
    });
    ok_or_artifacts(
        &world,
        "fast-models-auth-empty",
        "lattice models --auth",
        &out,
        result,
    );
}

#[test]
fn fast_models_auth_with_anthropic_key_lists_anthropic_aliases_only() {
    // `models --auth` prints one credentialed alias per line (renderer at
    // `crates/lattice-cli/src/commands/models.rs:6-24`). With only the
    // Anthropic key set, the output is exactly the Anthropic aliases - per-line
    // parsed and set-compared so reordering, merging lines, icons, URLs, or a
    // secret sentinel all fail this scenario. Note: the old
    // `cli_cutover_models_auth_one_alias_per_line_without_icons_or_secrets`
    // test (see `git show codeberg/main:crates/lattice-cli/tests/cli_cutover.rs`)
    // asserted non-empty/no-secret/no-icon/no-URL but did NOT count alias
    // lines; this one does, and rejects gpt-5.5 aliases whose credential is
    // absent.
    let spec = ScenarioSpec::fast("fast-models-auth-keyed");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    world.set_env("ANTHROPIC_API_KEY", SECRET_SENTINEL);
    let out = run(&world, &spec, &["models", "--auth"]);

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let lines: Vec<&str> = stdout
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    let result = assert_exit_success(&out)
        .and_then(|_| {
            // Each non-empty line must be exactly one alias token: no icon
            // markers, no URL scheme, no secret value.
            for line in &lines {
                if line.contains('✓')
                    || line.contains('✗')
                    || line.contains("http://")
                    || line.contains("https://")
                    || line.contains(SECRET_SENTINEL)
                {
                    return Err(AssertError::StdoutMissing {
                        expected: format!("alias-only line, got {line:?}"),
                    });
                }
            }
            // The line count must match the expected alias count exactly.
            // Without this, a HashSet comparison would mask duplicate alias
            // lines (e.g. "sonnet\nsonnet\nopus\nhaiku" would pass the set
            // check but violate the one-alias-per-line contract).
            let expected_count = 3; // opus, sonnet, haiku
            if lines.len() != expected_count {
                return Err(AssertError::StdoutMissing {
                    expected: format!("{expected_count} alias lines, got {}", lines.len()),
                });
            }
            // The credentialed Anthropic aliases, set-compared so output order
            // does not matter. Current catalog: opus/sonnet/haiku.
            let got: std::collections::HashSet<&str> = lines.iter().copied().collect();
            let expected: std::collections::HashSet<&str> =
                ["opus", "sonnet", "haiku"].into_iter().collect();
            if got != expected {
                return Err(AssertError::StdoutMissing {
                    expected: format!("aliases == {expected:?}, got {got:?}"),
                });
            }
            Ok(())
        })
        .and_then(|_| {
            // gpt-5.5 (no OpenAI credential present) must not appear anywhere.
            if stdout.contains("gpt-5.5") {
                return Err(AssertError::StdoutMissing {
                    expected: "gpt-5.5 absent (OPENAI_API_KEY not set)".into(),
                });
            }
            Ok(())
        })
        .and_then(|_| assert_no_secret(&out, SECRET_SENTINEL));
    ok_or_artifacts(
        &world,
        "fast-models-auth-keyed",
        "lattice models --auth (keyed)",
        &out,
        result,
    );
}

#[test]
fn fast_resolve_text_shows_provider_and_model() {
    let spec = ScenarioSpec::fast("fast-resolve-text");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(&world, &spec, &["resolve", "sonnet"]);

    let result = assert_exit_success(&out)
        .and_then(|_| assert_stdout_contains(&out, "Anthropic"))
        .and_then(|_| assert_stdout_contains(&out, "claude-sonnet"));
    ok_or_artifacts(
        &world,
        "fast-resolve-text",
        "lattice resolve sonnet",
        &out,
        result,
    );
}

#[test]
fn fast_resolve_json_includes_fields_and_excludes_secret_value() {
    let spec = ScenarioSpec::fast("fast-resolve-json");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    world.set_env("ANTHROPIC_API_KEY", SECRET_SENTINEL);
    let out = run(&world, &spec, &["resolve", "sonnet", "--json"]);

    let result = assert_exit_success(&out)
        .and_then(|_| parse_json::<Value>(&out))
        .and_then(|parsed| {
            let status = parsed.get("credential_status").and_then(|v| v.as_str());
            let provider = parsed.get("provider").and_then(|v| v.as_str());
            if status != Some("Present") || provider != Some("Anthropic") {
                return Err(AssertError::StdoutMissing {
                    expected: "credential_status=Present, provider=Anthropic".into(),
                });
            }
            // `provider_model` is the public, actionable resolved-model field.
            // It must exist, be a non-empty string. We do not pin the exact
            // model version (catalog may move); only that it is named.
            // Renderer contract: `crates/lattice-cli/src/commands/model_render.rs:6-15`.
            let provider_model = parsed.get("provider_model").and_then(|v| v.as_str());
            if provider_model.map(|m| m.is_empty()).unwrap_or(true) {
                return Err(AssertError::StdoutMissing {
                    expected: format!("provider_model non-empty string, got {provider_model:?}"),
                });
            }
            // `api_key_env` is the public diagnostic field naming the env var;
            // the missing-credential scenario also asserts this stays named.
            if parsed.get("api_key_env").and_then(|v| v.as_str()) != Some("ANTHROPIC_API_KEY") {
                return Err(AssertError::StdoutMissing {
                    expected: "api_key_env == ANTHROPIC_API_KEY".into(),
                });
            }
            Ok(())
        })
        .and_then(|_| assert_no_secret(&out, SECRET_SENTINEL));
    ok_or_artifacts(
        &world,
        "fast-resolve-json",
        "lattice resolve sonnet --json",
        &out,
        result,
    );
}

#[test]
fn fast_resolve_verbose_includes_auth_env_and_credential_status() {
    let spec = ScenarioSpec::fast("fast-resolve-verbose");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    world.set_env("ANTHROPIC_API_KEY", SECRET_SENTINEL);
    let out = run(&world, &spec, &["resolve", "sonnet", "--verbose"]);

    let result = assert_exit_success(&out)
        .and_then(|_| assert_stdout_contains(&out, "Auth Env: ANTHROPIC_API_KEY"))
        .and_then(|_| assert_stdout_contains(&out, "Credential Status: Present"))
        .and_then(|_| assert_no_secret(&out, SECRET_SENTINEL));
    ok_or_artifacts(
        &world,
        "fast-resolve-verbose",
        "lattice resolve sonnet --verbose",
        &out,
        result,
    );
}

#[test]
fn fast_resolve_unknown_model_errors() {
    let spec = ScenarioSpec::fast("fast-resolve-unknown");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(&world, &spec, &["resolve", "nope-xyz"]);

    let result =
        assert_exit_failure(&out).and_then(|_| assert_stderr_contains(&out, "unknown model alias"));
    ok_or_artifacts(
        &world,
        "fast-resolve-unknown",
        "lattice resolve nope-xyz",
        &out,
        result,
    );
}

#[test]
fn fast_doctor_text_lists_env_var_names_not_values() {
    let spec = ScenarioSpec::fast("fast-doctor-text");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    world.set_env("ANTHROPIC_API_KEY", SECRET_SENTINEL);
    let out = run(&world, &spec, &["doctor"]);

    let result = assert_exit_success(&out)
        .and_then(|_| assert_stdout_contains(&out, "ANTHROPIC_API_KEY"))
        .and_then(|_| assert_stdout_contains(&out, "Session Store"))
        .and_then(|_| assert_stdout_contains(&out, "Dir:"))
        .and_then(|_| assert_stdout_contains(&out, "aliases"))
        .and_then(|_| assert_no_secret(&out, SECRET_SENTINEL));
    ok_or_artifacts(&world, "fast-doctor-text", "lattice doctor", &out, result);
}

#[test]
fn fast_doctor_json_does_not_leak_secrets() {
    let spec = ScenarioSpec::fast("fast-doctor-json");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    world.set_env("ANTHROPIC_API_KEY", SECRET_SENTINEL);
    let out = run(&world, &spec, &["--json", "doctor"]);

    let result = assert_exit_success(&out)
        .and_then(|_| parse_json::<Value>(&out))
        .and_then(|parsed| {
            let credentials = parsed.get("credentials").and_then(|v| v.as_array());
            let creds = match credentials {
                Some(a) if !a.is_empty() => a,
                _ => {
                    return Err(AssertError::StdoutMissing {
                        expected: "credentials non-empty array".into(),
                    });
                }
            };
            // Every credential entry is an object with `env_var`/`status`
            // strings; status uses the public Present/Missing vocabulary
            // (renderer at `crates/lattice-cli/src/commands/doctor.rs:67-79`).
            for cred in creds {
                let env_var = cred.get("env_var").and_then(|v| v.as_str());
                let status = cred.get("status").and_then(|v| v.as_str());
                if env_var.map(|s| s.is_empty()).unwrap_or(true) || status.is_none() {
                    return Err(AssertError::StdoutMissing {
                        expected: "every credential has env_var + status strings".into(),
                    });
                }
                if !matches!(status, Some("Present") | Some("Missing")) {
                    return Err(AssertError::StdoutMissing {
                        expected: format!("status in {{Present, Missing}}, got {status:?}"),
                    });
                }
            }
            // The Anthropic credential is set (world injects the sentinel),
            // so it must report Present.
            let anthropic = creds
                .iter()
                .find(|c| c.get("env_var").and_then(|v| v.as_str()) == Some("ANTHROPIC_API_KEY"));
            if !matches!(
                anthropic,
                Some(c) if c.get("status").and_then(|v| v.as_str()) == Some("Present")
            ) {
                return Err(AssertError::StdoutMissing {
                    expected: format!("ANTHROPIC_API_KEY credential Present, got {anthropic:?}"),
                });
            }
            // Keep the existing alias_count numeric gate.
            if parsed.get("alias_count").and_then(|v| v.as_i64()).is_none() {
                return Err(AssertError::StdoutMissing {
                    expected: "alias_count integer".into(),
                });
            }
            Ok(())
        })
        .and_then(|_| assert_no_secret(&out, SECRET_SENTINEL));
    ok_or_artifacts(
        &world,
        "fast-doctor-json",
        "lattice --json doctor",
        &out,
        result,
    );
}

#[test]
fn fast_models_lists_openai_compatible_when_configured() {
    // The `openai-compatible` alias is gated on OPENAI_COMPATIBLE_BASE_URL
    // (and a model); without it the alias is absent, so `models` must list it
    // only when the user has configured the compatibility endpoint.
    let spec = ScenarioSpec::fast("fast-models-compat");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    world.set_env("OPENAI_COMPATIBLE_BASE_URL", "http://127.0.0.1:1/v1");
    world.set_env("OPENAI_COMPATIBLE_API_KEY", SECRET_SENTINEL);
    world.set_env("OPENAI_COMPATIBLE_MODEL", "compat-model");
    let out = run(&world, &spec, &["models"]);

    let result = assert_exit_success(&out)
        .and_then(|_| assert_stdout_contains(&out, "openai-compatible"))
        .and_then(|_| assert_no_secret(&out, SECRET_SENTINEL));
    ok_or_artifacts(
        &world,
        "fast-models-compat",
        "lattice models (compat configured)",
        &out,
        result,
    );
}

#[test]
fn fast_doctor_text_shows_missing_status_without_credentials() {
    // With the env cleared and no API keys set, every credential must report
    // Missing (covers the crate-internal `doctor_credentials_reflect_missing_status`).
    let spec = ScenarioSpec::fast("fast-doctor-missing");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(&world, &spec, &["doctor"]);

    let result = assert_exit_success(&out)
        .and_then(|_| assert_stdout_contains(&out, "ANTHROPIC_API_KEY"))
        .and_then(|_| assert_stdout_contains(&out, "missing"));
    ok_or_artifacts(
        &world,
        "fast-doctor-missing",
        "lattice doctor (no creds)",
        &out,
        result,
    );
}

#[test]
fn fast_resolve_json_without_credential_names_env_var_and_missing_status() {
    // With no API key set, `resolve sonnet --json` must still succeed and name
    // the env var (`ANTHROPIC_API_KEY`) so a missing-credential failure is
    // actionable. Covers the crate-internal `smoke_resolve_missing_credential_names_env_var`.
    let spec = ScenarioSpec::fast("fast-resolve-missing");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(&world, &spec, &["resolve", "sonnet", "--json"]);

    let result = assert_exit_success(&out)
        .and_then(|_| parse_json::<Value>(&out))
        .and_then(|parsed| {
            let env = parsed.get("api_key_env").and_then(|v| v.as_str());
            let status = parsed.get("credential_status").and_then(|v| v.as_str());
            if env != Some("ANTHROPIC_API_KEY") || status != Some("Missing") {
                return Err(AssertError::StdoutMissing {
                    expected: "api_key_env=ANTHROPIC_API_KEY, credential_status=Missing".into(),
                });
            }
            Ok(())
        });
    ok_or_artifacts(
        &world,
        "fast-resolve-missing",
        "lattice resolve sonnet --json (no key)",
        &out,
        result,
    );
}
