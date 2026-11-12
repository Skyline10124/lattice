// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use crate::local::{
    self, Availability, ConfigurationRequirement, LocalErrorKind, Request, Response, ipc,
};
use lattice_local::{AuthLoginEvent, LocalClient};
use std::{fs, os::unix::fs::PermissionsExt, time::Duration};

const KEY: &str = "auth-ipc-test-secret-9E82";

fn spawn_test_server(
    layout: crate::StateLayout,
    home: Option<std::path::PathBuf>,
) -> tokio::task::JoinHandle<local::Result<()>> {
    let (_stop_tx, stop_rx) = tokio::sync::mpsc::channel(1);
    tokio::spawn(async move {
        let exit = local::serve_for_test(layout, home, Duration::from_secs(60), stop_rx).await?;
        drop(exit.instance);
        exit.failure.map_or(Ok(()), Err)
    })
}

#[tokio::test]
async fn stop_cancels_browser_login_and_releases_the_backend_instance() {
    let directory = tempfile::tempdir().unwrap();
    let layout = crate::StateLayout::in_state_root(directory.path());
    let server = spawn_test_server(layout.clone(), Some(directory.path().into()));
    let client = LocalClient::new(layout);
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.status().await.is_err() {
            tokio::task::yield_now().await;
        }
        let mut login = client.auth_login("chatgpt").await.unwrap();
        assert!(matches!(
            login.next().await.unwrap(),
            Some(AuthLoginEvent::Authorization(_))
        ));
        let (stopped, outcome) = tokio::join!(client.stop(), login.next());
        assert!(stopped.unwrap().stopping);
        assert!(matches!(
            outcome.unwrap(),
            Some(AuthLoginEvent::Finished(
                local::AuthLoginOutcome::Cancelled {}
            ))
        ));
        server.await.unwrap().unwrap();
        assert!(
            tokio::net::TcpListener::bind("127.0.0.1:1455")
                .await
                .is_ok()
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn auth_ipc_uses_current_files_and_never_returns_credentials() {
    let directory = tempfile::tempdir().unwrap();
    let layout = crate::StateLayout::in_state_root(directory.path());
    let config = directory.path().join(".lattice");
    let server = spawn_test_server(layout.clone(), Some(directory.path().into()));
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
            .availability
    };
    assert_eq!(
        state(client.auth_status().await.unwrap(), "openai"),
        Availability::Unconfigured {
            requirement: ConfigurationRequirement::ApiKey
        }
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
    assert_eq!(state(status.clone(), "openai"), Availability::Ready);
    assert!(!serde_json::to_string(&status).unwrap().contains(KEY));
    assert!(!format!("{status:?}").contains(KEY));
    for provider in ["not-registered", "ollama", "chatgpt"] {
        let error = client.auth_set(provider, KEY.into()).await.unwrap_err();
        assert_eq!(error.kind, LocalErrorKind::BadRequest);
        assert!(!format!("{error:?}").contains(KEY));
    }
    let removed = client.auth_remove("chatgpt").await.unwrap();
    assert!(!removed.removed);
    assert_eq!(removed.revocation, Some(local::RevocationStatus::NotNeeded));
    let host = "urn:uuid:2fde08e4-7b61-47cc-adde-8fb8b05f5b31";
    let registration = serde_json::json!({"type":"chatgpt", "host_id":host,
        "client_id":"registered-client", "account":{"subject":"subject", "email":"user@example.com"}});
    let auth_path = config.join("auth.json");
    fs::write(
        &auth_path,
        serde_json::json!({"version":1,"providers":{
            "openai":{"type":"api_key","key":KEY}, "chatgpt":registration.clone()
        }})
        .to_string(),
    )
    .unwrap();
    let cache = config.join("cache/chatgpt-models.json");
    fs::create_dir_all(cache.parent().unwrap()).unwrap();
    fs::write(&cache, r#"{"models":[{"id":"synthetic-model"}]}"#).unwrap();
    assert_eq!(
        client.auth_logout("chatgpt").await.unwrap(),
        local::RevocationStatus::NotNeeded
    );
    let document: serde_json::Value =
        serde_json::from_slice(&fs::read(&auth_path).unwrap()).unwrap();
    for field in ["host_id", "client_id", "account"] {
        assert_eq!(document["providers"]["chatgpt"][field], registration[field]);
    }
    assert_eq!(
        fs::read_to_string(&cache).unwrap(),
        r#"{"models":[{"id":"synthetic-model"}]}"#
    );
    let removed = client.auth_remove("chatgpt").await.unwrap();
    assert!(removed.removed);
    assert_eq!(removed.revocation, Some(local::RevocationStatus::NotNeeded));
    let document: serde_json::Value =
        serde_json::from_slice(&fs::read(&auth_path).unwrap()).unwrap();
    assert_eq!(document["providers"]["chatgpt"]["host_id"], host);
    for field in ["client_id", "account", "tokens"] {
        assert!(document["providers"]["chatgpt"][field].is_null());
    }
    assert!(!cache.exists());
    let removed = client.auth_remove("chatgpt").await.unwrap();
    assert!(!removed.removed);
    assert_eq!(removed.revocation, Some(local::RevocationStatus::NotNeeded));
    assert!(client.auth_remove("openai").await.unwrap().removed);
    assert!(!client.auth_remove("openai").await.unwrap().removed);
    assert_eq!(
        state(client.auth_status().await.unwrap(), "openai"),
        Availability::Unconfigured {
            requirement: ConfigurationRequirement::ApiKey
        }
    );
    fs::write(
        config.join("providers.toml"),
        "[providers.custom]\nprotocol = 'generate-content'\nbase_url = 'http://localhost:8000'\n",
    )
    .unwrap();
    client.auth_set("custom", KEY.into()).await.unwrap();
    assert_eq!(
        state(client.auth_status().await.unwrap(), "custom"),
        Availability::Ready
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
    assert!(matches!(
        state(status.clone(), "openai"),
        Availability::Invalid { .. }
    ));
    assert_eq!(state(status.clone(), "custom"), Availability::Ready);
    assert!(!serde_json::to_string(&status).unwrap().contains(KEY));
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
    assert!(client.auth_remove("openai").await.unwrap().removed);
    assert!(!client.auth_remove("openai").await.unwrap().removed);
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
        let server = spawn_test_server(layout.clone(), home);
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
            Response::AuthRemoved(local::AuthRemoval {
                removed: false,
                revocation: None,
            }),
            serde_json::json!({"response":"auth_removed","value":{"removed":false}}),
        ),
        (
            Response::AuthLoggedOut(local::AuthLoggedOut {
                revocation: local::RevocationStatus::Confirmed,
            }),
            serde_json::json!({"response":"auth_logged_out","value":{"revocation":"confirmed"}}),
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
    for wire in [
        serde_json::json!({"response":"auth_removed","value":{"removed":false,"extra":true}}),
        serde_json::json!({"response":"auth_logged_out","value":{"revocation":"confirmed","extra":true}}),
    ] {
        assert!(serde_json::from_value::<Response>(wire).is_err());
    }
}
