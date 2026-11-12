// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn private_socket_rejects_second_owner_and_oversized_frame() {
    let directory = tempfile::tempdir().unwrap();
    let layout = crate::StateLayout::in_state_root(directory.path());
    let (instance, listener) = ipc::Instance::bind(&layout).unwrap();
    assert!(matches!(
        ipc::Instance::bind(&layout),
        Err(LocalError {
            kind: LocalErrorKind::AlreadyRunning,
            ..
        })
    ));
    let path = lattice_local::test_support::endpoint(&layout).unwrap();
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let (mut writer, mut reader) = tokio::net::UnixStream::pair().unwrap();
    ipc::check_peer(&writer).unwrap();
    writer
        .write_all(lattice_local::test_support::FRAME_MAGIC)
        .await
        .unwrap();
    writer.write_u32((ipc::MAX_FRAME + 1) as u32).await.unwrap();
    let result = ipc::read::<serde_json::Value>(&mut reader).await;
    assert!(matches!(
        result,
        Err(LocalError {
            kind: LocalErrorKind::Protocol,
            ..
        })
    ));
    drop(listener);
    instance.cleanup().unwrap();
    drop(instance);
    std::os::unix::fs::symlink("/dev/null", path).unwrap();
    assert!(matches!(
        ipc::Instance::bind(&layout),
        Err(LocalError {
            kind: LocalErrorKind::Permission,
            ..
        })
    ));
}

pub(super) fn configure(project: &std::path::Path, endpoint: &str) {
    let path = project.join(".lattice/agents/local-test/agent.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "schema_version = 1\n[agent]\nname = 'local-test'\nmodel = 'local/local-test'\n[prompt]\nsystem = 'test'\n").unwrap();
    let directory = project.join(".lattice");
    std::fs::set_permissions(
        &directory,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
    )
    .unwrap();
    std::fs::write(
        directory.join("providers.toml"),
        format!(
            "[providers.local]\nprotocol='chat-completions'\nbase_url='{endpoint}'\nauth='bearer'\n"
        ),
    )
    .unwrap();
    lattice_llm::auth::AuthStore::new(directory)
        .set("local", "local-test-secret")
        .unwrap();
}

// 单模型配置诊断与告警走同一脱敏：已知凭据不能随模型列表跨过后台边界。
#[test]
fn model_candidates_redact_known_credentials_in_config_error_and_warnings() {
    const SECRET: &str = "known-model-secret-value";
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(
        directory.path(),
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
    )
    .unwrap();
    let auth = lattice_llm::auth::AuthStore::new(directory.path().into());
    auth.set("openai", SECRET).unwrap();
    let mut candidates: lattice_llm::ModelCandidates = serde_json::from_value(serde_json::json!({
        "models":[
            {"provider":"local", "model":"broken", "name":null, "context":null, "output":null,
             "supported_efforts":[], "cost":null,
             "availability":{"status":"invalid", "reason":format!("model \"broken\" key={SECRET}: invalid effort")}},
            {"provider":"local", "model":"good", "name":null, "context":null, "output":null,
             "supported_efforts":[], "cost":null, "availability":{"status":"ready"}}
        ],
        "warnings":[{"provider":"local", "message":format!("catalog key={SECRET}")}]
    }))
    .unwrap();
    server::redact_model_candidates(&mut candidates, &auth);
    assert_eq!(
        candidates.models[0].availability,
        lattice_llm::Availability::Invalid {
            reason: "model \"broken\" key=[REDACTED]: invalid effort".into()
        }
    );
    assert_eq!(
        candidates.models[1].availability,
        lattice_llm::Availability::Ready
    );
    assert_eq!(candidates.warnings[0].message, "catalog key=[REDACTED]");
    assert!(!serde_json::to_string(&candidates).unwrap().contains(SECRET));
}
