// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::AtomicUsize;

#[tokio::test]
async fn model_query_fetches_only_a_missing_signed_in_account_catalog_and_preserves_other_providers()
 {
    let home = tempfile::tempdir().unwrap();
    let directory = home.path().join(".lattice");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let auth_path = directory.join("auth.json");
    std::fs::write(&auth_path, serde_json::json!({"version":1,"providers":{"chatgpt":{
        "type":"chatgpt","host_id":"urn:uuid:2fde08e4-7b61-47cc-adde-8fb8b05f5b31",
        "client_id":"issued-client","account":{"subject":"subject","email":"user@example.com"},
        "tokens":{"access_token":"directory-access-secret","refresh_token":"directory-refresh-secret","id_token":"directory-id-secret",
            "expires_at":(chrono::Utc::now() + chrono::TimeDelta::hours(1)).to_rfc3339(),"scopes":["chatgpt.tokens.use.direct"]}
    }}}).to_string()).unwrap();
    std::fs::set_permissions(&auth_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let layout = crate::StateLayout::in_state_root(home.path());
    let mut backend = Backend::with_tls_roots(
        layout,
        Some(home.path().into()),
        vec![],
        Arc::new(crate::NoTools),
    );
    let gateway = backend.gateway.as_ref().unwrap().clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let fail = Arc::new(AtomicBool::new(false));
    Arc::get_mut(&mut backend).unwrap().metadata =
        super::super::metadata::MetadataRefresh::with_sources(
            None,
            {
                let calls = calls.clone();
                let fail = fail.clone();
                let directory = directory.clone();
                move |source, _| {
                    assert_eq!(source, super::super::metadata::Source::Chatgpt);
                    calls.fetch_add(1, Ordering::SeqCst);
                    let directory = directory.clone();
                    let fail = fail.load(Ordering::SeqCst);
                    Box::pin(async move {
                        if fail {
                            return Err(LocalError::new(
                                LocalErrorKind::Transport,
                                "directory unavailable; directory-access-secret",
                            ));
                        }
                        std::fs::create_dir_all(directory.join("cache")).unwrap();
                        std::fs::write(directory.join("cache/chatgpt-models.json"), serde_json::json!({"version":1,"fetched_at":chrono::Utc::now().to_rfc3339(),"models":[{"slug":"account-model","display_name":"Account model"}]}).to_string()).unwrap();
                        Ok(1)
                    })
                }
            },
            move || {
                let gateway = gateway.clone();
                Box::pin(async move {
                    Ok(
                        tokio::task::spawn_blocking(move || gateway.chatgpt_catalog_state())
                            .await
                            .unwrap()
                            .unwrap(),
                    )
                })
            },
        );
    for query in [
        ModelCandidateQuery::Target(ModelRef::parse("chatgpt/outside").unwrap()),
        ModelCandidateQuery::List {
            provider: Some(ProviderId::parse("openai").unwrap()),
            ready_only: false,
        },
    ] {
        backend.dispatch(Request::Models { query }).await.unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    for _ in 0..2 {
        let Response::Models(result) = backend
            .dispatch(Request::Models {
                query: ModelCandidateQuery::List {
                    provider: Some(ProviderId::parse("chatgpt").unwrap()),
                    ready_only: false,
                },
            })
            .await
            .unwrap()
        else {
            panic!("模型结果缺失");
        };
        assert_eq!(result.models.len(), 1);
        assert_eq!(result.models[0].model, "account-model");
        assert_eq!(result.models[0].name.as_deref(), Some("Account model"));
        assert!(result.warnings.is_empty());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let cache = directory.join("cache/chatgpt-models.json");
    std::fs::remove_file(&cache).unwrap();
    fail.store(true, Ordering::SeqCst);
    let Response::Models(result) = backend
        .dispatch(Request::Models {
            query: ModelCandidateQuery::List {
                provider: None,
                ready_only: false,
            },
        })
        .await
        .unwrap()
    else {
        panic!("目录失败不能中断其他 provider");
    };
    assert!(result.models.iter().any(|model| model.provider == "openai"));
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.provider.as_deref() == Some("chatgpt")
                && warning.message.contains("directory unavailable"))
    );
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("directory-access-secret")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    std::fs::remove_file(auth_path).unwrap();
    backend
        .dispatch(Request::Models {
            query: ModelCandidateQuery::List {
                provider: Some(ProviderId::parse("chatgpt").unwrap()),
                ready_only: false,
            },
        })
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2, "未登录不联网");
    backend.metadata.stop();
    backend.metadata.drain().await;
}
