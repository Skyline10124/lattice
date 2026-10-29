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
    writer
        .write_all(super::test_support::FRAME_MAGIC)
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
