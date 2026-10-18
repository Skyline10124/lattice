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
    let path = ipc::endpoint(&layout).unwrap();
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let (mut writer, mut reader) = tokio::net::UnixStream::pair().unwrap();
    ipc::check_peer(&writer).unwrap();
    writer.write_all(b"LTLOCAL4").await.unwrap();
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
    std::os::unix::fs::symlink("/dev/null", path).unwrap();
    assert!(matches!(
        ipc::Instance::bind(&layout),
        Err(LocalError {
            kind: LocalErrorKind::Permission,
            ..
        })
    ));
}

pub(super) fn frozen(project: &std::path::Path, endpoint: &str) -> FrozenConfiguration {
    let path = project.join("agent.toml");
    std::fs::write(&path, "schema_version = 1\n[agent]\nname = 'local-test'\nmodel = 'compat:local-test'\n[prompt]\nsystem = 'test'\n").unwrap();
    let loaded = lattice_profile::load_agent_profile(&path).unwrap();
    let connection = lattice_llm::LlmConnection::new(
        lattice_llm::ResolvedModel {
            alias: "compat:local-test".into(),
            provider: lattice_llm::ProviderKind::OpenAiCompatible,
            provider_model: "local-test".into(),
            base_url: endpoint.into(),
            api_key_env: String::new(),
            credential_status: lattice_llm::CredentialStatus::Present,
        },
        "local-test-secret".into(),
        vec![],
    )
    .unwrap();
    FrozenConfiguration::capture(
        loaded,
        &crate::LocalWorkspaceRoot::from_path(project).unwrap(),
        connection,
    )
    .unwrap()
}
