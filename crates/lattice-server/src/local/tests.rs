// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn private_socket_rejects_second_owner_and_oversized_frame() {
    let directory = tempfile::tempdir().unwrap();
    let layout = lattice_session::StateLayout::in_state_root(directory.path());
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

#[tokio::test]
async fn host_lifecycle_observes_ready_and_stopping_outside_admission_lock() {
    use std::sync::{Arc, Mutex};

    struct RecordingLifecycle {
        events: Arc<Mutex<Vec<&'static str>>>,
        fail_ready: bool,
    }

    impl HostLifecycle for RecordingLifecycle {
        fn on_ready(&self) -> Result<()> {
            self.events.lock().unwrap().push("ready");
            if self.fail_ready {
                return Err(LocalError::new(
                    LocalErrorKind::Internal,
                    "模拟就绪通知失败",
                ));
            }
            Ok(())
        }

        fn on_stopping(&self) {
            self.events.lock().unwrap().push("stopping");
        }
    }

    // 1. 正常启动再经停机通道收尾：先收到 ready，再收到一次 stopping（重复信号不重复发送）。
    let dir = tempfile::tempdir().unwrap();
    let layout = lattice_session::StateLayout::in_state_root(dir.path());
    let events = Arc::new(Mutex::new(Vec::new()));
    let (stop_tx, stop_rx) = tokio::sync::mpsc::channel(4);
    let serving_layout = layout.clone();
    let home = Some(dir.path().to_path_buf());
    let events_clone = events.clone();
    let handle = tokio::spawn(async move {
        serve(
            serving_layout,
            home,
            std::time::Duration::from_secs(60),
            stop_rx,
            RecordingLifecycle {
                events: events_clone,
                fail_ready: false,
            },
        )
        .await
    });
    let client = lattice_local::LocalClient::new(layout.clone());
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match client.status().await {
            Ok(_) => break,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Err(err) => panic!("后台未能在期限内就绪：{err:?}"),
        }
    }
    assert_eq!(events.lock().unwrap().as_slice(), ["ready"]);
    stop_tx.send(()).await.unwrap();
    let exit = handle.await.unwrap().unwrap();
    assert_eq!(exit.drain, DrainOutcome::Completed);
    assert!(exit.failure.is_none());
    assert_eq!(events.lock().unwrap().as_slice(), ["ready", "stopping"]);
    drop(exit);

    // 2. 就绪前已收到停机请求：不发 ready，只发一次 stopping，并正常完成收尾。
    let dir_pre = tempfile::tempdir().unwrap();
    let layout_pre = lattice_session::StateLayout::in_state_root(dir_pre.path());
    let events_pre = Arc::new(Mutex::new(Vec::new()));
    let (stop_pre_tx, stop_pre_rx) = tokio::sync::mpsc::channel(4);
    stop_pre_tx.send(()).await.unwrap();
    let exit_pre = serve(
        layout_pre,
        Some(dir_pre.path().to_path_buf()),
        std::time::Duration::from_secs(60),
        stop_pre_rx,
        RecordingLifecycle {
            events: events_pre.clone(),
            fail_ready: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(exit_pre.drain, DrainOutcome::Completed);
    assert!(exit_pre.failure.is_none());
    assert_eq!(events_pre.lock().unwrap().as_slice(), ["stopping"]);
    drop(exit_pre);

    // 3. READY=1 发送失败：受控收尾（撤下 socket、携带错误返回），且不补发 stopping。
    let dir_fail = tempfile::tempdir().unwrap();
    let layout_fail = lattice_session::StateLayout::in_state_root(dir_fail.path());
    let events_fail = Arc::new(Mutex::new(Vec::new()));
    let (_stop_fail_tx, stop_fail_rx) = tokio::sync::mpsc::channel(4);
    let exit_fail = serve(
        layout_fail.clone(),
        Some(dir_fail.path().to_path_buf()),
        std::time::Duration::from_secs(60),
        stop_fail_rx,
        RecordingLifecycle {
            events: events_fail.clone(),
            fail_ready: true,
        },
    )
    .await
    .unwrap();
    assert_eq!(exit_fail.drain, DrainOutcome::Completed);
    assert_eq!(
        exit_fail.failure.as_ref().map(|e| e.message.as_str()),
        Some("模拟就绪通知失败")
    );
    assert_eq!(events_fail.lock().unwrap().as_slice(), ["ready"]);
    assert!(
        !layout_fail.lattice_root().join("local/api.sock").exists(),
        "就绪通知失败收尾后必须撤下 api.sock"
    );
}

#[tokio::test]
async fn socket_cleanup_and_bind_errors_use_chinese_context_without_not_running() {
    use std::os::unix::fs::PermissionsExt;

    // 1. `--state-root` 指向普通文件
    let dir = tempfile::tempdir().unwrap();
    let plain_file = dir.path().join("plain-state-root");
    std::fs::write(&plain_file, b"not a directory").unwrap();
    let file_layout = lattice_session::StateLayout::in_state_root(&plain_file);
    let bind_file_err = match ipc::Instance::bind(&file_layout) {
        Ok(_) => panic!("普通文件作为状态根应绑定失败"),
        Err(err) => err,
    };
    assert_ne!(bind_file_err.kind, LocalErrorKind::NotRunning);
    assert!(
        bind_file_err
            .message
            .contains(&plain_file.display().to_string())
            && !bind_file_err.message.contains("local connection failed")
            && !bind_file_err.message.contains("后台未运行"),
        "状态根为普通文件时应指出路径并用中文说明原因，实际：{}",
        bind_file_err.message
    );

    // 2. `lattice/local` 权限为 0777
    let perm_dir = tempfile::tempdir().unwrap();
    let perm_layout = lattice_session::StateLayout::in_state_root(perm_dir.path());
    let local_dir = perm_layout.lattice_root().join("local");
    std::fs::create_dir_all(&local_dir).unwrap();
    std::fs::set_permissions(
        perm_layout.lattice_root(),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    std::fs::set_permissions(&local_dir, std::fs::Permissions::from_mode(0o777)).unwrap();
    let bind_perm_err = match ipc::Instance::bind(&perm_layout) {
        Ok(_) => panic!("0777 的 local 目录应绑定失败"),
        Err(err) => err,
    };
    assert_eq!(bind_perm_err.kind, LocalErrorKind::Permission);
    assert!(
        bind_perm_err.message.contains("0777")
            && !bind_perm_err
                .message
                .contains("local endpoint ownership or permissions rejected"),
        "0777 local 目录应报中文权限错误，实际：{}",
        bind_perm_err.message
    );

    // 3. 就绪后外部删掉 api.sock 再停机：failure 说明清理 socket 失败，且不报 NotRunning 或“后台未运行”
    let clean_dir = tempfile::tempdir().unwrap();
    let clean_layout = lattice_session::StateLayout::in_state_root(clean_dir.path());
    let (stop_tx, stop_rx) = tokio::sync::mpsc::channel(4);
    let serving_layout = clean_layout.clone();
    let home = Some(clean_dir.path().to_path_buf());
    let handle = tokio::spawn(async move {
        serve_for_test(
            serving_layout,
            home,
            std::time::Duration::from_secs(60),
            stop_rx,
        )
        .await
    });
    let client = lattice_local::LocalClient::new(clean_layout.clone());
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match client.status().await {
            Ok(_) => break,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Err(err) => panic!("后台未在期限内就绪：{err:?}"),
        }
    }
    let sock_path = clean_layout.lattice_root().join("local/api.sock");
    std::fs::remove_file(&sock_path).unwrap();
    stop_tx.send(()).await.unwrap();
    let exit = handle.await.unwrap().unwrap();
    assert_eq!(exit.drain, DrainOutcome::Completed);
    let failure = exit.failure.expect("删掉 api.sock 后停机应记录清理失败");
    assert_eq!(failure.kind, LocalErrorKind::Internal);
    assert!(
        failure.message.contains("清理 socket 失败")
            && failure.message.contains("api.sock")
            && !failure.message.contains("后台未运行"),
        "清理失败诊断不符：{}",
        failure.message
    );
}
