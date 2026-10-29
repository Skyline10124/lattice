// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 经后台查询候选，保护元数据合并、状态过滤和安全输出。
mod support;
use lattice_system_harness::{FAKE_API_KEY, ProviderFixture, ScenarioSpec, ScenarioWorld};
use serde_json::{Value, json};
use support::{cli, finish_recorded};

#[test]
fn models_merge_metadata_and_render_readiness_without_secrets() {
    const CATALOG_KEY: &str = "ordinary.catalog.credential";
    let spec = ScenarioSpec::fast("models-candidates");
    let world = ScenarioWorld::new(&spec.name).unwrap();
    world.budget(&spec);
    let directory = world.home().join(".lattice");
    std::fs::create_dir_all(directory.join("cache")).unwrap();
    std::fs::write(
        directory.join("providers.toml"),
        r#"
[providers.local]
protocol = "chat-completions"
base_url = "http://localhost:9/v1"
auth = "bearer"
catalog = "arbitrary-entry"
[providers.local.models.current]
context = 999
[providers.local.models.manual]
output = 42
[providers.empty]
protocol = "chat-completions"
base_url = "http://localhost:9/v1"
auth = "none"
catalog = "absent-entry"
[providers.empty.models.unknown]
context = 12
"#,
    )
    .unwrap();
    std::fs::write(directory.join("cache/models.json"), serde_json::to_vec(&json!({
        "arbitrary-entry":{"models":{
            "current":{"id":"current","name":"Current","limit":{"context":100,"output":200},"cost":{"input":1.2,"output":3.4,"cache_read":0.1}},
            "retired":{"id":"retired","status":"deprecated"}
        }}
    })).unwrap()).unwrap();
    let mut evidence = lattice_system_harness::ScenarioEvidence::new(
        &world,
        &spec.name,
        [FAKE_API_KEY.to_owned(), CATALOG_KEY.to_owned()],
    )
    .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let missing = cli(&world, &spec, &["--json", "models", "--provider", "local"]);
        evidence.record_command("models missing key", &missing);
        assert!(missing.is_clean_exit(0));
        let list: Value = serde_json::from_slice(&missing.stdout).unwrap();
        let models = list["models"].as_array().unwrap();
        assert_eq!(models.len(), 2);
        let current = models
            .iter()
            .find(|model| model["model"] == "current")
            .unwrap();
        assert_eq!(current["context"], 999);
        assert_eq!(current["output"], 200);
        assert_eq!(current["cost"]["input"], 1.2);
        assert_eq!(current["credential_status"], "missing");
        let filtered = cli(
            &world,
            &spec,
            &["--json", "models", "--auth", "--provider", "local"],
        );
        evidence.record_command("models missing key filtered", &filtered);
        assert!(filtered.is_clean_exit(0));
        let filtered: Value = serde_json::from_slice(&filtered.stdout).unwrap();
        assert!(filtered["models"].as_array().unwrap().is_empty());
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
        let list: Value = serde_json::from_slice(&ready.stdout).unwrap();
        assert_eq!(list["models"].as_array().unwrap().len(), 2);
        assert!(
            list["models"]
                .as_array()
                .unwrap()
                .iter()
                .all(|model| model["credential_status"] == "ready")
        );
        let mixed = cli(&world, &spec, &["--json", "models", "--auth"]);
        evidence.record_command("models mixed credentials", &mixed);
        assert!(mixed.is_clean_exit(0));
        let mixed: Value = serde_json::from_slice(&mixed.stdout).unwrap();
        let models = mixed["models"].as_array().unwrap();
        assert!(models.iter().any(|model| model["provider"] == "empty"));
        assert!(models.iter().any(|model| model["provider"] == "local"));
        assert!(!models.iter().any(|model| model["provider"] == "anthropic"));
        assert!(
            models
                .iter()
                .all(|model| model["credential_status"] == "ready")
        );
        let text = cli(&world, &spec, &["models", "--provider", "local"]);
        evidence.record_command("models text", &text);
        assert!(text.is_clean_exit(0));
        let text = String::from_utf8(text.stdout).unwrap();
        for value in ["local/current", "999", "200", "1.2/3.4/0.1", "ready"] {
            assert!(text.contains(value));
        }
        let empty = cli(&world, &spec, &["--json", "models", "--provider", "empty"]);
        evidence.record_command("models missing catalog", &empty);
        let empty: Value = serde_json::from_slice(&empty.stdout).unwrap();
        assert_eq!(empty["models"][0]["credential_status"], "ready");
        assert!(
            empty["warnings"][0]["message"]
                .as_str()
                .unwrap()
                .contains("no metadata")
        );
        let source = std::fs::read_to_string(directory.join("providers.toml")).unwrap();
        world
            .register_key(
                ProviderFixture {
                    target: lattice_system_harness::ProviderTarget::Builtin("openai"),
                    ..ProviderFixture::chat_completions("unused")
                },
                Some(CATALOG_KEY),
            )
            .unwrap();
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
            assert!(warning.is_clean_exit(0));
            let public = format!(
                "{}{}",
                String::from_utf8_lossy(&warning.stdout),
                String::from_utf8_lossy(&warning.stderr)
            );
            assert!(
                !public.contains(CATALOG_KEY),
                "catalog must not expose a known credential"
            );
            assert!(public.contains("[REDACTED]"));
        }
        let chatgpt = cli(
            &world,
            &spec,
            &["--json", "models", "--provider", "chatgpt"],
        );
        evidence.record_command("models ChatGPT", &chatgpt);
        let chatgpt: Value = serde_json::from_slice(&chatgpt.stdout).unwrap();
        assert!(chatgpt["models"].as_array().unwrap().is_empty());
        assert!(
            chatgpt["warnings"][0]["message"]
                .as_str()
                .unwrap()
                .contains("not implemented")
        );
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
        assert!(short.is_clean_exit(0));
        let short: Value = serde_json::from_slice(&short.stdout).unwrap();
        assert!(
            short["warnings"][0]["message"]
                .as_str()
                .unwrap()
                .contains("EMPTY-catalog")
        );
        for directory_unavailable in [false, true] {
            if directory_unavailable {
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
            assert!(unavailable.is_clean_exit(0));
            let unavailable: Value = serde_json::from_slice(&unavailable.stdout).unwrap();
            let warnings = unavailable["warnings"].as_array().unwrap();
            let global: Vec<_> = warnings
                .iter()
                .filter(|warning| warning["provider"].is_null())
                .collect();
            assert_eq!(global.len(), 1);
            let reason = global[0]["message"].as_str().unwrap();
            assert!(reason.contains(if directory_unavailable {
                "permissions are too broad"
            } else {
                "auth.json"
            }));
            assert_eq!(
                warnings
                    .iter()
                    .filter(|warning| warning["provider"] == "chatgpt")
                    .count(),
                1
            );
            let text = cli(&world, &spec, &["models"]);
            evidence.record_command("models global credential warning text", &text);
            assert!(text.is_clean_exit(0));
            assert_eq!(
                String::from_utf8_lossy(&text.stderr)
                    .matches("provider credentials are not ready")
                    .count(),
                1
            );
            let missing = cli(&world, &spec, &["--json", "models", "--provider", "EMPTY"]);
            evidence.record_command(
                "models unavailable provider without readable credentials",
                &missing,
            );
            assert!(missing.is_clean_exit(2));
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
                assert!(message.contains(value));
            }
        }
        std::fs::set_permissions(
            &directory,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .unwrap();
    }));
    finish_recorded(evidence, result);
}
