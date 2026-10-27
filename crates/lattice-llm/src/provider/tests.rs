// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::{collections::BTreeSet, fs};

use crate::protocol::{
    Cache, ChatCompletionsFeatures, EffortParam, Features, MessagesFeatures, Protocol,
    ReasoningReplay, ThinkingBudget, ThinkingToggle,
};

use super::{
    config::{MAX_MODELS_PER_PROVIDER, MAX_PROVIDERS, MAX_PROVIDERS_FILE_BYTES, ProviderRegistry},
    model_ref::{MAX_MODEL_ID_BYTES, MAX_PROVIDER_ID_BYTES, validate_provider_id},
    *,
};

const PATH: &str = "/configuration/providers.toml";
const CHAT_PROVIDER: &str =
    "[providers.custom]\nprotocol = 'chat-completions'\nbase_url = 'http://localhost'";
const MESSAGES_PROVIDER: &str =
    "[providers.custom]\nprotocol = 'anthropic-messages'\nbase_url = 'http://localhost'";

fn parse(source: &str) -> ProviderRegistry {
    ProviderRegistry::parse(PATH, source).unwrap()
}

fn chat(features: Features) -> ChatCompletionsFeatures {
    let Features::ChatCompletions(features) = features else {
        panic!("expected chat-completions features")
    };
    features
}

fn assert_invalid(source: &str, line: usize, reason: &str) {
    let error = ProviderRegistry::parse(PATH, source).expect_err(source);
    assert!(
        error.to_string().starts_with(&format!("{PATH}:{line}: ")),
        "{source:?}: {error}"
    );
    assert!(
        error.reason.contains(reason),
        "{source:?}: expected {reason:?}, got {error}"
    );
}

#[test]
fn builtins_obey_provider_invariants() {
    let mut ids = BTreeSet::new();
    for provider in builtins::providers() {
        validate_provider_id(&provider.id).unwrap();
        assert!(
            ids.insert(provider.id.clone()),
            "duplicate id: {}",
            provider.id
        );
        net_guard::validate_base_url_static(&provider.base_url).unwrap();
        match provider.auth {
            Auth::OauthChatgpt => assert_eq!(provider.id, "chatgpt"),
            Auth::None => {
                let url = url::Url::parse(&provider.base_url).unwrap();
                assert!(matches!(
                    url.host_str(),
                    Some("localhost" | "127.0.0.1" | "[::1]")
                ));
            }
            auth => assert_eq!(
                auth,
                Auth::default_for(provider.protocol),
                "{}",
                provider.id
            ),
        }
    }
}

#[test]
fn merges_fields_and_model_features_without_losing_metadata_presence() {
    let builtin = ProviderRegistry::builtins();
    let original = builtin.get("deepseek").unwrap();
    let registry = parse(
        r#"
[providers.deepseek]
base_url = "https://gateway.invalid/deepseek"
[providers.deepseek.features]
thinking_toggle = "enable-thinking"
[providers.deepseek.models."deepseek-v4-flash"]
context = 131072
output = 8192
effort = ["off", "minimal", "low", "medium", "high", "xhigh", "max"]
tools = false
cost = { input = 1, output = 2.5, cache_read = 0.1, cache_write = 0.5 }
[providers.deepseek.models."deepseek-v4-flash".features]
reasoning_replay = "none"
[providers.deepseek.models."no-metadata"]
[providers.my-vllm]
protocol = "chat-completions"
base_url = "http://localhost:8000/v1"
auth = "none"
catalog = "custom-catalog"
"#,
    );
    let provider = registry.get("deepseek").unwrap();
    assert_eq!(provider.base_url, "https://gateway.invalid/deepseek");
    assert_eq!(
        (provider.protocol, provider.auth, &provider.catalog),
        (original.protocol, original.auth, &original.catalog)
    );
    let expected_features = ChatCompletionsFeatures {
        thinking_toggle: ThinkingToggle::EnableThinking,
        ..chat(original.features)
    };
    assert_eq!(chat(provider.features), expected_features);
    let model = &provider.models["deepseek-v4-flash"];
    assert_eq!(
        (model.context, model.output, model.tools),
        (Some(131072), Some(8192), Some(false))
    );
    assert_eq!(
        model.effort.as_deref(),
        Some(
            [
                Effort::Off,
                Effort::Minimal,
                Effort::Low,
                Effort::Medium,
                Effort::High,
                Effort::Xhigh,
                Effort::Max
            ]
            .as_slice()
        )
    );
    assert_eq!(
        model.cost,
        Some(ModelCost {
            input: Some(1.0),
            output: Some(2.5),
            cache_read: Some(0.1),
            cache_write: Some(0.5)
        })
    );
    assert_eq!(
        chat(model.features),
        ChatCompletionsFeatures {
            reasoning_replay: ReasoningReplay::None,
            ..expected_features
        }
    );
    let absent = &provider.models["no-metadata"];
    assert_eq!(
        (
            absent.context,
            absent.output,
            &absent.effort,
            absent.tools,
            &absent.cost
        ),
        (None, None, &None, None, &None)
    );
    assert_eq!(absent.features, provider.features);
    let custom = registry.get("my-vllm").unwrap();
    assert_eq!(
        (custom.auth, custom.catalog.as_deref()),
        (Auth::None, Some("custom-catalog"))
    );
    assert!(registry.get("missing").is_none());
    assert_eq!(registry.get("anthropic"), builtin.get("anthropic"));
}

#[test]
fn builtin_protocol_key_is_rejected_even_when_the_value_is_unchanged() {
    for (id, protocol) in [
        ("deepseek", "chat-completions"),
        ("deepseek", "anthropic-messages"),
        ("chatgpt", "responses"),
    ] {
        assert_invalid(
            &format!("[providers.{id}]\nprotocol = '{protocol}'"),
            2,
            "to use another protocol, create a new provider id",
        );
    }
}

#[test]
fn chatgpt_allows_only_handwritten_model_metadata() {
    for field in [
        "base_url = 'https://api.openai.com/v1'",
        "auth = 'oauth:chatgpt'",
        "features = {}",
        "catalog = 'openai'",
    ] {
        assert_invalid(
            &format!("[providers.chatgpt]\n{field}"),
            2,
            "chatgpt only permits",
        );
    }
    let builtin = ProviderRegistry::builtins();
    let registry = parse("[providers.chatgpt.models.example]\ncontext = 65536");
    let provider = registry.get("chatgpt").unwrap();
    assert_eq!(provider.auth, Auth::OauthChatgpt);
    assert_eq!(
        (&provider.base_url, provider.protocol, provider.features),
        (
            &builtin.get("chatgpt").unwrap().base_url,
            Protocol::Responses,
            Features::Responses
        )
    );
    assert_eq!(provider.models["example"].context, Some(65536));
    assert_invalid(
        "[providers.openai]\nauth = 'oauth:chatgpt'",
        2,
        "reserved for chatgpt",
    );
}

#[test]
fn strict_errors_report_path_line_and_reason() {
    for (source, line, reason) in [
        ("\nunknown = true", 2, "unknown field"),
        ("providers = []", 1, "invalid type"),
        ("[providers]\nopenai = []", 2, "table"),
        (
            "[providers]\ncustom = ['responses', 'http://localhost']",
            2,
            "table",
        ),
        ("[providers.openai.models]\nexample = []", 2, "table"),
        (
            "[providers.openai.models.example]\ncost = [1.0, 2.0]",
            2,
            "table",
        ),
        (
            "[providers.openai]\nkey = 'not-a-credential-field'",
            2,
            "unknown field",
        ),
        ("[providers.openai]\nbase_url = 10", 2, "invalid type"),
        (
            "[providers.custom]\nprotocol = 'unknown'\nbase_url = 'http://localhost'",
            2,
            "unknown variant",
        ),
        ("[providers.openai]\nauth = 'unknown'", 2, "unknown variant"),
        (
            "[providers.custom]\nprotocol = {responses = {}}\nbase_url = 'http://localhost'",
            2,
            "invalid type",
        ),
        (
            "[providers.openai]\nauth = {bearer = []}",
            2,
            "invalid type",
        ),
        ("[providers.openai]\ncatalog = false", 2, "invalid type"),
        ("[providers.openai]\nfeatures = []", 2, "invalid type"),
        ("[providers.openai]\nmodels = []", 2, "invalid type"),
        (
            "[providers.openai.models.test]\nunknown = true",
            2,
            "unknown field",
        ),
        (
            "[providers.openai.models.test]\ncontext = -1",
            2,
            "invalid value",
        ),
        (
            "[providers.openai.models.test]\noutput = 1.5",
            2,
            "invalid type",
        ),
        (
            "[providers.openai.models.test]\neffort = ['default']",
            2,
            "unknown variant",
        ),
        (
            "[providers.openai.models.test]\neffort = 'high'",
            2,
            "invalid type",
        ),
        (
            "[providers.openai.models.test]\neffort = [{high = {}}]",
            2,
            "invalid type",
        ),
        (
            "[providers.openai.models.test]\neffort = [\n'high',\n'unsupported'\n]",
            4,
            "unknown variant",
        ),
        (
            "[providers.openai.models.test]\ntools = 'true'",
            2,
            "invalid type",
        ),
        (
            "[providers.openai.models.test]\ncost = 1",
            2,
            "invalid type",
        ),
        (
            "[providers.openai.models.test.cost]\nreasoning = 1",
            2,
            "unknown field",
        ),
        (
            "[providers.openai.models.test.cost]\ninput = 'free'",
            2,
            "invalid type",
        ),
        (
            "[providers.'bad_id']\nprotocol = 'responses'\nbase_url = 'http://localhost'",
            1,
            "provider id",
        ),
        (
            "[providers.custom]\nbase_url = 'http://localhost'",
            1,
            "requires protocol",
        ),
        (
            "[providers.custom]\nprotocol = 'responses'",
            1,
            "requires base_url",
        ),
        ("[providers.openai.models.'']\ncontext = 1", 1, "model id"),
        ("[providers.openai]\nbase_url = [", 2, "array"),
        ("[providers.openai]\n[providers.openai]", 2, "duplicate key"),
    ] {
        assert_invalid(source, line, reason);
    }
    // 有效配置之后出现错误也必须拒绝整份文件，不能返回部分注册表。
    assert_invalid(
        "[providers.deepseek]\nbase_url = 'http://localhost'\n[providers.openai]\nunknown = true",
        4,
        "unknown field",
    );
}

#[test]
fn every_protocol_parses_and_uses_its_default_auth_and_features() {
    for (protocol, expected, auth) in [
        ("chat-completions", Protocol::ChatCompletions, Auth::Bearer),
        ("responses", Protocol::Responses, Auth::Bearer),
        ("anthropic-messages", Protocol::Messages, Auth::XApiKey),
        (
            "generate-content",
            Protocol::GenerateContent,
            Auth::XGoogApiKey,
        ),
        ("interactions", Protocol::Interactions, Auth::XGoogApiKey),
    ] {
        let registry = parse(&format!(
            "[providers.custom]\nprotocol = '{protocol}'\nbase_url = 'http://localhost'"
        ));
        let provider = registry.get("custom").unwrap();
        assert_eq!(
            (provider.protocol, provider.auth, provider.features),
            (expected, auth, Features::defaults(expected))
        );
        assert_eq!(provider.protocol.as_str(), protocol);
    }
    #[derive(serde::Deserialize)]
    struct Authentication {
        auth: Auth,
    }
    for (value, auth) in [
        ("bearer", Auth::Bearer),
        ("x-api-key", Auth::XApiKey),
        ("x-goog-api-key", Auth::XGoogApiKey),
        ("oauth:chatgpt", Auth::OauthChatgpt),
        ("none", Auth::None),
    ] {
        let parsed: Authentication = toml::from_str(&format!("auth = '{value}'")).unwrap();
        assert_eq!(parsed.auth, auth);
    }
}

#[test]
fn every_feature_value_parses_to_the_closed_type() {
    let parse_feature = |assignment: &str| {
        let registry = parse(&format!(
            "{CHAT_PROVIDER}\n[providers.custom.features]\n{assignment}"
        ));
        chat(registry.get("custom").unwrap().features)
    };
    for (value, expected) in [
        ("reasoning-effort", EffortParam::ReasoningEffort),
        ("reasoning-object", EffortParam::ReasoningObject),
    ] {
        assert_eq!(
            parse_feature(&format!("effort_param = '{value}'")).effort_param,
            expected
        );
    }
    for (value, expected) in [
        ("none", ThinkingToggle::None),
        ("thinking-type", ThinkingToggle::ThinkingType),
        ("enable-thinking", ThinkingToggle::EnableThinking),
    ] {
        assert_eq!(
            parse_feature(&format!("thinking_toggle = '{value}'")).thinking_toggle,
            expected
        );
    }
    for (value, expected) in [
        ("none", ThinkingBudget::None),
        ("thinking-budget", ThinkingBudget::ThinkingBudget),
        ("reasoning-max-tokens", ThinkingBudget::ReasoningMaxTokens),
    ] {
        assert_eq!(
            parse_feature(&format!("thinking_budget = '{value}'")).thinking_budget,
            expected
        );
    }
    for (value, expected) in [
        ("none", ReasoningReplay::None),
        ("reasoning-content", ReasoningReplay::ReasoningContent),
        ("content-chunks", ReasoningReplay::ContentChunks),
    ] {
        assert_eq!(
            parse_feature(&format!("reasoning_replay = '{value}'")).reasoning_replay,
            expected
        );
    }
    for enabled in [false, true] {
        assert_eq!(
            parse_feature(&format!("cache_markers = {enabled}")).cache_markers,
            enabled
        );
    }
    for (value, expected) in [("automatic", Cache::Automatic), ("none", Cache::None)] {
        let registry = parse(&format!(
            "{MESSAGES_PROVIDER}\n[providers.custom.features]\ncache = '{value}'"
        ));
        let Features::Messages(features) = registry.get("custom").unwrap().features else {
            panic!("expected messages features");
        };
        assert_eq!(features.cache, expected);
    }
}

#[test]
fn feature_defaults_match_standard_protocol_behavior() {
    assert_eq!(
        Features::defaults(Protocol::ChatCompletions),
        Features::ChatCompletions(ChatCompletionsFeatures {
            effort_param: EffortParam::ReasoningEffort,
            thinking_toggle: ThinkingToggle::None,
            thinking_budget: ThinkingBudget::None,
            reasoning_replay: ReasoningReplay::None,
            cache_markers: false,
        })
    );
    assert_eq!(
        Features::defaults(Protocol::Messages),
        Features::Messages(MessagesFeatures {
            cache: Cache::Automatic
        })
    );
}

#[test]
fn rejects_unknown_feature_values_types_and_features_from_other_protocols() {
    for name in [
        "effort_param",
        "thinking_toggle",
        "thinking_budget",
        "reasoning_replay",
    ] {
        assert_invalid(
            &format!("{CHAT_PROVIDER}\n[providers.custom.features]\n{name} = 'unsupported'"),
            5,
            "unknown variant",
        );
        assert_invalid(
            &format!("{MESSAGES_PROVIDER}\n[providers.custom.features]\n{name} = 'none'"),
            5,
            "unknown feature",
        );
    }
    for name in ["unknown", "cache"] {
        assert_invalid(
            &format!("{CHAT_PROVIDER}\n[providers.custom.features]\n{name} = 'none'"),
            5,
            "unknown feature",
        );
    }
    assert_invalid(
        &format!("{MESSAGES_PROVIDER}\n[providers.custom.features]\ncache = 'unsupported'"),
        5,
        "unknown variant",
    );
    for assignment in [
        "cache_markers = 'true'",
        "reasoning_replay = true",
        "effort_param = 1",
        "thinking_budget = []",
        "thinking_toggle = {}",
        "thinking_toggle = {none = {}}",
        "reasoning_replay = {none = []}",
    ] {
        assert_invalid(
            &format!("{CHAT_PROVIDER}\n[providers.custom.features]\n{assignment}"),
            5,
            "invalid type",
        );
    }
    for protocol in ["responses", "generate-content", "interactions"] {
        for name in ["effort_param", "cache"] {
            assert_invalid(
                &format!(
                    "[providers.custom]\nprotocol = '{protocol}'\nbase_url = 'http://localhost'\n[providers.custom.features]\n{name} = 'none'"
                ),
                5,
                "unknown feature",
            );
        }
    }
    assert_invalid(
        &format!(
            "{CHAT_PROVIDER}\n[providers.custom.models.test.features]\nreasoning_replay = 'unsupported'"
        ),
        5,
        "unknown variant",
    );
}

#[test]
fn registry_checks_base_url_and_reports_its_line() {
    for url in [
        "http://localhost/v1",
        "http://127.0.0.1:8000/v1",
        "http://[::1]:8000/v1",
        "https://provider.invalid/v1",
    ] {
        let registry = parse(&format!("[providers.openai]\nbase_url = '{url}'"));
        assert_eq!(registry.get("openai").unwrap().base_url, url);
    }
    assert_invalid(
        "[providers.openai]\nbase_url = 'http://remote.invalid'",
        2,
        "HTTPS",
    );
}

#[test]
fn file_byte_limit_is_inclusive_for_bounded_file_loading() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("providers.toml");
    let mut source = format!("#{}", "é".repeat((MAX_PROVIDERS_FILE_BYTES - 2) / 2));
    source.push('a');
    assert_eq!(source.len(), MAX_PROVIDERS_FILE_BYTES);
    fs::write(&path, &source).unwrap();
    ProviderRegistry::load(&path).unwrap();
    source.push('a');
    fs::write(&path, &source).unwrap();
    let error = ProviderRegistry::load(&path).unwrap_err();
    assert_eq!((error.path.as_path(), error.line), (path.as_path(), 1));
    assert!(error.reason.contains("1 MiB"));
}

#[test]
fn provider_count_counts_only_file_tables_including_builtin_overrides() {
    let mut source = String::from("[providers.deepseek]\n");
    for index in 1..MAX_PROVIDERS {
        source.push_str(&format!(
            "[providers.custom-{index}]\nprotocol = 'responses'\nbase_url = 'http://localhost'\n"
        ));
    }
    let registry = parse(&source);
    assert_eq!(
        registry.iter().count(),
        ProviderRegistry::builtins().iter().count() + MAX_PROVIDERS - 1
    );
    source.push_str("[providers.extra]\nprotocol = 'responses'\nbase_url = 'http://localhost'");
    assert_invalid(&source, 2 + 3 * (MAX_PROVIDERS - 1), "256 providers");
}

#[test]
fn handwritten_model_count_limit_is_inclusive() {
    let mut source = String::new();
    for index in 0..MAX_MODELS_PER_PROVIDER {
        source.push_str(&format!("[providers.ollama.models.model-{index}]\n"));
    }
    assert_eq!(
        parse(&source).get("ollama").unwrap().models.len(),
        MAX_MODELS_PER_PROVIDER
    );
    source.push_str("[providers.ollama.models.extra]\n");
    assert_invalid(
        &source,
        MAX_MODELS_PER_PROVIDER + 1,
        "1024 handwritten models",
    );
}

#[test]
fn file_loading_applies_overrides_and_reports_invalid_input() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("providers.toml");
    assert_eq!(
        ProviderRegistry::load(&path).unwrap(),
        ProviderRegistry::builtins()
    );
    fs::write(
        &path,
        "[providers.openai]\nbase_url = 'https://gateway.invalid/v1'",
    )
    .unwrap();
    let registry = ProviderRegistry::load(&path).unwrap();
    assert_eq!(
        registry.get("openai").unwrap().base_url,
        "https://gateway.invalid/v1"
    );
    assert_eq!(
        ProviderRegistry::load(directory.path()).unwrap_err().path,
        directory.path()
    );
    fs::write(&path, b"# comment\n\xff").unwrap();
    let error = ProviderRegistry::load(&path).unwrap_err();
    assert_eq!(
        (error.path.as_path(), error.line, error.reason.as_str()),
        (path.as_path(), 2, "providers.toml must be UTF-8")
    );
}

#[test]
fn model_reference_splits_once_and_round_trips() {
    let source = "openrouter/anthropic/claude-sonnet-4.6";
    let model: ModelRef = source.parse().unwrap();
    assert_eq!(
        (model.provider(), model.model()),
        ("openrouter", "anthropic/claude-sonnet-4.6")
    );
    assert_eq!(model.to_string(), source);
}

#[test]
fn provider_ids_enforce_alphabet_and_inclusive_length_limit() {
    for id in [
        "a".to_owned(),
        "0".to_owned(),
        "a-b-9-".to_owned(),
        "a".repeat(MAX_PROVIDER_ID_BYTES),
    ] {
        validate_provider_id(&id).unwrap();
        ModelRef::parse(&format!("{id}/model")).unwrap();
    }
    for id in [
        "".to_owned(),
        "-a".to_owned(),
        "A".to_owned(),
        "a_b".to_owned(),
        "a.b".to_owned(),
        "a b".to_owned(),
        "é".to_owned(),
        "a/".to_owned(),
        "a".repeat(MAX_PROVIDER_ID_BYTES + 1),
    ] {
        assert!(validate_provider_id(&id).is_err(), "{id:?}");
    }
}

#[test]
fn model_reference_rejects_missing_parts_invalid_provider_and_control_characters() {
    for source in [
        "",
        "openai",
        "/model",
        "openai/",
        "bad_id/model",
        "openai/m\0",
        "openai/m\u{7f}",
        "openai/m\u{85}",
    ] {
        assert!(ModelRef::parse(source).is_err(), "{source:?}");
    }
    // 空格、Unicode 与后续斜杠都是不透明模型名的一部分，不属于控制字符。
    assert_eq!(
        ModelRef::parse("openai/ 模型 /suffix").unwrap().model(),
        " 模型 /suffix"
    );
}

#[test]
fn model_reference_model_byte_limit_is_independent_of_provider_length() {
    let provider = "a".repeat(MAX_PROVIDER_ID_BYTES);
    let model = "é".repeat(MAX_MODEL_ID_BYTES / 2);
    let reference = format!("{provider}/{model}");
    assert_eq!(reference.len(), MAX_PROVIDER_ID_BYTES + 1 + 4_096);
    ModelRef::parse(&reference).unwrap();
    assert!(ModelRef::parse(&format!("{reference}a")).is_err());
}
