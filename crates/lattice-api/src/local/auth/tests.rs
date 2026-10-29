// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use crate::local::{self, AuthState, LocalClient, LocalErrorKind, Request, Response, ipc};
use std::{fs, os::unix::fs::PermissionsExt, time::Duration};

const KEY: &str = "auth-ipc-test-secret-9E82";

#[tokio::test]
async fn auth_ipc_uses_current_files_and_never_returns_credentials() {
    let directory = tempfile::tempdir().unwrap();
    let layout = crate::StateLayout::in_state_root(directory.path());
    let config = directory.path().join(".lattice");
    let server = tokio::spawn(local::serve(layout.clone(), Some(directory.path().into())));
    let client = LocalClient::new(layout);
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.status().await.is_err() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let state = |status: local::AuthStatus, provider: &str| {
        status
            .providers
            .into_iter()
            .find(|row| row.provider == provider)
            .unwrap()
            .status
    };
    assert_eq!(
        state(client.auth_status().await.unwrap(), "openai"),
        AuthState::Missing
    );
    client.auth_set("openai", KEY.into()).await.unwrap();
    for key in ["".into(), "bad\nkey".into(), "k".repeat(16_385)] {
        let error = client.auth_set("openai", key).await.unwrap_err();
        assert_eq!(error.kind, LocalErrorKind::BadRequest);
        assert_eq!(
            error.message,
            "API key must be 1–16384 bytes and contain no control characters"
        );
    }
    fs::set_permissions(&config, fs::Permissions::from_mode(0o500)).unwrap();
    let denied = client.auth_set("openai", "replacement".into()).await;
    fs::set_permissions(&config, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(denied.unwrap_err().kind, LocalErrorKind::Permission);
    let status = client.auth_status().await.unwrap();
    assert_eq!(state(status.clone(), "openai"), AuthState::Ready);
    assert!(!serde_json::to_string(&status).unwrap().contains(KEY));
    assert!(!format!("{status:?}").contains(KEY));
    for provider in ["not-registered", "ollama", "chatgpt"] {
        let error = client.auth_set(provider, KEY.into()).await.unwrap_err();
        assert_eq!(error.kind, LocalErrorKind::BadRequest);
        assert!(!format!("{error:?}").contains(KEY));
    }
    assert_eq!(
        client.auth_remove("chatgpt").await.unwrap_err().kind,
        LocalErrorKind::BadRequest
    );
    assert!(client.auth_remove("openai").await.unwrap());
    assert!(!client.auth_remove("openai").await.unwrap());
    assert_eq!(
        state(client.auth_status().await.unwrap(), "openai"),
        AuthState::Missing
    );
    fs::write(
        config.join("providers.toml"),
        "[providers.custom]\nprotocol = 'generate-content'\nbase_url = 'http://localhost:8000'\n",
    )
    .unwrap();
    client.auth_set("custom", KEY.into()).await.unwrap();
    assert_eq!(
        state(client.auth_status().await.unwrap(), "custom"),
        AuthState::Ready
    );
    fs::write(config.join("providers.toml"), "[providers.custom]\nprotocol = 'generate-content'\nbase_url = 'http://localhost:8000'\nauth = 'none'\n").unwrap();
    assert_eq!(
        client
            .auth_set("custom", KEY.into())
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::BadRequest
    );
    fs::write(
        config.join("auth.json"),
        format!(r#"{{"version":"{KEY}"}}"#),
    )
    .unwrap();
    let status = client.auth_status().await.unwrap();
    assert_eq!(state(status.clone(), "openai"), AuthState::Invalid);
    assert_eq!(state(status.clone(), "custom"), AuthState::Ready);
    assert!(!serde_json::to_string(&status).unwrap().contains(KEY));
    for invalid_directory in [false, true] {
        if invalid_directory {
            fs::set_permissions(&config, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let error = client
            .redact_configuration_error(crate::ApiError::FrontendConfiguration(
                crate::ConfigurationDiagnostic::new(
                    "agent profile missing: sk-private-profile-value-123".into(),
                ),
            ))
            .await;
        assert_eq!(error.kind, LocalErrorKind::BadRequest);
        assert_eq!(error.message, "agent profile missing: [REDACTED]");
    }
    fs::set_permissions(&config, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        client
            .auth_set("openai", KEY.into())
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::BadRequest
    );
    assert_eq!(
        client.auth_remove("openai").await.unwrap_err().kind,
        LocalErrorKind::BadRequest
    );
    fs::write(
        config.join("providers.toml"),
        format!("[providers.custom]\nunknown = '{KEY}'\n"),
    )
    .unwrap();
    let error = client.auth_status().await.unwrap_err();
    assert_eq!(error.kind, LocalErrorKind::BadRequest);
    assert!(!format!("{error:?} {error}").contains(KEY));
    fs::write(
        config.join("auth.json"),
        serde_json::json!({"version":1,"providers":{"openai":{"type":"api_key","key":KEY}}})
            .to_string(),
    )
    .unwrap();
    assert!(client.auth_remove("openai").await.unwrap());
    assert!(!client.auth_remove("openai").await.unwrap());
    client.stop().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn invalid_home_only_rejects_operations_that_need_configuration() {
    for (home, reason) in [
        (None, "HOME is not set"),
        (
            Some(std::path::PathBuf::from("relative")),
            "HOME must be an absolute path",
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let layout = crate::StateLayout::in_state_root(directory.path());
        let server = tokio::spawn(local::serve(layout.clone(), home));
        let client = LocalClient::new(layout);
        tokio::time::timeout(Duration::from_secs(5), async {
            while client.status().await.is_err() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        for error in [
            client.auth_status().await.unwrap_err(),
            client.auth_set("openai", KEY.into()).await.unwrap_err(),
            client.auth_remove("openai").await.unwrap_err(),
        ] {
            assert_eq!(error.kind, LocalErrorKind::BadRequest);
            assert_eq!(error.message, reason);
        }
        assert!(!client.status().await.unwrap().stopping);
        client.stop().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

#[test]
fn credential_internal_errors_preserve_their_class_and_reason() {
    let error = super::error(lattice_llm::auth::AuthError::Internal(
        "credential writer lock is unavailable",
    ));
    assert_eq!(error.kind, LocalErrorKind::Internal);
    assert_eq!(error.message, "credential writer lock is unavailable");
}

#[tokio::test]
async fn auth_frames_round_trip_without_changing_existing_frame_shape() {
    let (mut sender, mut receiver) = tokio::net::UnixStream::pair().unwrap();
    for wire in [
        serde_json::json!({"operation":"auth_set","provider":"openai","key":KEY}),
        serde_json::json!({"operation":"auth_remove","provider":"openai"}),
        serde_json::json!({"operation":"auth_status"}),
    ] {
        let request: Request = serde_json::from_value(wire.clone()).unwrap();
        ipc::write(&mut sender, &request).await.unwrap();
        let received: Request = ipc::read(&mut receiver).await.unwrap();
        assert!(serde_json::to_value(&received).unwrap() == wire);
    }
    assert!(
        serde_json::from_value::<Request>(serde_json::json!({"operation":"auth_status","key":KEY}))
            .is_err()
    );
    for (response, expected) in [
        (
            Response::AuthUpdated,
            serde_json::json!({"response":"auth_updated"}),
        ),
        (
            Response::AuthRemoved { removed: false },
            serde_json::json!({"response":"auth_removed","value":{"removed":false}}),
        ),
    ] {
        let mut writer = ipc::ResponseWriter::default();
        writer.write(&mut sender, &response).await.unwrap();
        let received: Response = ipc::ResponseReader::default()
            .read(&mut receiver)
            .await
            .unwrap();
        assert_eq!(serde_json::to_value(received).unwrap(), expected);
    }
}
