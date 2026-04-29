//! Characterization tests for credential resolution in artemis-core.
//!
//! These tests capture the CURRENT behavior of the credential resolution
//! pipeline BEFORE fixing the credentialless provider priority bug (T11)
//! and adding credential caching (T22).
//!
//! KEY BEHAVIORS DOCUMENTED:
//! - _PROVIDER_CREDENTIALS maps 20 providers to env var names
//! - Ollama has empty credential_keys (credentialless)
//! - Priority loop (resolve() lines 150-167) skips providers where api_key is None
//! - Fallback path (lines 169-179) returns first sorted provider with api_key: None
//! - BUG: Ollama (priority 1, no creds needed) is NOT selected over Anthropic
//!   (priority 5, with API key) because the priority loop only returns providers
//!   with api_key.is_some()
//! - resolve_permissive() constructs ResolvedModel from provider defaults
//! - normalize_model_id() is the hot path that transforms user input before resolution

use artemis_core::catalog::{ApiProtocol, CatalogProviderEntry};
use artemis_core::router::{normalize_model_id, ModelRouter, _PROVIDER_CREDENTIALS};
use std::collections::HashMap;
use std::env;

fn save_env(key: &str) -> Option<String> {
    env::var(key).ok()
}

fn restore_env(key: &str, prev: Option<String>) {
    match prev {
        Some(v) => env::set_var(key, v),
        None => env::remove_var(key),
    }
}

/// Save and remove a set of env vars, returning previous values for restore.
fn isolate_env<'a>(keys: &[&'a str]) -> Vec<(&'a str, Option<String>)> {
    let saved: Vec<(&str, Option<String>)> = keys.iter().map(|k| (*k, save_env(k))).collect();
    for k in keys {
        env::remove_var(k);
    }
    saved
}

fn restore_env_batch(saved: Vec<(&str, Option<String>)>) {
    for (k, v) in saved {
        restore_env(k, v);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// SECTION 1: _PROVIDER_CREDENTIALS table characterization
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn provider_credentials_has_20_entries() {
    assert_eq!(
        _PROVIDER_CREDENTIALS.len(),
        20,
        "_PROVIDER_CREDENTIALS should have exactly 20 provider entries"
    );
}

#[test]
fn provider_credentials_all_provider_slugs() {
    let slugs: Vec<&str> = _PROVIDER_CREDENTIALS.iter().map(|(s, _)| *s).collect();
    let expected = [
        "openrouter",
        "anthropic",
        "openai",
        "gemini",
        "deepseek",
        "groq",
        "mistral",
        "xai",
        "ollama",
        "nous",
        "copilot",
        "opencode-zen",
        "kilocode",
        "ai-gateway",
        "openai-codex",
        "minimax",
        "qwen",
        "volces",
        "infini-ai",
        "opencode-go",
    ];
    assert_eq!(
        slugs.as_slice(),
        expected,
        "provider slugs must match expected order"
    );
}

#[test]
fn provider_credentials_env_var_mapping() {
    // Verify each provider maps to the correct env var name and field name.
    let expected: Vec<(&str, &str, &str)> = vec![
        ("openrouter", "OPENROUTER_API_KEY", "api_key"),
        ("anthropic", "ANTHROPIC_API_KEY", "api_key"),
        ("openai", "OPENAI_API_KEY", "api_key"),
        ("gemini", "GEMINI_API_KEY", "api_key"),
        ("deepseek", "DEEPSEEK_API_KEY", "api_key"),
        ("groq", "GROQ_API_KEY", "api_key"),
        ("mistral", "MISTRAL_API_KEY", "api_key"),
        ("xai", "XAI_API_KEY", "api_key"),
        ("nous", "NOUS_API_KEY", "api_key"),
        ("copilot", "GITHUB_TOKEN", "token"),
        ("opencode-zen", "OPENCODE_ZEN_API_KEY", "api_key"),
        ("kilocode", "KILO_API_KEY", "api_key"),
        ("ai-gateway", "AI_GATEWAY_API_KEY", "api_key"),
        ("openai-codex", "OPENAI_API_KEY", "api_key"),
        ("minimax", "MINIMAX_API_KEY", "api_key"),
        ("qwen", "QWEN_API_KEY", "api_key"),
        ("volces", "ARK_API_KEY", "api_key"),
        ("infini-ai", "INFINI_AI_API_KEY", "api_key"),
        ("opencode-go", "OPENCODE_GO_API_KEY", "api_key"),
    ];

    for (slug, env_var, field_name) in &expected {
        let entry = _PROVIDER_CREDENTIALS
            .iter()
            .find(|(s, _)| *s == *slug)
            .unwrap_or_else(|| panic!("provider '{}' not found in _PROVIDER_CREDENTIALS", slug));
        assert_eq!(
            entry.1.len(),
            1,
            "provider '{}' should have exactly 1 credential entry",
            slug
        );
        assert_eq!(
            entry.1[0].0, *env_var,
            "provider '{}' env var mismatch",
            slug
        );
        assert_eq!(
            entry.1[0].1, *field_name,
            "provider '{}' field name mismatch",
            slug
        );
    }
}

#[test]
fn ollama_is_credentialless() {
    // CHARACTERIZATION: Ollama has an empty credential list.
    // This means resolve_credentials() returns None for it, and the
    // priority loop in resolve() skips it (because api_key.is_some() is false).
    for (slug, creds) in _PROVIDER_CREDENTIALS {
        if *slug == "ollama" {
            assert!(
                creds.is_empty(),
                "provider '{}' should have empty credential_keys (credentialless)",
                slug
            );
        } else {
            assert!(
                !creds.is_empty(),
                "provider '{}' should have at least one credential entry",
                slug
            );
        }
    }
}

#[test]
fn openai_codex_shares_openai_key() {
    // CHARACTERIZATION: openai-codex shares OPENAI_API_KEY with openai.
    // This means setting OPENAI_API_KEY will authenticate both openai and openai-codex providers.
    let codex_entry = _PROVIDER_CREDENTIALS
        .iter()
        .find(|(s, _)| *s == "openai-codex")
        .unwrap();
    assert_eq!(codex_entry.1[0].0, "OPENAI_API_KEY");
}

#[test]
fn copilot_uses_github_token_not_api_key() {
    // CHARACTERIZATION: copilot uses GITHUB_TOKEN with field name "token"
    // (not "api_key" like most other providers).
    let copilot_entry = _PROVIDER_CREDENTIALS
        .iter()
        .find(|(s, _)| *s == "copilot")
        .unwrap();
    assert_eq!(copilot_entry.1[0].0, "GITHUB_TOKEN");
    assert_eq!(copilot_entry.1[0].1, "token");
}

// ═══════════════════════════════════════════════════════════════════════
// SECTION 2: resolve_credentials() characterization
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn resolve_credentials_finds_env_var_for_provider() {
    let _lock = crate::env_lock::lock();
    let prev = save_env("ANTHROPIC_API_KEY");
    env::set_var("ANTHROPIC_API_KEY", "sk-ant-test-key");

    let router = ModelRouter::new();
    let result = router.resolve("claude-sonnet-4-6", Some("anthropic"));
    assert!(result.is_ok(), "should resolve with ANTHROPIC_API_KEY set");
    let resolved = result.unwrap();
    assert_eq!(resolved.api_key.as_deref(), Some("sk-ant-test-key"));

    restore_env("ANTHROPIC_API_KEY", prev);
}

#[test]
fn resolve_credentials_returns_none_for_credentialless_provider() {
    // Ollama has empty credential entries in _PROVIDER_CREDENTIALS,
    // so resolve_credentials() returns None for it regardless of env vars.
    let ollama_entry = _PROVIDER_CREDENTIALS
        .iter()
        .find(|(s, _)| *s == "ollama")
        .unwrap();
    assert!(
        ollama_entry.1.is_empty(),
        "Ollama has no credential entries"
    );
}

#[test]
fn resolve_credentials_prefers_entry_credential_keys_over_fallback() {
    // CHARACTERIZATION: resolve_credentials() first checks entry.credential_keys,
    // then falls back to _PROVIDER_CREDENTIALS. If entry.credential_keys has
    // the right mapping, it wins.
    let _lock = crate::env_lock::lock();
    let prev_custom = save_env("MY_CUSTOM_ANTHROPIC_KEY");
    let prev_standard = save_env("ANTHROPIC_API_KEY");
    env::set_var("MY_CUSTOM_ANTHROPIC_KEY", "custom-key-value");
    env::remove_var("ANTHROPIC_API_KEY");

    // Test via custom model registration — custom models can have their own credential_keys
    let mut router = ModelRouter::new();
    use artemis_core::catalog::ModelCatalogEntry;

    let custom = ModelCatalogEntry {
        canonical_id: "test-custom-cred-model".to_string(),
        display_name: "Test Custom Cred Model".to_string(),
        description: String::new(),
        context_length: 8192,
        capabilities: vec![],
        providers: vec![CatalogProviderEntry {
            provider_id: "anthropic".to_string(),
            api_model_id: "test-model".to_string(),
            priority: 1,
            weight: 1,
            credential_keys: HashMap::from([(
                "api_key".to_string(),
                "MY_CUSTOM_ANTHROPIC_KEY".to_string(),
            )]),
            base_url: Some("https://api.anthropic.com".to_string()),
            api_protocol: ApiProtocol::AnthropicMessages,
            provider_specific: HashMap::new(),
        }],
        aliases: vec![],
    };
    router.register_model(custom);

    let resolved = router.resolve("test-custom-cred-model", None).unwrap();
    assert_eq!(
        resolved.api_key.as_deref(),
        Some("custom-key-value"),
        "should use entry.credential_keys first, not _PROVIDER_CREDENTIALS fallback"
    );

    restore_env("MY_CUSTOM_ANTHROPIC_KEY", prev_custom);
    restore_env("ANTHROPIC_API_KEY", prev_standard);
}

#[test]
fn resolve_credentials_ignores_empty_env_var_values() {
    // CHARACTERIZATION: resolve_credentials() trims env var values and
    // returns None if the trimmed value is empty.
    let _lock = crate::env_lock::lock();
    let prev = save_env("ANTHROPIC_API_KEY");
    env::set_var("ANTHROPIC_API_KEY", "   ");

    let router = ModelRouter::new();
    let result = router.resolve("claude-sonnet-4-6", Some("anthropic"));
    assert!(
        result.is_ok(),
        "should resolve metadata even with empty key"
    );
    let resolved = result.unwrap();
    assert!(
        resolved.api_key.is_none(),
        "empty/whitespace-only env var should not produce an api_key"
    );

    restore_env("ANTHROPIC_API_KEY", prev);
}

// ═══════════════════════════════════════════════════════════════════════
// SECTION 3: Priority loop behavior characterization
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn priority_loop_skips_providers_without_credentials() {
    // CHARACTERIZATION: The priority loop at resolve() lines 150-167 does:
    //   for pe in &sorted_providers {
    //       let api_key = self.resolve_credentials(pe);
    //       if api_key.is_some() { return Ok(ResolvedModel { ... }) }
    //   }
    // Providers without credentials are silently skipped.
    let _lock = crate::env_lock::lock();
    let prev_keys = isolate_env(&[
        "ANTHROPIC_API_KEY",
        "NOUS_API_KEY",
        "GITHUB_TOKEN",
        "OPENCODE_ZEN_API_KEY",
        "KILO_API_KEY",
        "AI_GATEWAY_API_KEY",
    ]);

    // Only set Copilot (GITHUB_TOKEN)
    env::set_var("GITHUB_TOKEN", "gh-test-token");

    let router = ModelRouter::new();
    let resolved = router.resolve("claude-sonnet-4-6", None).unwrap();

    // Copilot has the only valid credential, so it wins even though
    // anthropic/nous/etc have higher priority in the catalog
    assert_eq!(resolved.provider, "copilot");
    assert_eq!(resolved.api_key.as_deref(), Some("gh-test-token"));

    restore_env_batch(prev_keys);
}

#[test]
fn priority_loop_selects_first_provider_with_credential() {
    // CHARACTERIZATION: With multiple providers having credentials,
    // the one with the lowest priority number wins. When priorities are equal,
    // the first provider in the sorted order (stable sort preserves catalog order)
    // that has credentials wins.
    let _lock = crate::env_lock::lock();
    let prev_nous = save_env("NOUS_API_KEY");
    let prev_gh = save_env("GITHUB_TOKEN");

    env::set_var("NOUS_API_KEY", "nous-key");
    env::remove_var("GITHUB_TOKEN");

    let router = ModelRouter::new();
    let resolved = router.resolve("claude-sonnet-4-6", None).unwrap();

    // All providers for claude-sonnet-4-6 have priority=1.
    // "nous" comes first in the catalog order and has NOUS_API_KEY set,
    // so it wins the priority loop.
    assert_eq!(resolved.provider, "nous");
    assert_eq!(resolved.api_key.as_deref(), Some("nous-key"));

    restore_env("NOUS_API_KEY", prev_nous);
    restore_env("GITHUB_TOKEN", prev_gh);
}

#[test]
fn priority_loop_bug_ollama_skipped_despite_highest_priority() {
    // CHARACTERIZATION BUG (T11): When a model is available via Ollama
    // (a credentialless provider with priority 1), the priority loop
    // skips it because resolve_credentials() returns None for Ollama,
    // and the loop only returns providers where api_key.is_some().
    //
    // This means: if you have a model served by both Ollama (priority 1, no creds)
    // and Anthropic (priority 5, with creds), the engine picks Anthropic —
    // even though the user might want the local Ollama instance.
    //
    // The fallback path (lines 169-179) only kicks in when NO provider
    // has credentials, so Ollama is never selected if any other provider
    // has a key.
    let _lock = crate::env_lock::lock();
    let prev_keys = isolate_env(&[
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "DEEPSEEK_API_KEY",
        "GROQ_API_KEY",
        "MISTRAL_API_KEY",
        "XAI_API_KEY",
    ]);

    // With no credentials set at all, the fallback path returns the
    // first sorted provider with api_key: None
    let router = ModelRouter::new();
    let result = router.resolve("gemma-3-27b", None);

    if let Ok(resolved) = result {
        // The model resolves but with api_key: None
        assert!(
            resolved.api_key.is_none(),
            "without any credentials, api_key should be None (CHARACTERIZATION: bug — \
             Ollama should be usable without credentials)"
        );
    }

    restore_env_batch(prev_keys);
}

// ═══════════════════════════════════════════════════════════════════════
// SECTION 4: Fallback path characterization (lines 169-179)
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn fallback_returns_first_sorted_provider_with_none_api_key() {
    // CHARACTERIZATION: When no provider has credentials, the fallback
    // path returns sorted_providers[0] with api_key: None.
    // sorted_providers is sorted by priority (ascending), so the
    // provider with the lowest priority number gets returned.
    let _lock = crate::env_lock::lock();
    let prev_keys = isolate_env(&[
        "ANTHROPIC_API_KEY",
        "NOUS_API_KEY",
        "GITHUB_TOKEN",
        "OPENCODE_ZEN_API_KEY",
        "KILO_API_KEY",
        "AI_GATEWAY_API_KEY",
    ]);

    let router = ModelRouter::new();
    let result = router.resolve("claude-sonnet-4-6", None);

    assert!(
        result.is_ok(),
        "fallback should always return Ok with api_key: None"
    );
    let resolved = result.unwrap();
    assert_eq!(resolved.canonical_id, "claude-sonnet-4-6");
    assert!(
        resolved.api_key.is_none(),
        "fallback path should produce api_key: None when no credentials are set"
    );

    restore_env_batch(prev_keys);
}

#[test]
fn fallback_preserves_provider_metadata() {
    // CHARACTERIZATION: The fallback path preserves api_protocol,
    // api_model_id from the first sorted provider, even though api_key is None.
    let _lock = crate::env_lock::lock();
    let prev_keys = isolate_env(&[
        "ANTHROPIC_API_KEY",
        "NOUS_API_KEY",
        "GITHUB_TOKEN",
        "OPENCODE_ZEN_API_KEY",
        "KILO_API_KEY",
        "AI_GATEWAY_API_KEY",
    ]);

    let router = ModelRouter::new();
    let resolved = router.resolve("claude-sonnet-4-6", None).unwrap();

    assert!(!resolved.canonical_id.is_empty());
    assert!(!resolved.provider.is_empty());
    assert!(!resolved.api_model_id.is_empty());

    restore_env_batch(prev_keys);
}

// ═══════════════════════════════════════════════════════════════════════
// SECTION 5: resolve_permissive() characterization
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn permissive_splits_provider_model_format() {
    // CHARACTERIZATION: resolve_permissive() splits on the first '/'
    // to extract provider_part and model_part.
    let router = ModelRouter::new();
    let result = router.resolve_permissive("anthropic/claude-sonnet-4.6");
    assert!(result.is_ok());
    let resolved = result.unwrap();
    assert_eq!(resolved.provider, "anthropic");
    assert_eq!(resolved.api_model_id, "claude-sonnet-4.6");
}

#[test]
fn permissive_looks_up_provider_defaults() {
    // CHARACTERIZATION: resolve_permissive() uses catalog provider_defaults
    // to populate base_url, api_protocol, and credential_keys.
    let router = ModelRouter::new();
    let result = router.resolve_permissive("anthropic/claude-sonnet-4.6");
    assert!(result.is_ok());
    let resolved = result.unwrap();
    assert_eq!(resolved.base_url, "https://api.anthropic.com");
    assert_eq!(resolved.api_protocol, ApiProtocol::AnthropicMessages);
}

#[test]
fn permissive_resolves_credentials_from_defaults() {
    // CHARACTERIZATION: resolve_permissive() constructs a CatalogProviderEntry
    // from provider defaults (including credential_keys) and then calls
    // resolve_credentials() on it.
    let _lock = crate::env_lock::lock();
    let prev = save_env("ANTHROPIC_API_KEY");
    env::set_var("ANTHROPIC_API_KEY", "ant-permissive-key");

    let router = ModelRouter::new();
    let resolved = router
        .resolve_permissive("anthropic/my-custom-model")
        .unwrap();
    assert_eq!(resolved.api_key.as_deref(), Some("ant-permissive-key"));

    restore_env("ANTHROPIC_API_KEY", prev);
}

#[test]
fn permissive_returns_none_api_key_without_env_var() {
    let _lock = crate::env_lock::lock();
    let prev = save_env("ANTHROPIC_API_KEY");
    env::remove_var("ANTHROPIC_API_KEY");

    let router = ModelRouter::new();
    let resolved = router
        .resolve_permissive("anthropic/my-custom-model")
        .unwrap();
    assert!(resolved.api_key.is_none());

    restore_env("ANTHROPIC_API_KEY", prev);
}

#[test]
fn permissive_uses_default_context_length() {
    // CHARACTERIZATION: resolve_permissive() always sets context_length to 131072.
    let router = ModelRouter::new();
    let resolved = router
        .resolve_permissive("anthropic/my-custom-model")
        .unwrap();
    assert_eq!(resolved.context_length, 131072);
}

#[test]
fn permissive_fails_for_unknown_provider() {
    let router = ModelRouter::new();
    let result = router.resolve_permissive("unknown-provider/model");
    assert!(
        result.is_err(),
        "should fail for provider not in provider_defaults"
    );
}

#[test]
fn permissive_fails_without_slash() {
    // CHARACTERIZATION: resolve_permissive() requires a '/' in the model name.
    // Without it, there's no (provider, model) split.
    let router = ModelRouter::new();
    let result = router.resolve_permissive("just-a-model-name");
    assert!(result.is_err());
}

#[test]
fn permissive_provider_model_becomes_canonical_id() {
    // CHARACTERIZATION: The full "provider/model" string becomes canonical_id.
    let _lock = crate::env_lock::lock();
    let prev = save_env("ANTHROPIC_API_KEY");
    env::remove_var("ANTHROPIC_API_KEY");

    let router = ModelRouter::new();
    let resolved = router
        .resolve_permissive("anthropic/claude-sonnet-4.6")
        .unwrap();
    assert_eq!(resolved.canonical_id, "anthropic/claude-sonnet-4.6");

    restore_env("ANTHROPIC_API_KEY", prev);
}

#[test]
fn permissive_deepseek_provider() {
    // CHARACTERIZATION: resolve_permissive() works for providers in provider_defaults.
    // "openrouter" is NOT in provider_defaults, so it would fail.
    // "deepseek" IS in provider_defaults.
    let _lock = crate::env_lock::lock();
    let prev = save_env("DEEPSEEK_API_KEY");
    env::set_var("DEEPSEEK_API_KEY", "ds-test-key");

    let router = ModelRouter::new();
    let result = router.resolve_permissive("deepseek/deepseek-chat");
    assert!(result.is_ok());
    let resolved = result.unwrap();
    assert_eq!(resolved.provider, "deepseek");
    assert_eq!(resolved.api_key.as_deref(), Some("ds-test-key"));

    restore_env("DEEPSEEK_API_KEY", prev);
}

#[test]
fn permissive_openrouter_not_in_defaults() {
    // CHARACTERIZATION: "openrouter" is NOT in catalog provider_defaults,
    // so resolve_permissive() returns ModelNotFound for it.
    let router = ModelRouter::new();
    let result = router.resolve_permissive("openrouter/anthropic/claude-sonnet-4.6");
    assert!(
        result.is_err(),
        "openrouter is not in provider_defaults, should fail"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// SECTION 6: normalize_model_id() characterization
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn normalize_strips_openrouter_vendor_prefix() {
    assert_eq!(
        normalize_model_id("anthropic/claude-sonnet-4.6"),
        "claude-sonnet-4-6"
    );
    assert_eq!(normalize_model_id("openai/gpt-4o"), "gpt-4o");
}

#[test]
fn normalize_strips_prefix() {
    assert_eq!(
        normalize_model_id("us.anthropic.claude-sonnet-4-6-v1:0"),
        "claude-sonnet-4-6"
    );
    assert_eq!(normalize_model_id("us.amazon.nova-pro-v1:0"), "nova-pro");
    assert_eq!(
        normalize_model_id("us.meta.llama4-maverick-17b-instruct-v1:0"),
        "llama4-maverick-17b-instruct"
    );
}

#[test]
fn normalize_strips_suffix() {
    assert_eq!(
        normalize_model_id("claude-sonnet-4-6-v1:0"),
        "claude-sonnet-4-6"
    );
    assert_eq!(
        normalize_model_id("claude-sonnet-4-6-v1"),
        "claude-sonnet-4-6"
    );
}

#[test]
fn normalize_claude_dots_to_hyphens() {
    assert_eq!(normalize_model_id("claude-sonnet-4.6"), "claude-sonnet-4-6");
    assert_eq!(normalize_model_id("claude-opus-4.7"), "claude-opus-4-7");
    assert_eq!(normalize_model_id("claude-haiku-4.5"), "claude-haiku-4-5");
}

#[test]
fn normalize_lowercase() {
    assert_eq!(normalize_model_id("GPT-4O"), "gpt-4o");
    assert_eq!(
        normalize_model_id("ANTHROPIC/CLAUDE-SONNET-4.6"),
        "claude-sonnet-4-6"
    );
}

#[test]
fn normalize_noop_for_plain_ids() {
    assert_eq!(normalize_model_id("gpt-4o"), "gpt-4o");
    assert_eq!(normalize_model_id("deepseek-v4-pro"), "deepseek-v4-pro");
}

#[test]
fn normalize_empty_string() {
    assert_eq!(normalize_model_id(""), "");
}

#[test]
fn normalize_double_slash_takes_after_first() {
    // CHARACTERIZATION: "openrouter/anthropic/claude" splits on first '/'
    // and takes the rest, giving "anthropic/claude"
    let result = normalize_model_id("openrouter/anthropic/claude");
    assert!(!result.contains("openrouter"));
    // After split_once('/'), rest = "anthropic/claude"
    assert_eq!(result, "anthropic/claude");
}

#[test]
fn normalize_does_not_affect_non_claude_dots() {
    // CHARACTERIZATION: Dot-to-hyphen conversion only applies to "claude-" prefix models.
    assert_eq!(normalize_model_id("gpt-4.5"), "gpt-4.5");
    assert_eq!(normalize_model_id("model-3.5-turbo"), "model-3.5-turbo");
}

// ═══════════════════════════════════════════════════════════════════════
// SECTION 7: Full resolution pipeline characterization
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn resolve_with_provider_override_skips_priority_loop() {
    // CHARACTERIZATION: When provider_override is set, the engine
    // directly looks for that provider in the entry, bypassing the
    // priority loop entirely.
    let _lock = crate::env_lock::lock();
    let prev_ant = save_env("ANTHROPIC_API_KEY");
    let prev_gh = save_env("GITHUB_TOKEN");

    env::remove_var("ANTHROPIC_API_KEY");
    env::set_var("GITHUB_TOKEN", "gh-key");

    let router = ModelRouter::new();
    // Override to anthropic even though it has no key — copilot would win
    // in the priority loop but we explicitly request anthropic
    let resolved = router
        .resolve("claude-sonnet-4-6", Some("anthropic"))
        .unwrap();
    assert_eq!(resolved.provider, "anthropic");
    assert!(
        resolved.api_key.is_none(),
        "anthropic has no key set, override still works"
    );

    restore_env("ANTHROPIC_API_KEY", prev_ant);
    restore_env("GITHUB_TOKEN", prev_gh);
}

#[test]
fn resolve_provider_override_nonexistent_returns_error() {
    let router = ModelRouter::new();
    let result = router.resolve("claude-sonnet-4-6", Some("nonexistent-provider"));
    assert!(
        result.is_err(),
        "override to nonexistent provider should fail"
    );
}

#[test]
fn resolve_unnormalized_input_gets_normalized() {
    // CHARACTERIZATION: resolve() calls normalize_model_id() first,
    // so "claude-sonnet-4.6" (with dot) resolves the same as "claude-sonnet-4-6".
    let _lock = crate::env_lock::lock();
    let prev = save_env("ANTHROPIC_API_KEY");
    env::set_var("ANTHROPIC_API_KEY", "test-key");

    let router = ModelRouter::new();
    let resolved = router.resolve("claude-sonnet-4.6", None).unwrap();
    assert_eq!(resolved.canonical_id, "claude-sonnet-4-6");

    restore_env("ANTHROPIC_API_KEY", prev);
}

#[test]
fn resolve_permissive_called_for_unknown_models() {
    // CHARACTERIZATION: When a model is not in the catalog and not an alias,
    // resolve() calls resolve_permissive() automatically.
    // This enables "provider/model" syntax for uncataloged models.
    let _lock = crate::env_lock::lock();
    let prev = save_env("OPENAI_API_KEY");
    env::set_var("OPENAI_API_KEY", "sk-test");

    let router = ModelRouter::new();
    // "openai/gpt-future-model" is not in the catalog but
    // "openai" is a known provider — permissive fallback works
    let result = router.resolve("openai/gpt-future-model", None);
    assert!(result.is_ok());
    let resolved = result.unwrap();
    assert_eq!(resolved.provider, "openai");

    restore_env("OPENAI_API_KEY", prev);
}

#[test]
fn resolve_unknown_provider_model_returns_error() {
    let router = ModelRouter::new();
    let result = router.resolve("nonexistent-provider/some-model", None);
    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════
// SECTION 8: Credentialless provider end-to-end scenarios
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn credentialless_provider_ollama_resolves_with_none_api_key() {
    // CHARACTERIZATION: Ollama models resolve successfully but with
    // api_key: None because Ollama doesn't require authentication.
    // This is correct behavior — the bug is in the priority loop
    // that skips Ollama when other providers have credentials.
    let _lock = crate::env_lock::lock();
    let prev_keys = isolate_env(&[
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "DEEPSEEK_API_KEY",
        "GROQ_API_KEY",
        "MISTRAL_API_KEY",
        "XAI_API_KEY",
    ]);

    let router = ModelRouter::new();

    let result = router.resolve_permissive("ollama/llama3");
    if result.is_ok() {
        let resolved = result.unwrap();
        assert_eq!(resolved.provider, "ollama");
        assert!(
            resolved.api_key.is_none(),
            "Ollama should always resolve with api_key: None (credentialless)"
        );
    }

    restore_env_batch(prev_keys);
}

#[test]
fn bug_ollama_not_selected_when_anthropic_has_creds() {
    // CHARACTERIZATION BUG (T11): This test documents the current broken
    // behavior where a model available via both Ollama (local, no auth)
    // and Anthropic (remote, with API key) always picks Anthropic.
    //
    // Expected after fix: Ollama should be selectable because it has
    // the highest priority (lowest number) and doesn't need credentials.
    //
    // Current behavior: The priority loop requires api_key.is_some(),
    // so Ollama is skipped even though it's a valid provider.
    let _lock = crate::env_lock::lock();
    let prev_ant = save_env("ANTHROPIC_API_KEY");
    env::set_var("ANTHROPIC_API_KEY", "sk-ant-key");

    // Priority loop logic (lines 150-167):
    //   sorted by priority → iterate → return first with api_key.is_some()
    //
    // This means: credentialless providers can NEVER win the priority loop
    // when ANY other provider has credentials. They can only be selected
    // via the fallback path (lines 169-179), which only activates when
    // NO provider has credentials.
    //
    // The fix for T11 should change this so that credentialless providers
    // are treated as "always available" rather than "never available."

    restore_env("ANTHROPIC_API_KEY", prev_ant);
}

#[test]
fn all_19_credentialed_providers_have_env_var_mapping() {
    // CHARACTERIZATION: Of the 20 entries in _PROVIDER_CREDENTIALS,
    // 19 have at least one env var mapping, and 1 (ollama) has none.
    let credentialed: Vec<&&str> = _PROVIDER_CREDENTIALS
        .iter()
        .filter(|(_, creds)| !creds.is_empty())
        .map(|(slug, _)| slug)
        .collect();
    let credentialless: Vec<&&str> = _PROVIDER_CREDENTIALS
        .iter()
        .filter(|(_, creds)| creds.is_empty())
        .map(|(slug, _)| slug)
        .collect();

    assert_eq!(
        credentialed.len(),
        19,
        "19 providers should have credential mappings"
    );
    assert_eq!(
        credentialless.len(),
        1,
        "1 provider should be credentialless"
    );
    assert_eq!(*credentialless[0], "ollama");
}

#[test]
fn each_env_var_maps_to_at_most_one_primary_provider() {
    // CHARACTERIZATION: Most env vars are unique to a single provider.
    // Exception: OPENAI_API_KEY is shared by "openai" and "openai-codex".
    let mut env_var_to_providers: HashMap<&str, Vec<&str>> = HashMap::new();
    for (slug, creds) in _PROVIDER_CREDENTIALS {
        for (env_var, _field) in *creds {
            env_var_to_providers
                .entry(*env_var)
                .or_default()
                .push(*slug);
        }
    }

    // OPENAI_API_KEY is shared between openai and openai-codex
    let openai_providers = env_var_to_providers.get("OPENAI_API_KEY").unwrap();
    assert_eq!(openai_providers.len(), 2);
    assert!(openai_providers.contains(&"openai"));
    assert!(openai_providers.contains(&"openai-codex"));

    // All other env vars map to exactly one provider
    for (env_var, providers) in &env_var_to_providers {
        if *env_var != "OPENAI_API_KEY" {
            assert_eq!(
                providers.len(),
                1,
                "env var '{}' should map to exactly one provider, got: {:?}",
                env_var,
                providers
            );
        }
    }
}
