// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 经后台查询候选，保护凭据过滤、脱敏和失败说明。
//! 元数据合并、废弃模型与缺失 catalog 的合并规则由 lattice-llm 的
//! `candidates_merge_metadata_filter_deprecated_and_report_missing_catalogs` 覆盖。
mod support;
use lattice_system_harness::{FAKE_API_KEY, ProviderFixture, ScenarioSpec, ScenarioWorld};
use serde_json::{Value, json};
use support::{cli, finish_recorded};

#[test]
fn backend_startup_observably_skips_disabled_automatic_metadata_refresh() {
    use lattice_system_harness::{LatticeDriver, RunningCommand, ScenarioEvidence};
    use std::time::Instant;

    let spec = ScenarioSpec::fast("metadata-auto-refresh-disabled");
    let world = ScenarioWorld::new(&spec.name).unwrap();
    let budget = world.budget(&spec);
    world.set_env("LATTICE_LOG", "info");
    let driver = LatticeDriver::from_env().unwrap();
    let log_path = world.root().join("daemon.stderr");
    let mut evidence = ScenarioEvidence::new(&world, &spec.name, Vec::<String>::new()).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // 经驱动登记场景产物，使显式 serve 也属于场景清理范围。
        let help = cli(&world, &spec, &["--help"]);
        evidence.record_command("register binary", &help);
        assert!(help.is_clean_exit(0));
        // exec 保留驱动创建的进程组；独立日志文件可观察自动刷新分支完成。
        let mut command = world.command("/bin/sh");
        command
            .args([
                "-c",
                "exec \"$1\" --verbose daemon serve --state-root \"$2\" 2>\"$3\"",
                "metadata-daemon",
            ])
            .arg(driver.binary_path())
            .arg(world.session_store_dir().parent().unwrap())
            .arg(&log_path);
        let mut daemon = RunningCommand::spawn(command).unwrap();
        let deadline = Instant::now() + budget.cap_timeout(spec.step_timeout());
        let log = loop {
            let log = match std::fs::read_to_string(&log_path) {
                Ok(log) => log,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(error) => panic!("daemon log: {error}"),
            };
            // 以跳过日志检查禁用分支的接线。
            if log.contains("automatic model metadata refresh disabled") {
                break log;
            }
            assert!(!daemon.has_exited().unwrap(), "daemon exited: {log}");
            assert!(
                Instant::now() < deadline,
                "automatic refresh did not report skipping: {log}"
            );
            std::thread::yield_now();
        };
        evidence
            .record_json("automatic refresh skipped", &json!({"log":log}))
            .unwrap();
        let stop = cli(&world, &spec, &["daemon", "stop"]);
        evidence.record_command("stop disabled refresh backend", &stop);
        assert!(stop.is_clean_exit(0));
        let stopped = daemon
            .wait(Some(budget.cap_timeout(spec.step_timeout())))
            .unwrap();
        evidence.record_command("disabled refresh backend", &stopped);
        assert!(stopped.is_clean_exit(0));
        assert!(!world.home().join(".lattice/cache/models.json").exists());
    }));
    finish_recorded(evidence, result);
}

#[test]
fn refresh_without_tls_roots_reports_the_reason_and_preserves_cached_candidates() {
    let spec = ScenarioSpec::fast("models-refresh-no-roots");
    let world = ScenarioWorld::new(&spec.name).unwrap();
    world.budget(&spec);
    world.set_env(
        "SSL_CERT_FILE",
        world.root().join("missing-root.pem").display().to_string(),
    );
    let directory = world.home().join(".lattice");
    std::fs::create_dir_all(directory.join("cache")).unwrap();
    let previous = br#"{"anthropic":{"models":{"cached-model":{"id":"cached-model","limit":{"output":8192}}}}}"#;
    std::fs::write(directory.join("cache/models.json"), previous).unwrap();
    let mut evidence =
        lattice_system_harness::ScenarioEvidence::new(&world, &spec.name, Vec::<String>::new())
            .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let refresh = cli(&world, &spec, &["--json", "models", "refresh"]);
        evidence.record_command("refresh without roots", &refresh);
        assert!(refresh.is_clean_exit(2));
        let error: Value = serde_json::from_slice(&refresh.stdout).unwrap();
        assert_eq!(error["kind"], "bad_request");
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains("no usable TLS root certificates")
        );
        let refresh = cli(&world, &spec, &["models", "refresh"]);
        evidence.record_command("refresh failure text", &refresh);
        assert!(refresh.is_clean_exit(2));
        assert!(
            String::from_utf8_lossy(&refresh.stderr).contains("no usable TLS root certificates")
        );
        let models = cli(
            &world,
            &spec,
            &["--json", "models", "--provider", "anthropic"],
        );
        evidence.record_command("candidates after failed refresh", &models);
        assert!(models.is_clean_exit(0));
        let models: Value = serde_json::from_slice(&models.stdout).unwrap();
        assert_eq!(models["models"][0]["model"], "cached-model");
        assert_eq!(models["models"][0]["output"], 8192);
        assert_eq!(
            std::fs::read(directory.join("cache/models.json")).unwrap(),
            previous
        );
    }));
    finish_recorded(evidence, result);
}

fn write_credential_providers(directory: &std::path::Path) {
    std::fs::create_dir_all(directory).unwrap();
    std::fs::write(
        directory.join("providers.toml"),
        r#"
[metadata]
auto_refresh = false
[providers.local]
protocol = "chat-completions"
base_url = "http://localhost:9/v1"
auth = "bearer"
[providers.local.models.current]
[providers.empty]
protocol = "chat-completions"
base_url = "http://localhost:9/v1"
auth = "none"
catalog = "absent-entry"
[providers.empty.models.listed]
"#,
    )
    .unwrap();
}

#[test]
fn models_hide_unready_credentials_and_keep_auth_none_ready() {
    let spec = ScenarioSpec::fast("models-credential-filter");
    let world = ScenarioWorld::new(&spec.name).unwrap();
    world.budget(&spec);
    write_credential_providers(&world.home().join(".lattice"));
    let mut evidence = lattice_system_harness::ScenarioEvidence::new(
        &world,
        &spec.name,
        [FAKE_API_KEY.to_owned()],
    )
    .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let missing = cli(&world, &spec, &["--json", "models", "--provider", "local"]);
        evidence.record_command("models missing key", &missing);
        assert!(missing.is_clean_exit(0), "missing key query failed");
        let list: Value = serde_json::from_slice(&missing.stdout).unwrap();
        let current = list["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["model"] == "current")
            .expect("handwritten local model");
        assert_eq!(
            current["credential_status"], "missing",
            "bearer provider without a key"
        );
        let filtered = cli(
            &world,
            &spec,
            &["--json", "models", "--auth", "--provider", "local"],
        );
        evidence.record_command("models missing key filtered", &filtered);
        assert!(filtered.is_clean_exit(0), "auth filter failed");
        let filtered: Value = serde_json::from_slice(&filtered.stdout).unwrap();
        assert!(
            filtered["models"].as_array().unwrap().is_empty(),
            "missing bearer credential stayed in --auth"
        );
        world
            .register_key(
                ProviderFixture::chat_completions("current"),
                Some(FAKE_API_KEY),
            )
            .unwrap();
        let ready = cli(
            &world,
            &spec,
            &["--json", "models", "--auth", "--provider", "local"],
        );
        evidence.record_command("models ready", &ready);
        assert!(ready.is_clean_exit(0), "ready query failed");
        let ready: Value = serde_json::from_slice(&ready.stdout).unwrap();
        assert!(
            ready["models"]
                .as_array()
                .unwrap()
                .iter()
                .all(|model| model["credential_status"] == "ready"),
            "stored key did not mark local ready: {ready}"
        );
        let mixed = cli(&world, &spec, &["--json", "models", "--auth"]);
        evidence.record_command("models mixed credentials", &mixed);
        assert!(mixed.is_clean_exit(0), "mixed auth query failed");
        let mixed: Value = serde_json::from_slice(&mixed.stdout).unwrap();
        let models = mixed["models"].as_array().unwrap();
        assert!(
            models.iter().any(|model| model["provider"] == "empty"),
            "auth=none provider missing from --auth"
        );
        assert!(
            models.iter().any(|model| model["provider"] == "local"),
            "ready bearer provider missing from --auth"
        );
        assert!(
            !models.iter().any(|model| model["provider"] == "anthropic"),
            "unready builtin provider leaked into --auth"
        );
        assert!(
            models
                .iter()
                .all(|model| model["credential_status"] == "ready"),
            "--auth listed a provider that is not ready"
        );
    }));
    finish_recorded(evidence, result);
}

#[test]
fn models_text_lists_the_selected_provider_without_the_key() {
    let spec = ScenarioSpec::fast("models-text-ready");
    let world = ScenarioWorld::new(&spec.name).unwrap();
    world.budget(&spec);
    write_credential_providers(&world.home().join(".lattice"));
    world
        .register_key(
            ProviderFixture::chat_completions("current"),
            Some(FAKE_API_KEY),
        )
        .unwrap();
    let mut evidence = lattice_system_harness::ScenarioEvidence::new(
        &world,
        &spec.name,
        [FAKE_API_KEY.to_owned()],
    )
    .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let text = cli(&world, &spec, &["models", "--provider", "local"]);
        evidence.record_command("models text", &text);
        assert!(text.is_clean_exit(0), "text models failed");
        let public = format!(
            "{}{}",
            String::from_utf8_lossy(&text.stdout),
            String::from_utf8_lossy(&text.stderr)
        );
        assert!(
            public.contains("local/current"),
            "text output omitted the model: {public}"
        );
        assert!(
            public.contains("ready"),
            "text output omitted credential status: {public}"
        );
        assert!(
            !public.contains(FAKE_API_KEY),
            "text output included the provider key"
        );
    }));
    finish_recorded(evidence, result);
}

#[test]
fn models_redact_catalog_ids_that_match_a_known_credential() {
    const CATALOG_KEY: &str = "ordinary.catalog.credential";
    let spec = ScenarioSpec::fast("models-catalog-redaction");
    let world = ScenarioWorld::new(&spec.name).unwrap();
    world.budget(&spec);
    let directory = world.home().join(".lattice");
    write_credential_providers(&directory);
    let mut evidence =
        lattice_system_harness::ScenarioEvidence::new(&world, &spec.name, [CATALOG_KEY.to_owned()])
            .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        world
            .register_key(
                ProviderFixture {
                    target: lattice_system_harness::ProviderTarget::Builtin("openai"),
                    ..ProviderFixture::chat_completions("unused")
                },
                Some(CATALOG_KEY),
            )
            .unwrap();
        let source = std::fs::read_to_string(directory.join("providers.toml")).unwrap();
        std::fs::write(
            directory.join("providers.toml"),
            source.replace("absent-entry", CATALOG_KEY),
        )
        .unwrap();
        for args in [
            vec!["--json", "models", "--provider", "empty"],
            vec!["models", "--provider", "empty"],
        ] {
            let warning = cli(&world, &spec, &args);
            evidence.record_command("models catalog credential redaction", &warning);
            assert!(warning.is_clean_exit(0), "catalog warning query failed");
            let public = format!(
                "{}{}",
                String::from_utf8_lossy(&warning.stdout),
                String::from_utf8_lossy(&warning.stderr)
            );
            assert!(
                !public.contains(CATALOG_KEY),
                "catalog id equal to a known credential was printed"
            );
            assert!(
                public.contains("[REDACTED]"),
                "known credential was not redacted: {public}"
            );
        }
        world
            .register_key(ProviderFixture::chat_completions("unused"), Some("EMPTY"))
            .unwrap();
        let source = std::fs::read_to_string(directory.join("providers.toml")).unwrap();
        std::fs::write(
            directory.join("providers.toml"),
            source.replace(CATALOG_KEY, "EMPTY-catalog"),
        )
        .unwrap();
        let short = cli(&world, &spec, &["--json", "models", "--provider", "empty"]);
        evidence.record_command("short credential does not mask catalog", &short);
        assert!(
            short.is_clean_exit(0),
            "short credential catalog query failed"
        );
        let short: Value = serde_json::from_slice(&short.stdout).unwrap();
        let message = short["warnings"][0]["message"].as_str().unwrap();
        assert!(
            message.contains("EMPTY-catalog"),
            "short credential masked a longer catalog id: {message}"
        );
    }));
    finish_recorded(evidence, result);
}

#[test]
fn models_report_unreadable_credentials() {
    let spec = ScenarioSpec::fast("models-unreadable-credentials");
    let world = ScenarioWorld::new(&spec.name).unwrap();
    world.budget(&spec);
    let directory = world.home().join(".lattice");
    write_credential_providers(&directory);
    let mut evidence =
        lattice_system_harness::ScenarioEvidence::new(&world, &spec.name, Vec::<String>::new())
            .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let started = cli(&world, &spec, &["--json", "models", "--provider", "empty"]);
        evidence.record_command("models before credential failure", &started);
        assert!(started.is_clean_exit(0), "initial models query failed");
        for (label, unavailable_directory) in [
            ("malformed auth file", false),
            ("broad directory permissions", true),
        ] {
            if unavailable_directory {
                std::fs::write(
                    directory.join("auth.json"),
                    "{\"version\":1,\"providers\":{}}",
                )
                .unwrap();
                std::fs::set_permissions(
                    &directory,
                    <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
                )
                .unwrap();
            } else {
                std::fs::write(directory.join("auth.json"), "invalid auth").unwrap();
            }
            let unavailable = cli(&world, &spec, &["--json", "models"]);
            evidence.record_command("models global credential warning", &unavailable);
            assert!(unavailable.is_clean_exit(0), "{label}: models exited");
            let unavailable: Value = serde_json::from_slice(&unavailable.stdout).unwrap();
            let warnings = unavailable["warnings"].as_array().unwrap();
            let global: Vec<_> = warnings
                .iter()
                .filter(|warning| warning["provider"].is_null())
                .collect();
            assert_eq!(global.len(), 1, "{label}: global warning count");
            let reason = global[0]["message"].as_str().unwrap();
            let expected = if unavailable_directory {
                "permissions are too broad"
            } else {
                "auth.json"
            };
            assert!(reason.contains(expected), "{label}: warning was {reason}");
            let text = cli(&world, &spec, &["models"]);
            evidence.record_command("models global credential warning text", &text);
            assert!(text.is_clean_exit(0), "{label}: text models exited");
            assert_eq!(
                String::from_utf8_lossy(&text.stderr)
                    .matches("provider credentials are not ready")
                    .count(),
                1,
                "{label}: text warning count"
            );
        }
        std::fs::set_permissions(
            &directory,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .unwrap();
    }));
    finish_recorded(evidence, result);
}

#[test]
fn models_name_available_providers_when_the_requested_one_is_absent() {
    let spec = ScenarioSpec::fast("models-unknown-provider");
    let world = ScenarioWorld::new(&spec.name).unwrap();
    world.budget(&spec);
    write_credential_providers(&world.home().join(".lattice"));
    let mut evidence =
        lattice_system_harness::ScenarioEvidence::new(&world, &spec.name, Vec::<String>::new())
            .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let missing = cli(&world, &spec, &["--json", "models", "--provider", "EMPTY"]);
        evidence.record_command("models unknown provider", &missing);
        assert!(missing.is_clean_exit(2), "unknown provider was accepted");
        let missing: Value = serde_json::from_slice(&missing.stdout).unwrap();
        let message = missing["message"].as_str().unwrap();
        for value in [
            "EMPTY",
            "available providers:",
            "anthropic",
            "openai",
            "google",
            "local",
        ] {
            assert!(
                message.contains(value),
                "unknown provider message omitted {value}: {message}"
            );
        }
    }));
    finish_recorded(evidence, result);
}
