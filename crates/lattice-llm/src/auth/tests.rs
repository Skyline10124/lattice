// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt, symlink},
};

const KEY: &str = "credential-test-secret-7J29";

fn fixture() -> (tempfile::TempDir, AuthStore) {
    let directory = tempfile::tempdir().unwrap();
    let store = AuthStore::new(directory.path().join("config"));
    (directory, store)
}

fn write(store: &AuthStore, bytes: impl AsRef<[u8]>) {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&store.directory)
        .unwrap();
    let path = store.directory.join("auth.json");
    fs::write(&path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn state(store: &AuthStore, provider: &str) -> AuthState {
    store
        .status()
        .unwrap()
        .providers
        .into_iter()
        .find(|row| row.provider == provider)
        .unwrap()
        .status
}

fn chatgpt() -> serde_json::Value {
    serde_json::json!({"type":"chatgpt", "host_id":"host", "client_id":"client", "account":{"subject":"subject", "email":"user@example.com"}, "tokens":{
        "access_token":"access-test-secret", "refresh_token":"refresh-test-secret", "id_token":"id-test-secret",
        "expires_at":"2020-01-01T00:00:00+00:00", "scopes":["openid", "chatgpt.tokens.use.direct"]
    }})
}

#[test]
fn credential_entries_reject_arrays() {
    let (_directory, store) = fixture();
    write(
        &store,
        r#"{"version":1,"providers":{"openai":["api_key","example"]}}"#,
    );
    assert_eq!(state(&store, "openai"), AuthState::Invalid);
}

#[test]
fn credential_entries_validate_the_entire_provider_id() {
    let (_directory, store) = fixture();
    write(
        &store,
        r#"{"version":1,"providers":{"openai/extra":{"type":"api_key","key":"example"}}}"#,
    );
    assert_eq!(state(&store, "openai"), AuthState::Invalid);
}

#[test]
fn provider_configuration_errors_keep_path_line_and_reason() {
    let (_directory, store) = fixture();
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&store.directory)
        .unwrap();
    let path = store.directory.join("providers.toml");
    fs::write(
        &path,
        "[providers.custom]\nbase_url = 'https://example.com'\n",
    )
    .unwrap();
    let message = store.status().unwrap_err().to_string();
    assert!(message.starts_with(&path.display().to_string()));
    assert!(message.contains(":1: new provider requires protocol"));
}

#[test]
fn set_remove_and_current_registry_policy() {
    let (_directory, store) = fixture();
    assert_eq!(state(&store, "deepseek"), AuthState::Missing);
    assert_eq!(state(&store, "ollama"), AuthState::Ready);
    assert!(!store.directory.exists());
    for provider in ["openai", "anthropic", "google"] {
        store.set(provider, KEY).unwrap();
        assert_eq!(state(&store, provider), AuthState::Ready);
    }
    let path = store.directory.join("auth.json");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    for provider in ["unknown", "ollama", "chatgpt"] {
        assert!(store.set(provider, KEY).is_err());
    }
    fs::write(
        store.directory.join("providers.toml"),
        "[providers.openai]\nauth = 'none'\n",
    )
    .unwrap();
    assert!(matches!(
        store.set("openai", KEY),
        Err(AuthError::UnsupportedAuth)
    ));
    assert_eq!(state(&store, "openai"), AuthState::Ready);
    fs::write(
        store.directory.join("providers.toml"),
        "[providers.local]\nprotocol = 'anthropic-messages'\nbase_url = 'http://localhost:8000'\n",
    )
    .unwrap();
    store.set("local", KEY).unwrap();
    fs::write(store.directory.join("providers.toml"), "").unwrap();
    store.remove("local").unwrap();
    store.remove("anthropic").unwrap();
    store.remove("anthropic").unwrap();
    assert_eq!(state(&store, "anthropic"), AuthState::Missing);
    assert_eq!(state(&store, "google"), AuthState::Ready);
}

#[test]
fn saved_key_round_trips_and_setting_again_replaces_it() {
    let (_directory, store) = fixture();
    for expected in [KEY, "replacement-key-9F28"] {
        store.set("openai", expected).unwrap();
        let document = file::Directory::open(&store.directory, false)
            .unwrap()
            .unwrap()
            .read()
            .unwrap();
        let Some(format::Credential::ApiKey { key }) = document.providers.get("openai") else {
            panic!("API key entry is missing");
        };
        assert!(key.0 == expected, "saved key differs from supplied key");
    }
}

#[test]
fn removing_keys_does_not_require_a_valid_provider_registry() {
    let (_directory, store) = fixture();
    store.set("openai", KEY).unwrap();
    fs::write(store.directory.join("providers.toml"), "[broken").unwrap();
    assert!(store.remove("openai").unwrap());
    assert!(!store.remove("already-missing").unwrap());
    let document = file::Directory::open(&store.directory, false)
        .unwrap()
        .unwrap()
        .read()
        .unwrap();
    assert!(!document.providers.contains_key("openai"));
}

#[test]
fn input_errors_describe_the_key_and_available_providers() {
    let (_directory, store) = fixture();
    for key in ["", "bad\nkey", &"k".repeat(16_385)] {
        let error = store.set("openai", key).unwrap_err();
        assert_eq!(
            error.to_string(),
            "API key must be 1–16384 bytes and contain no control characters"
        );
    }
    let message = store.set("not-registered", KEY).unwrap_err().to_string();
    assert!(message.contains("not-registered"));
    assert!(message.contains("available providers for auth set:"));
    for provider in ["openai", "anthropic", "google", "deepseek"] {
        assert!(message.contains(provider));
    }
    assert!(!message.contains("ollama"));
    assert!(!message.contains("chatgpt"));
}

#[test]
fn owner_read_only_credentials_are_accepted_without_permission_changes() {
    let (_directory, store) = fixture();
    store.set("openai", KEY).unwrap();
    let path = store.directory.join("auth.json");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
    assert_eq!(state(&store, "openai"), AuthState::Ready);
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o400
    );
}

#[test]
fn configuration_directory_permissions_are_private_and_not_repaired() {
    let (_directory, store) = fixture();
    assert!(!store.directory.exists());
    store.set("openai", KEY).unwrap();
    assert_eq!(
        fs::metadata(&store.directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let path = store.directory.join("auth.json");
    let original = fs::read(&path).unwrap();
    fs::set_permissions(&store.directory, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(state(&store, "openai"), AuthState::Invalid);
    let reason = "configuration directory permissions are too broad; group and other permissions must be absent; permissions were not changed";
    assert_eq!(
        store.status().unwrap().error.unwrap(),
        format!("auth.json: {reason}")
    );
    for error in [
        store.set("openai", "replacement").unwrap_err(),
        store.remove("openai").unwrap_err(),
    ] {
        assert!(matches!(error, AuthError::InvalidFile(message) if message == reason));
    }
    assert_eq!(
        fs::metadata(&store.directory).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert!(fs::read(&path).unwrap() == original);
    for mode in [0o700, 0o500, 0o700] {
        fs::set_permissions(&store.directory, fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(state(&store, "openai"), AuthState::Ready);
        assert_eq!(
            fs::metadata(&store.directory).unwrap().permissions().mode() & 0o777,
            mode
        );
    }
    store.set("openai", "replacement").unwrap();
    assert!(store.remove("openai").unwrap());
}

#[test]
fn invalid_files_are_never_overwritten_and_status_stays_safe() {
    let (_directory, store) = fixture();
    for source in [
        format!(r#"{{"version":2,"providers":{{"openai":{{"type":"api_key","key":"{KEY}"}}}}}}"#),
        format!(r#"{{"version":1,"providers":{{"openai":{{"type":"api_key","key":"{KEY}","unknown":true}}}}}}"#),
        format!(r#"{{"version":1,"providers":{{"openai":{{"type":"{KEY}"}}}}}}"#),
        r#"{"version":1,"providers":{"openai":{"type":"api_key","key":""}}}"#.into(),
        r#"{"version":1,"providers":{"openai":{"type":"api_key","key":"bad\u0085key"}}}"#.into(),
        r#"{"version":1,"providers":{"chatgpt":{"type":"api_key","key":"wrong"}}}"#.into(),
        r#"{"version":1,"providers":{},"unknown":true}"#.into(),
        r#"{"version":1,"version":1,"providers":{}}"#.into(),
        r#"{"version":1,"providers":{"openai":{"type":"api_key","key":"a"},"openai":{"type":"api_key","key":"b"}}}"#.into(),
        r#"[1,{}]"#.into(),
        r#"{"version":1,"providers":{}} false"#.into(),
        format!(r#"{{"version":1,"providers":{{"openai":{{"type":"api_key","key":"{}"}}}}}}"#, "k".repeat(16_385)),
    ] {
        write(&store, &source);
        let status = store.status().unwrap();
        assert_eq!(state(&store, "openai"), AuthState::Invalid);
        assert_eq!(state(&store, "ollama"), AuthState::Ready);
        assert!(status.error.is_some());
        let set = store.set("openai", "replacement").unwrap_err();
        let remove = store.remove("openai").unwrap_err();
        assert!(!format!("{status:?} {set:?} {set} {remove:?} {remove}").contains(KEY));
        assert!(fs::read(store.directory.join("auth.json")).unwrap() == source.as_bytes());
    }
}

#[test]
fn chatgpt_material_survives_api_key_updates_and_logged_out_records_are_valid() {
    let (_directory, store) = fixture();
    let entry = chatgpt();
    let source = serde_json::json!({"version":1,"providers":{"chatgpt":entry}});
    write(&store, source.to_string());
    store.set("deepseek", KEY).unwrap();
    let saved: serde_json::Value =
        serde_json::from_slice(&fs::read(store.directory.join("auth.json")).unwrap()).unwrap();
    assert_eq!(saved["providers"]["chatgpt"], entry);
    assert!(matches!(
        store.remove("chatgpt"),
        Err(AuthError::ChatgptRemoval)
    ));
    let status = store.status().unwrap();
    let row = status
        .providers
        .iter()
        .find(|row| row.provider == "chatgpt")
        .unwrap();
    assert_eq!(row.status, AuthState::Ready);
    assert_eq!(row.email.as_deref(), Some("user@example.com"));
    assert_eq!(row.expires_at.as_deref(), Some("2020-01-01T00:00:00+00:00"));
    let document = format::parse(&fs::read(store.directory.join("auth.json")).unwrap()).unwrap();
    assert!(
        matches!(document.providers.get("deepseek"), Some(format::Credential::ApiKey { key }) if key.0 == KEY)
    );
    let debug = format!("{document:?}");
    let public = serde_json::to_string(&status).unwrap();
    for secret in [
        KEY,
        "access-test-secret",
        "refresh-test-secret",
        "id-test-secret",
    ] {
        assert!(!debug.contains(secret));
        assert!(!public.contains(secret));
    }
    store.remove("deepseek").unwrap();
    let saved: serde_json::Value =
        serde_json::from_slice(&fs::read(store.directory.join("auth.json")).unwrap()).unwrap();
    assert!(saved["providers"]["chatgpt"] == entry);
    for entry in [
        serde_json::json!({"type":"chatgpt","host_id":"host"}),
        serde_json::json!({"type":"chatgpt","host_id":"host","client_id":"client","account":{"subject":"subject","email":"user@example.com"}}),
    ] {
        write(
            &store,
            serde_json::json!({"version":1,"providers":{"chatgpt":entry}}).to_string(),
        );
        assert_eq!(state(&store, "chatgpt"), AuthState::Missing);
    }
    for path in ["expires_at", "access_token"] {
        let mut entry = chatgpt();
        entry["tokens"][path] = serde_json::json!("invalid");
        if path == "access_token" {
            entry["tokens"][path] = serde_json::Value::Null;
        }
        write(
            &store,
            serde_json::json!({"version":1,"providers":{"chatgpt":entry}}).to_string(),
        );
        assert_eq!(state(&store, "chatgpt"), AuthState::Invalid);
    }
}

#[test]
fn permissions_links_and_special_files_are_rejected_without_repair() {
    let (_directory, store) = fixture();
    store.set("openai", KEY).unwrap();
    let path = store.directory.join("auth.json");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(state(&store, "openai"), AuthState::Invalid);
    assert!(store.set("openai", "replacement").is_err());
    assert!(store.remove("openai").is_err());
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o644
    );
    fs::rename(&path, store.directory.join("original")).unwrap();
    fs::set_permissions(
        store.directory.join("original"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    symlink("original", &path).unwrap();
    assert_eq!(state(&store, "openai"), AuthState::Invalid);
    assert!(
        store
            .status()
            .unwrap()
            .error
            .unwrap()
            .contains("symbolic link")
    );
    assert!(store.set("openai", "replacement").is_err());
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert_eq!(state(&store, "openai"), AuthState::Invalid);
    fs::remove_dir(&path).unwrap();
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &path,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        0,
    )
    .unwrap();
    assert_eq!(state(&store, "openai"), AuthState::Invalid);
    fs::remove_file(&path).unwrap();
    fs::rename(store.directory.join("original"), &path).unwrap();
    assert_eq!(state(&store, "openai"), AuthState::Ready);
    let alias = store.directory.with_file_name("alias");
    symlink(&store.directory, &alias).unwrap();
    let linked = AuthStore::new(alias);
    assert_eq!(state(&linked, "openai"), AuthState::Invalid);
    assert!(
        linked
            .status()
            .unwrap()
            .error
            .unwrap()
            .contains("symbolic link")
    );
    assert!(linked.set("openai", KEY).is_err());
}

#[test]
fn key_and_file_byte_limits_include_serialized_size() {
    let (_directory, store) = fixture();
    for key in ["", "a\n", "a\0", "a\u{7f}", "a\u{85}", &"é".repeat(8_193)] {
        assert!(store.set("openai", key).is_err());
    }
    store.set("openai", "k").unwrap();
    assert_eq!(state(&store, "openai"), AuthState::Ready);
    store.set("openai", &"é".repeat(8_192)).unwrap();
    let original = fs::read(store.directory.join("auth.json")).unwrap();
    let mut document = format::Document::default();
    for index in 0..64 {
        document.providers.insert(
            format!("p{index}"),
            format::Credential::ApiKey {
                key: Secret("\\".repeat(16_384)),
            },
        );
    }
    let directory = file::Directory::open(&store.directory, false)
        .unwrap()
        .unwrap();
    assert!(directory.write(&document).is_err());
    assert!(fs::read(store.directory.join("auth.json")).unwrap() == original);
    let mut padded = original;
    padded.resize(1_048_576, b' ');
    write(&store, &padded);
    assert_eq!(state(&store, "openai"), AuthState::Ready);
    padded.push(b' ');
    write(&store, &padded);
    assert_eq!(state(&store, "openai"), AuthState::Invalid);
    assert!(store.set("openai", KEY).is_err());
}

#[test]
fn successful_replacement_is_private_and_failed_temporary_creation_preserves_original() {
    let (_directory, store) = fixture();
    store.set("openai", KEY).unwrap();
    let path = store.directory.join("auth.json");
    let original = fs::read(&path).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::read_dir(&store.directory).unwrap().count(), 1);
    fs::set_permissions(&store.directory, fs::Permissions::from_mode(0o500)).unwrap();
    let failed = store.set("openai", "replacement");
    fs::set_permissions(&store.directory, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(
        failed,
        Err(AuthError::Io {
            operation: "create temporary credential file",
            kind: std::io::ErrorKind::PermissionDenied
        })
    ));
    assert!(fs::read(&path).unwrap() == original);
    assert_eq!(fs::read_dir(&store.directory).unwrap().count(), 1);
}
