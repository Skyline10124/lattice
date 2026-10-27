// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeSet;

use serde_json::json;

use super::*;

fn cache(dir: &Path, source: &serde_json::Value) {
    std::fs::create_dir_all(dir.join("cache")).unwrap();
    std::fs::write(
        dir.join("cache/models.json"),
        serde_json::to_vec(source).unwrap(),
    )
    .unwrap();
}

#[test]
fn conversion_keeps_only_the_allowlist_and_preserves_raw_reasoning() {
    let source = json!({
        "openai": {
            "id": "openai", "name": "ignored", "env": ["OTHER_KEY"],
            "api": "https://untrusted.invalid", "protocol": "ignored",
            "models": {"m": {
                "id": "m", "name": "Model", "limit": {"context": 123, "output": 45, "input": 100},
                "reasoning": true,
                "reasoning_options": [{"type": "future", "values": ["default", "none"], "custom": {"z": 1, "a": 2}}],
                "tool_call": false, "modalities": {"input": ["text", "image"], "output": ["text"]},
                "cost": {"input": 1, "output": 2, "cache_read": 0.1, "cache_write": 3, "reasoning": 4,
                         "tiers": [], "context_over_200k": {"input": 10}},
                "status": "deprecated", "endpoint": "https://untrusted.invalid",
                "env": ["OTHER_KEY"], "protocol": "ignored", "effort": ["max"],
                "features": {"reasoning_replay": "reasoning-content"}, "attachment": true
            }}
        },
        "not-built-in": false
    });
    let normalized = snapshot_from_json(&serde_json::to_vec(&source).unwrap()).unwrap();
    let actual: serde_json::Value = serde_json::from_slice(&normalized).unwrap();
    assert_eq!(
        actual,
        json!({"openai": {"models": {"m": {
            "id": "m", "name": "Model", "limit": {"context": 123, "output": 45},
            "reasoning": true, "reasoning_options": source["openai"]["models"]["m"]["reasoning_options"],
            "tool_call": false, "modalities": {"input": ["text", "image"]},
            "cost": {"input": 1.0, "output": 2.0, "cache_read": 0.1, "cache_write": 3.0, "reasoning": 4.0},
            "status": "deprecated"
        }}}})
    );
    assert_eq!(snapshot_from_json(&normalized).unwrap(), normalized);
    assert_eq!(normalized.last(), Some(&b'\n'));
}

#[test]
fn conversion_is_stable_across_input_order_and_rejects_invalid_selected_entries() {
    let a = br#"{"openai":{"models":{"z":{"id":"z"},"a":{"id":"a","reasoning_options":[{"z":1,"a":2}]}}},"google":{"models":{}}}"#;
    let b = br#"{"google":{"models":{}},"openai":{"models":{"a":{"reasoning_options":[{"a":2,"z":1}],"id":"a"},"z":{"id":"z"}}}}"#;
    assert_eq!(
        snapshot_from_json(a).unwrap(),
        snapshot_from_json(b).unwrap()
    );
    assert!(
        snapshot_from_json(br#"{"openai":{"models":{"bad":{"id":"bad","tool_call":"yes"}}}}"#)
            .is_err()
    );
}

#[test]
fn missing_unreadable_or_invalid_cache_uses_the_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let registry = ProviderRegistry::builtins();
    let provider = registry.get("openai").unwrap();
    let baseline = Metadata::load(dir.path()).merge(provider);
    assert!(!baseline.is_empty());
    std::fs::create_dir_all(dir.path().join("cache/models.json")).unwrap();
    assert_eq!(Metadata::load(dir.path()).merge(provider), baseline);
    std::fs::remove_dir(dir.path().join("cache/models.json")).unwrap();
    for source in [
        "not json",
        r#"{"openai":{"models":{"m":{"id":"m","limit":{"context":"invalid"}}}}}"#,
    ] {
        std::fs::write(dir.path().join("cache/models.json"), source).unwrap();
        assert_eq!(Metadata::load(dir.path()).merge(provider), baseline);
    }
}

#[test]
fn positional_records_are_invalid_and_the_cache_falls_back_to_the_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let registry = ProviderRegistry::builtins();
    let provider = registry.get("openai").unwrap();
    let baseline = Metadata::load(dir.path()).merge(provider);
    for (record, source) in [
        ("catalog", json!({"openai": [{}]})),
        ("model", json!({"openai": {"models": {"m": ["m"]}}})),
        (
            "limit",
            json!({"openai": {"models": {"m": {"id": "m", "limit": [123, 45]}}}}),
        ),
        (
            "cost",
            json!({"openai": {"models": {"m": {"id": "m", "cost": [1, 2, 3, 4, 5]}}}}),
        ),
        (
            "modalities",
            json!({"openai": {"models": {"m": {"id": "m", "modalities": [["text"]]}}}}),
        ),
    ] {
        let bytes = serde_json::to_vec(&source).unwrap();
        assert!(
            snapshot_from_json(&bytes).is_err(),
            "{record} must be an object"
        );
        cache(dir.path(), &source);
        assert_eq!(
            Metadata::load(dir.path()).merge(provider),
            baseline,
            "{record}"
        );
    }
}

#[test]
fn cache_replaces_whole_entries_and_missing_catalogs_fall_back() {
    let dir = tempfile::tempdir().unwrap();
    let registry = ProviderRegistry::builtins();
    let openai = registry.get("openai").unwrap();
    let anthropic = registry.get("anthropic").unwrap();
    let snapshot = Metadata::load(dir.path());
    let snapshot_models = snapshot.merge(openai);
    let id = snapshot_models.keys().next().unwrap();
    cache(dir.path(), &json!({"openai": {"models": {id: {"id": id}}}}));
    let metadata = Metadata::load(dir.path());
    let models = metadata.merge(openai);
    assert_eq!(models.len(), 1);
    assert_eq!(models[id].limit, None);
    assert_eq!(models[id].cost, None);
    assert_eq!(models[id].tool_call, Some(true));
    assert_eq!(metadata.merge(anthropic), snapshot.merge(anthropic));
    cache(dir.path(), &json!({"openai": {"models": {}}}));
    assert!(Metadata::load(dir.path()).merge(openai).is_empty());
}

#[test]
fn cached_custom_catalogs_are_loaded_without_a_registry_filter() {
    let dir = tempfile::tempdir().unwrap();
    cache(
        dir.path(),
        &json!({"togetherai": {"models": {
            "org/model": {"id": "org/model", "limit": {"context": 32768},
                          "endpoint": "https://untrusted.invalid", "env": ["OTHER_KEY"]}
        }}}),
    );
    let metadata = Metadata::load(dir.path());
    let registry = ProviderRegistry::parse(
        "providers.toml",
        r#"
        [providers.custom]
        protocol = "chat-completions"
        base_url = "https://example.com/v1"
        catalog = "togetherai"
        [providers.missing]
        protocol = "chat-completions"
        base_url = "https://example.com/v1"
        catalog = "does-not-exist"
    "#,
    )
    .unwrap();
    let models = metadata.merge(registry.get("custom").unwrap());
    assert_eq!(models.len(), 1);
    assert_eq!(
        models["org/model"].limit.as_ref().unwrap().context,
        Some(32768)
    );
    assert!(metadata.merge(registry.get("missing").unwrap()).is_empty());
}

#[test]
fn an_invalid_custom_catalog_invalidates_the_entire_cache() {
    let dir = tempfile::tempdir().unwrap();
    let registry = ProviderRegistry::builtins();
    let provider = registry.get("openai").unwrap();
    let baseline = Metadata::load(dir.path()).merge(provider);
    cache(
        dir.path(),
        &json!({
            "openai": {"models": {"cached": {"id": "cached"}}},
            "togetherai": [{}]
        }),
    );
    assert_eq!(Metadata::load(dir.path()).merge(provider), baseline);
}

#[test]
fn handwritten_values_override_individual_fields_and_can_define_models_alone() {
    let dir = tempfile::tempdir().unwrap();
    cache(
        dir.path(),
        &json!({"openai": {"models": {
            "m": {"id": "m", "name": "Remote name", "limit": {"context": 100, "output": 50},
                  "reasoning": true, "reasoning_options": [{"type": "toggle"}], "tool_call": true,
                  "modalities": {"input": ["text"]}, "status": "deprecated",
                  "cost": {"input": 1, "output": 2, "cache_read": 3, "cache_write": 4, "reasoning": 5}},
            "n": {"id": "n", "tool_call": false, "limit": {"context": 200, "output": 60},
                  "cost": {"input": 1, "output": 2, "cache_read": 3, "cache_write": 4}}
        }}}),
    );
    let registry = ProviderRegistry::parse(
        "providers.toml",
        r#"
        [providers.openai.models.m]
        context = 999
        effort = ["off", "high"]
        tools = false
        cost = { output = 0.0, cache_write = 7.0 }
        [providers.openai.models.n]
        output = 90
        tools = true
        cost = { input = 8.0, cache_read = 9.0 }
        [providers.openai.models.only]
        output = 12
        [providers.openai.models.empty]
    "#,
    )
    .unwrap();
    let models = Metadata::load(dir.path()).merge(registry.get("openai").unwrap());
    let m = &models["m"];
    assert_eq!(
        m.limit,
        Some(Limits {
            context: Some(999),
            output: Some(50)
        })
    );
    assert_eq!(
        m.cost,
        Some(Cost {
            input: Some(1.0),
            output: Some(0.0),
            cache_read: Some(3.0),
            cache_write: Some(7.0),
            reasoning: Some(5.0)
        })
    );
    assert_eq!(m.effort, Some(vec![Effort::Off, Effort::High]));
    assert_eq!(m.tool_call, Some(false));
    assert_eq!(m.name.as_deref(), Some("Remote name"));
    assert_eq!(m.status.as_deref(), Some("deprecated"));
    assert_eq!(m.reasoning, Some(true));
    assert_eq!(m.reasoning_options, Some(json!([{"type": "toggle"}])));
    assert_eq!(
        m.modalities.as_ref().unwrap().input,
        Some(vec!["text".into()])
    );
    let n = &models["n"];
    assert_eq!(
        n.limit,
        Some(Limits {
            context: Some(200),
            output: Some(90)
        })
    );
    assert_eq!(
        n.cost,
        Some(Cost {
            input: Some(8.0),
            output: Some(2.0),
            cache_read: Some(9.0),
            cache_write: Some(4.0),
            reasoning: None
        })
    );
    assert_eq!(n.tool_call, Some(true));
    assert_eq!(n.effort, None);
    let only = &models["only"];
    assert_eq!(only.id, "only");
    assert_eq!(
        only.limit,
        Some(Limits {
            context: None,
            output: Some(12)
        })
    );
    assert_eq!(only.tool_call, Some(true));
    assert_eq!(only.cost, None);
    assert_eq!(models["empty"].limit, None);
}

#[test]
fn providers_share_by_catalog_and_providers_without_catalog_use_only_handwritten_values() {
    let dir = tempfile::tempdir().unwrap();
    cache(
        dir.path(),
        &json!({"openai": {"models": {"m": {"id": "m", "tool_call": false}}}}),
    );
    let registry = ProviderRegistry::parse(
        "providers.toml",
        r#"
        [providers.custom]
        protocol = "chat-completions"
        base_url = "https://example.com/v1"
        catalog = "openai"
        [providers.ollama.models.only]
        context = 456
    "#,
    )
    .unwrap();
    let metadata = Metadata::load(dir.path());
    let expected = metadata.merge(registry.get("openai").unwrap());
    assert_eq!(metadata.merge(registry.get("custom").unwrap()), expected);
    assert_eq!(metadata.merge(registry.get("chatgpt").unwrap()), expected);
    let local = metadata.merge(registry.get("ollama").unwrap());
    assert_eq!(local.len(), 1);
    assert_eq!(local["only"].limit.as_ref().unwrap().context, Some(456));
    assert_eq!(local["only"].tool_call, Some(true));
}

#[test]
fn embedded_snapshot_matches_builtin_catalogs_and_model_identity() {
    let registry = ProviderRegistry::builtins();
    let expected: BTreeSet<_> = registry
        .iter()
        .filter_map(|p| p.catalog.as_deref())
        .collect();
    let raw: BTreeMap<String, serde_json::Value> = serde_json::from_slice(SNAPSHOT).unwrap();
    assert_eq!(
        raw.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        expected
    );
    assert_eq!(snapshot_from_json(SNAPSHOT).unwrap(), SNAPSHOT);
    for (catalog, entry) in &BUILTIN_SNAPSHOT.0 {
        assert!(!entry.models.is_empty(), "{catalog}");
        for (id, model) in &entry.models {
            assert_eq!(*id, model.id, "{catalog}");
            assert!(!id.is_empty(), "{catalog}");
        }
    }
}
