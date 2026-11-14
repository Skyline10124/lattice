// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 经后台查询候选，保护凭据过滤、脱敏和失败说明。
//! 元数据合并、废弃模型与缺失目录的合并规则由 `lattice-llm` 的
//! `candidates_merge_metadata_filter_deprecated_and_report_missing_catalogs` 覆盖；
//! 这里只核对 `lattice models` 显示了合并结果。
mod support;
use lattice_system_harness::{FAKE_API_KEY, ProviderFixture, ScenarioSpec, ScenarioWorld};
use serde_json::{Value, json};
use support::{cli, finish_recorded};

#[test]
fn backend_startup_observably_skips_disabled_automatic_metadata_refresh() {
    use lattice_system_harness::{DaemonOptions, ScenarioEvidence};
    use std::time::Instant;

    let spec = ScenarioSpec::fast("metadata-auto-refresh-disabled");
    let world = ScenarioWorld::new(&spec.name).unwrap();
    let budget = world.budget(&spec);
    let mut evidence = ScenarioEvidence::new(&world, &spec.name, Vec::<String>::new()).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        world
            .start_daemon_with(&DaemonOptions::new().env("LATTICE_LOG", "info"))
            .expect("显式启动禁用自动刷新的后台");
        let deadline = Instant::now() + budget.cap_timeout(spec.step_timeout());
        let log = loop {
            let log = String::from_utf8_lossy(&world.daemon_stderr()).into_owned();
            // 以跳过日志检查禁用分支的接线。
            if log.contains("automatic model metadata refresh disabled") {
                break log;
            }
            assert!(
                !world.daemon_has_exited().unwrap(),
                "后台在写出禁用刷新日志前提前退出: {log}"
            );
            assert!(
                Instant::now() < deadline,
                "后台未在预算内写出跳过自动刷新日志: {log}"
            );
            std::thread::yield_now();
        };
        evidence
            .record_json("automatic refresh skipped", &json!({"log":log}))
            .unwrap();
        let stop = cli(&world, &spec, &["daemon", "stop"]);
        evidence.record_command("停止禁用自动刷新的后台", &stop);
        assert!(stop.is_clean_exit(0));
        world.daemon_departed().unwrap();
        let stopped = world
            .take_daemon_outcome()
            .expect("回收禁用刷新后台的退出结果");
        evidence.record_command("禁用自动刷新的后台进程退出", &stopped);
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
    world.start_daemon().expect("以缺失证书根环境显式启动后台");
    let mut evidence =
        lattice_system_harness::ScenarioEvidence::new(&world, &spec.name, Vec::<String>::new())
            .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let refresh = cli(&world, &spec, &["--json", "models", "refresh"]);
        evidence.record_command("无可用信任根时的 models refresh（JSON）", &refresh);
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
        evidence.record_command("无可用信任根时的 models refresh（文本）", &refresh);
        assert!(refresh.is_clean_exit(2));
        assert!(
            String::from_utf8_lossy(&refresh.stderr).contains("no usable TLS root certificates")
        );
        let models = cli(
            &world,
            &spec,
            &["--json", "models", "--provider", "anthropic"],
        );
        evidence.record_command("刷新失败后查询模型候选缓存", &models);
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

#[test]
fn models_display_merged_metadata_hide_deprecated_and_warn_missing_catalog() {
    let spec = ScenarioSpec::fast("models-metadata-display");
    let world = ScenarioWorld::new(&spec.name).unwrap();
    let budget = world.budget(&spec);
    world.start_daemon().expect("显式启动后台");
    let directory = world.home().join(".lattice");
    std::fs::create_dir_all(directory.join("cache")).unwrap();
    std::fs::write(
        directory.join("providers.toml"),
        r#"
[metadata]
auto_refresh = false
[providers.local]
protocol = "chat-completions"
base_url = "http://localhost:9/v1"
auth = "none"
catalog = "arbitrary-entry"
[providers.local.models.current]
context = 999
[providers.empty]
protocol = "chat-completions"
base_url = "http://localhost:9/v1"
auth = "none"
catalog = "absent-entry"
[providers.empty.models.listed]
[providers.openai]
catalog = "sort-catalog"
[providers.openai.models.sort-z]
[providers.openai.models.sort-a]
[providers.anthropic.models.sort-a]
[providers.xai.models.sort-a]
[providers.zzz]
protocol = "chat-completions"
base_url = "http://localhost:9/v1"
auth = "none"
[providers.zzz.models.sort-a]
[providers.aaa]
protocol = "chat-completions"
base_url = "http://localhost:9/v1"
auth = "none"
catalog = "sort-catalog"
[providers.aaa.models.sort-z]
[providers.aaa.models.sort-a]
[providers.pending-responses]
protocol = "responses"
base_url = "http://localhost:9/v1"
auth = "none"
catalog = "arbitrary-entry"
[providers.pending-interactions]
protocol = "interactions"
base_url = "http://localhost:9/v1"
auth = "none"
catalog = "arbitrary-entry"
"#,
    )
    .unwrap();
    std::fs::write(directory.join("cache/models.json"), serde_json::to_vec(&json!({
        "arbitrary-entry":{"models":{
            "current":{"id":"current","name":"Current","limit":{"context":100,"output":200},"cost":{"input":1.2,"output":3.4,"cache_read":0.1,"cache_write":0.2},"reasoning_options":[{"type":"effort","values":["high","none"]}]},
            "retired":{"id":"retired","status":"deprecated"}
        }},
        "sort-catalog":{"models":{
            "sort-a":{"id":"sort-a","name":"Z Model"},
            "sort-z":{"id":"sort-z","name":"A Model"}
        }},
        "openai":{"models":{
            "current":{"id":"current","name":"Current","reasoning_options":[{"type":"effort","values":["none","high"]}]}
        }}
    })).unwrap()).unwrap();
    let mut evidence = lattice_system_harness::ScenarioEvidence::new(
        &world,
        &spec.name,
        [FAKE_API_KEY.to_owned()],
    )
    .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let local = cli(&world, &spec, &["--json", "models", "--provider", "local"]);
        evidence.record_command("查询合并元数据后的模型列表（JSON）", &local);
        assert!(local.is_clean_exit(0), "合并元数据模型查询失败");
        let local: Value = serde_json::from_slice(&local.stdout).unwrap();
        let models = local["models"].as_array().unwrap();
        let current = models
            .iter()
            .find(|model| model["model"] == "current")
            .expect("合并结果应列出 current 模型");
        assert_eq!(
            current["context"], 999,
            "手写 context 配置应优先于目录元数据"
        );
        assert_eq!(current["output"], 200, "output 应取自目录元数据");
        assert_eq!(current["supported_efforts"], json!(["off", "high"]));
        assert_eq!(
            current["cost"],
            json!({"input":1.2,"output":3.4,"cache_read":0.1,"cache_write":0.2}),
            "价格信息应取自目录元数据"
        );
        assert!(
            !models.iter().any(|model| model["model"] == "retired"),
            "已弃用的 retired 模型必须隐藏，实际返回：{models:?}"
        );
        let text = cli(&world, &spec, &["models", "--provider", "local"]);
        evidence.record_command("查询合并元数据后的模型列表（文本）", &text);
        assert!(text.is_clean_exit(0), "合并元数据文本查询失败");
        let text = String::from_utf8(text.stdout).unwrap();
        assert_eq!(
            text.lines().next(),
            Some(
                "MODEL\tCONTEXT\tOUTPUT\tEFFORTS\tPRICE ($/M tokens: input/output/cache-read/cache-write)\tAUTH"
            ),
            "文本表头必须包含价格与凭据列"
        );
        assert_eq!(
            text.lines()
                .find(|line| line.starts_with("local/current\t")),
            Some("local/current\t999\t200\toff,high\t1.2/3.4/0.1/0.2\tready"),
            "文本行必须包含模型限额、价格与凭据状态"
        );
        assert!(
            !text.contains("retired"),
            "已弃用的 retired 模型不得出现在文本输出中：{text}"
        );
        let empty = cli(&world, &spec, &["--json", "models", "--provider", "empty"]);
        evidence.record_command("查询缺失目录元数据的 provider 模型", &empty);
        assert!(empty.is_clean_exit(0), "缺失目录模型查询失败");
        let empty: Value = serde_json::from_slice(&empty.stdout).unwrap();
        assert_eq!(empty["models"][0]["supported_efforts"], json!([]));
        assert!(
            empty["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("no metadata"))),
            "缺少目录元数据时应给出 no metadata 告警，实际返回：{empty}"
        );
        let empty_text = cli(&world, &spec, &["models", "--provider", "empty"]);
        evidence.record_command("查询无档位模型的文本输出", &empty_text);
        assert!(empty_text.is_clean_exit(0));
        assert!(
            String::from_utf8_lossy(&empty_text.stdout)
                .lines()
                .any(|line| line == "empty/listed\t-\t-\t-\t-/-/-/-\tready")
        );
        let ordered = cli(&world, &spec, &["--json", "models"]);
        evidence.record_command("查询候选 provider 与模型的排序结果", &ordered);
        assert!(ordered.is_clean_exit(0));
        let ordered: Value = serde_json::from_slice(&ordered.stdout).unwrap();
        let names: Vec<_> = ordered["models"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|model| model["model"].as_str().unwrap().starts_with("sort-"))
            .map(|model| {
                format!(
                    "{}/{}",
                    model["provider"].as_str().unwrap(),
                    model["model"].as_str().unwrap()
                )
            })
            .collect();
        assert_eq!(
            names,
            [
                "openai/sort-a",
                "openai/sort-z",
                "anthropic/sort-a",
                "xai/sort-a",
                "aaa/sort-a",
                "aaa/sort-z",
                "zzz/sort-a"
            ]
        );
        for (target, model, efforts) in [
            ("local/retired", "retired", json!([])),
            ("local/org/unknown", "org/unknown", json!([])),
            ("empty/unknown", "unknown", json!([])),
            (
                "pending-responses/current",
                "current",
                json!(["off", "high"]),
            ),
            ("pending-interactions/current", "current", json!(["high"])),
        ] {
            let reply = support::ipc(
                &world,
                budget.cap_timeout(spec.step_timeout()),
                json!({"operation":"models", "query":{"target":target}}),
            );
            evidence
                .record_json("exact model metadata", &reply)
                .unwrap();
            assert_eq!(reply["response"], "models");
            let models = reply["value"]["models"].as_array().unwrap();
            assert_eq!(models.len(), 1);
            assert_eq!(models[0]["model"], model);
            assert_eq!(models[0]["supported_efforts"], efforts);
            assert_eq!(models[0]["availability"], json!({"status":"ready"}));
            if model == "org/unknown" {
                for field in ["name", "context", "output", "cost"] {
                    assert!(models[0][field].is_null());
                }
            }
            if target == "empty/unknown" {
                assert!(
                    reply["value"]["warnings"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|warning| warning["message"]
                            .as_str()
                            .unwrap()
                            .contains("no metadata"))
                );
            }
        }
        let subscription = support::ipc(
            &world,
            budget.cap_timeout(spec.step_timeout()),
            json!({"operation":"models", "query":{"target":"chatgpt/current"}}),
        );
        evidence
            .record_json("chatgpt capabilities from openai metadata", &subscription)
            .unwrap();
        assert_eq!(subscription["response"], "models");
        assert_eq!(subscription["value"]["models"].as_array().unwrap().len(), 1);
        assert_eq!(subscription["value"]["models"][0]["model"], "current");
        assert_eq!(subscription["value"]["models"][0]["name"], "Current");
        assert_eq!(
            subscription["value"]["models"][0]["supported_efforts"],
            json!(["off", "high"])
        );
        for provider in ["pending-responses", "pending-interactions"] {
            let output = cli(&world, &spec, &["--json", "models", "--provider", provider]);
            evidence.record_command("查询指定协议 provider 的模型列表", &output);
            assert!(output.is_clean_exit(0));
            let listed: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(listed["models"].as_array().unwrap().len(), 1);
            assert_eq!(listed["models"][0]["model"], "current");
            assert_eq!(
                listed["models"][0]["supported_efforts"],
                if provider == "pending-responses" {
                    json!(["off", "high"])
                } else {
                    json!(["high"])
                }
            );
        }
        let config_path = directory.join("providers.toml");
        let mut config: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        for id in ["openai", "xai", "google"] {
            let provider = config["providers"]
                .as_table_mut()
                .unwrap()
                .entry(id)
                .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                .as_table_mut()
                .unwrap();
            provider.insert("catalog".into(), "builtin-capabilities".into());
            let models = provider
                .entry("models")
                .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                .as_table_mut()
                .unwrap();
            for model in ["handwritten", "budget"] {
                models.insert(
                    model.into(),
                    toml::Value::Table(toml::from_str("effort=['low','high']").unwrap()),
                );
            }
        }
        std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();
        let cache_path = directory.join("cache/models.json");
        let mut cache: Value =
            serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
        cache["builtin-capabilities"] = json!({"models":{
            "effort":{"id":"effort","reasoning_options":[{"type":"effort","values":["none","low","high","xhigh"]}]},
            "budget":{"id":"budget","reasoning_options":[{"type":"toggle"},{"type":"budget_tokens"}]},
            "toggle":{"id":"toggle","reasoning_options":[{"type":"toggle"}]}
        }});
        std::fs::write(cache_path, serde_json::to_vec(&cache).unwrap()).unwrap();
        for provider in ["openai", "xai", "google"] {
            for (model, expected) in [
                (
                    "effort",
                    if provider == "google" {
                        json!(["low", "high"])
                    } else {
                        json!(["off", "low", "high", "xhigh"])
                    },
                ),
                ("budget", json!([])),
                ("toggle", json!([])),
                ("handwritten", json!(["low", "high"])),
                ("unknown", json!([])),
            ] {
                let target = format!("{provider}/{model}");
                let listed = support::ipc(
                    &world,
                    budget.cap_timeout(spec.step_timeout()),
                    json!({"operation":"models","query":{"target":target}}),
                );
                evidence
                    .record_json(&format!("builtin capabilities {provider}-{model}"), &listed)
                    .unwrap();
                assert_eq!(listed["response"], "models");
                let candidate = &listed["value"]["models"][0];
                assert_eq!(candidate["supported_efforts"], expected, "{target}");
                assert_eq!(
                    candidate["availability"],
                    json!({"status":"unconfigured", "requirement":"api_key"}),
                    "{target}: {candidate}"
                );
            }
        }
        let config = directory.join("providers.toml");
        let source = std::fs::read_to_string(&config).unwrap();
        std::fs::write(
            config,
            format!(
                "{source}\n[providers.pending-interactions.models.invalid]\neffort=['xhigh']\n"
            ),
        )
        .unwrap();
        let output = cli(
            &world,
            &spec,
            &["--json", "models", "--provider", "pending-interactions"],
        );
        evidence.record_command("查询 Interactions 模型级非法档位配置", &output);
        assert!(output.is_clean_exit(0));
        let listed: Value = serde_json::from_slice(&output.stdout).unwrap();
        let models = listed["models"].as_array().unwrap();
        let invalid = models
            .iter()
            .find(|model| model["model"] == "invalid")
            .unwrap();
        assert_eq!(invalid["availability"]["status"], "invalid");
        assert!(
            invalid["availability"]["reason"]
                .as_str()
                .unwrap()
                .contains("thinking_level")
        );
        assert_eq!(invalid["supported_efforts"], json!([]));
        assert_eq!(
            models
                .iter()
                .find(|model| model["model"] == "current")
                .unwrap()["supported_efforts"],
            json!(["high"])
        );
        for query in [
            json!({"operation":"models", "query":{"list":{"provider":"missing","ready_only":false}}}),
            json!({"operation":"models", "query":{"target":"missing/model"}}),
        ] {
            let reply = support::ipc(&world, budget.cap_timeout(spec.step_timeout()), query);
            evidence
                .record_json("invalid exact model query", &reply)
                .unwrap();
            assert_eq!(reply["response"], "error");
            assert_eq!(reply["value"]["kind"], "bad_request");
        }
        let providers_path = directory.join("providers.toml");
        let invalid_effort = |provider: &str, model: &str| {
            format!(
                "effort = ['low']\n[providers.{provider}.models.{model}.features]\nthinking_toggle = 'enable-thinking'"
            )
        };
        let configuration = std::fs::read_to_string(&providers_path)
            .unwrap()
            .replace(
                "[providers.empty.models.listed]",
                &format!(
                    "[providers.empty.models.good]\neffort = ['off', 'high']\n[providers.empty.models.good.features]\nthinking_toggle = 'enable-thinking'\n[providers.empty.models.listed]\n{}",
                    invalid_effort("empty", "listed")
                ),
            )
            .replace(
                "[providers.local.models.current]",
                &format!(
                    "[providers.local.models.retired]\n{}\n[providers.local.models.current]",
                    invalid_effort("local", "retired")
                ),
            );
        std::fs::write(&providers_path, configuration).unwrap();
        // 手写档位无效只拒绝该模型：列表成功，好模型照常列出，坏模型标出配置错误。
        let invalid = cli(&world, &spec, &["--json", "models", "--provider", "empty"]);
        evidence.record_command("查询含无效手写档位的模型列表（JSON）", &invalid);
        assert!(invalid.is_clean_exit(0), "{}", invalid.summary());
        let invalid: Value = serde_json::from_slice(&invalid.stdout).unwrap();
        let models = invalid["models"].as_array().unwrap();
        let good = models
            .iter()
            .find(|model| model["model"] == "good")
            .unwrap();
        assert_eq!(good["supported_efforts"], json!(["off", "high"]));
        assert_eq!(good["availability"], json!({"status":"ready"}), "{good}");
        let listed = models
            .iter()
            .find(|model| model["model"] == "listed")
            .unwrap();
        assert_eq!(listed["supported_efforts"], json!([]));
        let exact = support::ipc(
            &world,
            budget.cap_timeout(spec.step_timeout()),
            json!({"operation":"models", "query":{"target":"empty/listed"}}),
        );
        evidence
            .record_json("invalid handwritten effort exact query", &exact)
            .unwrap();
        assert_eq!(exact["response"], "models");
        assert_eq!(exact["value"]["models"][0]["supported_efforts"], json!([]));
        for availability in [
            &listed["availability"],
            &exact["value"]["models"][0]["availability"],
        ] {
            assert_eq!(availability["status"], "invalid", "{availability}");
            let message = availability["reason"].as_str().unwrap();
            for expected in [
                "provider \"empty\"",
                "model \"listed\"",
                "effort low",
                "thinking_toggle",
            ] {
                assert!(message.contains(expected), "{message}");
            }
        }
        let text = cli(&world, &spec, &["models", "--provider", "empty"]);
        evidence.record_command("查询含无效手写档位的模型列表（文本）", &text);
        assert!(text.is_clean_exit(0), "{}", text.summary());
        let stdout = String::from_utf8(text.stdout).unwrap();
        assert!(
            stdout.lines().any(|line| line.starts_with("empty/good\t")
                && line.split('\t').nth(3) == Some("off,high")),
            "{stdout}"
        );
        assert!(
            stdout.lines().any(|line| line.starts_with("empty/listed\t")
                && line.split('\t').nth(3) == Some("config-error")),
            "{stdout}"
        );
        assert!(
            String::from_utf8_lossy(&text.stderr)
                .lines()
                .any(|line| line.starts_with("empty/listed: ") && line.contains("thinking_toggle")),
            "{}",
            String::from_utf8_lossy(&text.stderr)
        );
        // 已弃用模型先被滤掉，其无效配置既不出现也不让列表失败。
        let local = cli(&world, &spec, &["--json", "models", "--provider", "local"]);
        evidence.record_command("查询带无效手写档位的已弃用模型过滤结果", &local);
        assert!(local.is_clean_exit(0), "{}", local.summary());
        let local: Value = serde_json::from_slice(&local.stdout).unwrap();
        let local = local["models"].as_array().unwrap();
        assert!(
            !local.iter().any(|model| model["model"] == "retired"),
            "{local:?}"
        );
        assert!(
            local
                .iter()
                .all(|model| model["availability"] == json!({"status":"ready"})),
            "{local:?}"
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
catalog = "local-absent"
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
    world.start_daemon().expect("显式启动后台");
    let mut evidence = lattice_system_harness::ScenarioEvidence::new(
        &world,
        &spec.name,
        [FAKE_API_KEY.to_owned()],
    )
    .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let missing = cli(&world, &spec, &["--json", "models", "--provider", "local"]);
        evidence.record_command("录入密钥前查询模型列表", &missing);
        assert!(missing.is_clean_exit(0), "未配置密钥时的模型查询失败");
        let list: Value = serde_json::from_slice(&missing.stdout).unwrap();
        let current = list["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["model"] == "current")
            .expect("应列出手写的 local 模型");
        assert_eq!(
            current["availability"],
            json!({"status":"unconfigured", "requirement":"api_key"}),
            "未配置密钥的 bearer provider 应标为 unconfigured"
        );
        assert!(
            list["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning["provider"] == "local"),
            "未过滤凭据的 provider 查询应报告缺失目录告警"
        );
        let filtered = cli(
            &world,
            &spec,
            &["--json", "models", "--auth", "--provider", "local"],
        );
        evidence.record_command("录入密钥前按 --auth 过滤指定 provider", &filtered);
        assert!(filtered.is_clean_exit(0), "--auth 过滤查询失败");
        let filtered: Value = serde_json::from_slice(&filtered.stdout).unwrap();
        assert!(
            filtered["models"].as_array().unwrap().is_empty(),
            "缺少 bearer 凭据的模型不应留在 --auth 结果中"
        );
        assert!(
            filtered["warnings"].as_array().unwrap().is_empty(),
            "被跳过的 provider 告警泄漏到了 --auth --provider 结果中：{filtered}"
        );
        let auth_before_key = cli(&world, &spec, &["--json", "models", "--auth"]);
        evidence.record_command("录入密钥前查询 --auth 模型列表（JSON）", &auth_before_key);
        assert!(auth_before_key.is_clean_exit(0), "--auth 列表查询失败");
        let auth_before_key: Value = serde_json::from_slice(&auth_before_key.stdout).unwrap();
        assert!(
            auth_before_key["models"]
                .as_array()
                .unwrap()
                .iter()
                .all(|model| model["provider"] == "empty"),
            "被跳过的 provider 泄漏到了 --auth 模型列表中：{auth_before_key}"
        );
        assert!(
            auth_before_key["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .all(|warning| warning["provider"] == "empty"),
            "被跳过的 provider 泄漏到了 --auth 告警列表中：{auth_before_key}"
        );
        let auth_text = cli(&world, &spec, &["models", "--auth"]);
        evidence.record_command("录入密钥前查询 --auth 模型列表（文本）", &auth_text);
        assert!(auth_text.is_clean_exit(0), "--auth 文本列表查询失败");
        let auth_stdout = String::from_utf8_lossy(&auth_text.stdout);
        let auth_stderr = String::from_utf8_lossy(&auth_text.stderr);
        assert!(auth_stdout.contains("empty/listed"), "{auth_stdout}");
        assert!(auth_stderr.contains("empty: "), "{auth_stderr}");
        for skipped in ["local", "anthropic", "lmstudio", "ollama"] {
            assert!(
                !auth_stdout.contains(&format!("{skipped}/")),
                "被跳过的 provider {skipped} 出现在标准输出中：{auth_stdout}"
            );
            assert!(
                !auth_stderr.contains(&format!("{skipped}:")),
                "被跳过的 provider {skipped} 出现在标准错误告警中：{auth_stderr}"
            );
        }
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
        evidence.record_command("录入密钥后查询就绪 provider 模型", &ready);
        assert!(ready.is_clean_exit(0), "就绪模型查询失败");
        let ready: Value = serde_json::from_slice(&ready.stdout).unwrap();
        assert!(
            ready["models"]
                .as_array()
                .unwrap()
                .iter()
                .all(|model| model["availability"] == json!({"status":"ready"})),
            "录入密钥后未将 local provider 标记为 ready：{ready}"
        );
        let mixed = cli(&world, &spec, &["--json", "models", "--auth"]);
        evidence.record_command("混合凭据状态下查询 --auth 模型列表", &mixed);
        assert!(mixed.is_clean_exit(0), "混合凭据状态查询失败");
        let mixed: Value = serde_json::from_slice(&mixed.stdout).unwrap();
        let models = mixed["models"].as_array().unwrap();
        assert!(
            models.iter().any(|model| model["provider"] == "empty"),
            "--auth 结果缺少 auth=none provider"
        );
        assert!(
            models.iter().any(|model| model["provider"] == "local"),
            "--auth 结果缺少已就绪的 bearer provider"
        );
        assert!(
            !models.iter().any(|model| model["provider"] == "anthropic"),
            "未就绪的内置 provider 泄漏到了 --auth 结果中"
        );
        assert!(
            mixed["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .all(|warning| warning["provider"] == "empty" || warning["provider"] == "local"),
            "未就绪 provider 的告警泄漏到了 --auth 结果中：{mixed}"
        );
        assert!(
            models
                .iter()
                .all(|model| model["availability"] == json!({"status":"ready"})),
            "--auth 列出了未就绪的模型"
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
    world.start_daemon().expect("显式启动后台");
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
        evidence.record_command("查询模型文本输出与密钥不泄漏", &text);
        assert!(text.is_clean_exit(0), "模型文本列表查询失败");
        let public = format!(
            "{}{}",
            String::from_utf8_lossy(&text.stdout),
            String::from_utf8_lossy(&text.stderr)
        );
        assert!(
            public.contains("local/current"),
            "文本输出缺少目标模型：{public}"
        );
        assert!(public.contains("ready"), "文本输出缺少凭据状态：{public}");
        assert!(
            !public.contains(FAKE_API_KEY),
            "文本输出泄漏了 provider 密钥"
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
    world.start_daemon().expect("显式启动后台");
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
            evidence.record_command("查询目录告警中的已知凭据脱敏", &warning);
            assert!(warning.is_clean_exit(0), "目录告警查询失败");
            let public = format!(
                "{}{}",
                String::from_utf8_lossy(&warning.stdout),
                String::from_utf8_lossy(&warning.stderr)
            );
            assert!(
                !public.contains(CATALOG_KEY),
                "与已知凭据同值的目录标识被原样输出"
            );
            assert!(public.contains("[REDACTED]"), "已知凭据未被脱敏：{public}");
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
        evidence.record_command("短凭据不应误遮蔽更长的目录标识", &short);
        assert!(short.is_clean_exit(0), "短凭据下的目录查询失败");
        let short: Value = serde_json::from_slice(&short.stdout).unwrap();
        let message = short["warnings"][0]["message"].as_str().unwrap();
        assert!(
            message.contains("EMPTY-catalog"),
            "短凭据误遮蔽了更长的目录标识：{message}"
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
    world.start_daemon().expect("显式启动后台");
    let mut evidence =
        lattice_system_harness::ScenarioEvidence::new(&world, &spec.name, Vec::<String>::new())
            .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let started = cli(&world, &spec, &["--json", "models", "--provider", "empty"]);
        evidence.record_command("凭据损坏前的初始模型查询", &started);
        assert!(started.is_clean_exit(0), "初始模型查询失败");
        for (label, unavailable_directory) in
            [("凭据文件格式错误", false), ("凭据目录权限过宽", true)]
        {
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
            evidence.record_command("凭据不可读时查询模型可用性（JSON）", &unavailable);
            assert!(unavailable.is_clean_exit(0), "{label}：models 命令非零退出");
            let unavailable: Value = serde_json::from_slice(&unavailable.stdout).unwrap();
            let warnings = unavailable["warnings"].as_array().unwrap();
            assert!(
                !warnings.iter().any(|warning| warning["provider"].is_null()),
                "{label}：出现了预期之外的全局告警：{warnings:?}"
            );
            let models = unavailable["models"].as_array().unwrap();
            let local = models
                .iter()
                .find(|model| model["provider"] == "local" && model["model"] == "current")
                .expect("已声明的 bearer 模型应出现在列表中");
            assert_eq!(local["availability"]["status"], "invalid", "{local}");
            let reason = local["availability"]["reason"].as_str().unwrap();
            let expected = if unavailable_directory {
                "permissions are too broad"
            } else {
                "auth.json"
            };
            assert!(reason.contains(expected), "{label}：实际原因为 {reason}");
            let empty = models
                .iter()
                .find(|model| model["provider"] == "empty" && model["model"] == "listed")
                .expect("auth=none 模型应出现在列表中");
            assert_eq!(empty["availability"], json!({"status":"ready"}), "{empty}");
            let text = cli(&world, &spec, &["models"]);
            evidence.record_command("凭据不可读时查询模型列表（文本）", &text);
            assert!(
                text.is_clean_exit(0),
                "{label}：文本模式 models 命令非零退出"
            );
            let stderr = String::from_utf8_lossy(&text.stderr);
            assert!(
                !stderr.contains("provider credentials are not ready"),
                "{label}：已移除的全局凭据告警仍被打印：{stderr}"
            );
            assert!(
                stderr
                    .lines()
                    .any(|line| line.starts_with("local/current: ") && line.contains(expected)),
                "{label}：标准错误输出为 {stderr}"
            );
            let ready_only = cli(&world, &spec, &["--json", "models", "--auth"]);
            evidence.record_command("凭据不可读时查询 --auth 就绪模型", &ready_only);
            assert!(ready_only.is_clean_exit(0), "{label}：--auth 查询非零退出");
            let ready_only: Value = serde_json::from_slice(&ready_only.stdout).unwrap();
            assert!(
                !ready_only["models"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|model| model["provider"] == "local"),
                "{label}：凭据无效的 bearer provider 泄漏到了 --auth 结果中：{ready_only}"
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
    world.start_daemon().expect("显式启动后台");
    let mut evidence =
        lattice_system_harness::ScenarioEvidence::new(&world, &spec.name, Vec::<String>::new())
            .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let missing = cli(&world, &spec, &["--json", "models", "--provider", "absent"]);
        evidence.record_command("查询未知 provider 应报错拒绝", &missing);
        assert!(missing.is_clean_exit(2), "未知 provider 不应被接受");
        let missing: Value = serde_json::from_slice(&missing.stdout).unwrap();
        let message = missing["message"].as_str().unwrap();
        for value in [
            "absent",
            "available providers:",
            "anthropic",
            "openai",
            "google",
            "local",
        ] {
            assert!(
                message.contains(value),
                "未知 provider 报错信息缺少 `{value}`：{message}"
            );
        }
    }));
    finish_recorded(evidence, result);
}
