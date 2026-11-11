// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("局部调度超时")
}

async fn backend() -> (
    tempfile::TempDir,
    Arc<Backend>,
    super::super::LocalClient,
    tokio::task::JoinHandle<()>,
) {
    let dir = tempfile::tempdir().unwrap();
    let layout = crate::StateLayout::in_state_root(dir.path());
    let (instance, listener) = ipc::Instance::bind(&layout).unwrap();
    let listener = UnixListener::from_std(listener).unwrap();
    let backend = Backend::with_tls_roots(
        layout.clone(),
        Some(dir.path().into()),
        vec![include_bytes!("test_root.der").to_vec()],
        Arc::new(crate::NoTools),
    );
    let server = backend.clone();
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        let mut closed = server.closed.subscribe();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (socket, _) = accepted.unwrap();
                    connections.spawn(connection(server.clone(), socket));
                }
                _ = connections.join_next(), if !connections.is_empty() => {},
                _ = closed.changed() => break,
            }
        }
        connections.abort_all();
        while connections.join_next().await.is_some() {}
        instance.cleanup().unwrap();
    });
    (dir, backend, super::super::LocalClient::new(layout), task)
}

async fn idle(backend: &Backend) {
    let mut changed = backend.changed.subscribe();
    bounded(async {
        while backend.status().owners != 0 {
            changed.changed().await.unwrap();
        }
    })
    .await;
}

async fn finish(backend: &Backend, task: tokio::task::JoinHandle<()>) {
    idle(backend).await;
    backend.closed.send_replace(true);
    bounded(task).await.unwrap();
}

fn canonical_dir(path: &std::path::Path) -> std::path::PathBuf {
    crate::LocalWorkspaceRoot::from_path(path)
        .unwrap()
        .canonical_path()
        .to_path_buf()
}

fn write_agent(project: &std::path::Path, name: &str, body: &str) {
    let path = project
        .join(".lattice/agents")
        .join(name)
        .join("agent.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

#[tokio::test]
async fn configure_new_existing_session_returns_session_exists() {
    let (dir, backend, client, task) = backend().await;
    let provider = tests::Provider::start().await;
    super::super::tests::configure(dir.path(), &provider.endpoint);
    let workspace = canonical_dir(dir.path());
    let sid = super::super::new_session_id();

    let created = client
        .configure(ConfigureRequest {
            session: ConfigureSession::New(sid),
            workspace: workspace.clone(),
            agent: Some(AgentName::parse("local-test").unwrap()),
            model: None,
            effort: None,
        })
        .await
        .expect("首次 configure(New) 应成功创建草稿会话");
    assert_eq!(created.head.session_id, sid);
    assert_eq!(created.head.position, NodeId(0));

    let duplicate = client
        .configure(ConfigureRequest {
            session: ConfigureSession::New(sid),
            workspace,
            agent: Some(AgentName::parse("local-test").unwrap()),
            model: None,
            effort: None,
        })
        .await
        .expect_err("重复 configure(New) 应被拒绝");
    assert_eq!(duplicate.kind, LocalErrorKind::Conflict);
    assert_eq!(
        duplicate.conflict,
        Some(ConflictReason::SessionExists),
        "重复 configure(New) 必须返回 SessionExists 冲突"
    );

    let _ = provider.finish().await;
    finish(&backend, task).await;
}

#[tokio::test]
async fn configure_existing_rejects_binding_mismatch() {
    let (dir, backend, client, task) = backend().await;
    let provider = tests::Provider::start().await;
    super::super::tests::configure(dir.path(), &provider.endpoint);
    let workspace = canonical_dir(dir.path());
    let other_dir = tempfile::tempdir().unwrap();
    let other_workspace = canonical_dir(other_dir.path());
    let sid = super::super::new_session_id();

    client
        .configure(ConfigureRequest {
            session: ConfigureSession::New(sid),
            workspace,
            agent: Some(AgentName::parse("local-test").unwrap()),
            model: None,
            effort: None,
        })
        .await
        .expect("创建会话应成功");

    let mismatch = client
        .configure(ConfigureRequest {
            session: ConfigureSession::Existing(sid),
            workspace: other_workspace,
            agent: None,
            model: Some(ModelRef::parse("local/other").unwrap()),
            effort: None,
        })
        .await
        .expect_err("跨工作区 configure(Existing) 应返回绑定不一致");
    assert_eq!(mismatch.kind, LocalErrorKind::Conflict);
    assert_eq!(
        mismatch.conflict,
        Some(ConflictReason::BindingMismatch),
        "项目绑定不一致时必须返回 BindingMismatch"
    );

    let _ = provider.finish().await;
    finish(&backend, task).await;
}

#[tokio::test]
async fn configure_during_running_turn_returns_busy() {
    let (dir, backend, client, task) = backend().await;
    let mut provider = tests::Provider::start().await;
    provider.release.send_replace(false);
    super::super::tests::configure(dir.path(), &provider.endpoint);
    let workspace = canonical_dir(dir.path());
    let sid = super::super::new_session_id();

    client
        .configure(ConfigureRequest {
            session: ConfigureSession::New(sid),
            workspace: workspace.clone(),
            agent: Some(AgentName::parse("local-test").unwrap()),
            model: None,
            effort: None,
        })
        .await
        .expect("创建会话应成功");

    client
        .submit(TurnRequest {
            turn_id: super::super::new_turn_id(),
            session: sid,
            workspace: workspace.clone(),
            input: "执行中".into(),
            expected_settings: None,
        })
        .await
        .expect("提交应被接受");
    bounded(&mut provider.arrived).await.unwrap();

    let busy_err = client
        .configure(ConfigureRequest {
            session: ConfigureSession::Existing(sid),
            workspace,
            agent: None,
            model: Some(ModelRef::parse("local/next").unwrap()),
            effort: None,
        })
        .await
        .expect_err("一轮执行期间 configure 必须返回 Busy");
    assert_eq!(
        busy_err.kind,
        LocalErrorKind::Busy,
        "执行中的会话修改设置必须返回 Busy"
    );

    provider.release.send_replace(true);
    idle(&backend).await;
    assert_eq!(provider.finish().await.len(), 1);
    finish(&backend, task).await;
}

#[tokio::test]
async fn configure_pushes_updated_head_to_multiple_watchers() {
    let (dir, backend, client, task) = backend().await;
    let provider = tests::Provider::start().await;
    super::super::tests::configure(dir.path(), &provider.endpoint);
    let workspace = canonical_dir(dir.path());
    let sid = super::super::new_session_id();

    let initial = client
        .configure(ConfigureRequest {
            session: ConfigureSession::New(sid),
            workspace: workspace.clone(),
            agent: Some(AgentName::parse("local-test").unwrap()),
            model: None,
            effort: None,
        })
        .await
        .expect("创建草稿会话应成功");

    let mut watcher_a = client.watch(sid).await.expect("观察者 A 连接应成功");
    let mut watcher_b = client.watch(sid).await.expect("观察者 B 连接应成功");

    let Some(ViewUpdate::Head(head_a0)) = bounded(watcher_a.next()).await.unwrap() else {
        panic!("观察者 A 首条消息应为初始会话头");
    };
    let Some(ViewUpdate::Head(head_b0)) = bounded(watcher_b.next()).await.unwrap() else {
        panic!("观察者 B 首条消息应为初始会话头");
    };
    assert_eq!(head_a0.settings_version, initial.head.settings_version);
    assert_eq!(head_b0.settings_version, initial.head.settings_version);

    let updated = client
        .configure(ConfigureRequest {
            session: ConfigureSession::Existing(sid),
            workspace,
            agent: None,
            model: Some(ModelRef::parse("local/updated-model").unwrap()),
            effort: None,
        })
        .await
        .expect("更新会话模型应成功");
    assert_ne!(
        updated.head.settings_version, initial.head.settings_version,
        "修改模型后 settings_version 应改变"
    );
    assert_eq!(
        updated.head.model.as_ref().map(|m| m.as_str()),
        Some("updated-model")
    );

    let Some(ViewUpdate::Head(head_a1)) = bounded(watcher_a.next()).await.unwrap() else {
        panic!("观察者 A 应收到 configure 后的新会话头");
    };
    let Some(ViewUpdate::Head(head_b1)) = bounded(watcher_b.next()).await.unwrap() else {
        panic!("观察者 B 应收到 configure 后的新会话头");
    };
    assert_eq!(head_a1.settings_version, updated.head.settings_version);
    assert_eq!(head_b1.settings_version, updated.head.settings_version);
    assert_eq!(head_a1.model, updated.head.model);
    assert_eq!(head_b1.model, updated.head.model);

    drop(watcher_a);
    drop(watcher_b);
    let _ = provider.finish().await;
    finish(&backend, task).await;
}

#[tokio::test]
async fn configure_succeeds_without_credentials_while_submit_rejects_missing_credential() {
    let (dir, backend, client, task) = backend().await;
    let provider = tests::Provider::start().await;
    super::super::tests::configure(dir.path(), &provider.endpoint);
    let removed = client.auth_remove("local").await.expect("移除凭据应成功");
    assert!(removed.removed);

    let workspace = canonical_dir(dir.path());
    let sid = super::super::new_session_id();
    let reply = client
        .configure(ConfigureRequest {
            session: ConfigureSession::New(sid),
            workspace: workspace.clone(),
            agent: Some(AgentName::parse("local-test").unwrap()),
            model: None,
            effort: None,
        })
        .await
        .expect("缺少凭据时 configure 仍应成功写入会话设置");
    assert_eq!(
        reply.head.provider.as_ref().map(|p| p.as_str()),
        Some("local")
    );
    assert_eq!(
        reply.head.model.as_ref().map(|m| m.as_str()),
        Some("local-test")
    );

    let rejection = client
        .submit(TurnRequest {
            turn_id: super::super::new_turn_id(),
            session: sid,
            workspace,
            input: "需要凭据".into(),
            expected_settings: Some(reply.head.settings_version),
        })
        .await
        .expect_err("缺少凭据时提交应在准入阶段被拒绝");
    assert_eq!(
        rejection.kind,
        LocalErrorKind::BadRequest,
        "凭据缺失应返回配置拒绝: {rejection:?}"
    );
    let persisted = client
        .session_head(sid)
        .await
        .expect("草稿会话与设置应保留");
    assert_eq!(persisted.settings_version, reply.head.settings_version);

    let _ = provider.finish().await;
    finish(&backend, task).await;
}

#[tokio::test]
async fn submit_with_mismatched_expected_settings_returns_settings_changed_distinct_from_turn_id_used()
 {
    let (dir, backend, client, task) = backend().await;
    let provider = tests::Provider::start().await;
    super::super::tests::configure(dir.path(), &provider.endpoint);
    let workspace = canonical_dir(dir.path());
    let sid = super::super::new_session_id();

    let initial = client
        .configure(ConfigureRequest {
            session: ConfigureSession::New(sid),
            workspace: workspace.clone(),
            agent: Some(AgentName::parse("local-test").unwrap()),
            model: None,
            effort: None,
        })
        .await
        .expect("创建会话应成功");

    let updated = client
        .configure(ConfigureRequest {
            session: ConfigureSession::Existing(sid),
            workspace: workspace.clone(),
            agent: None,
            model: Some(ModelRef::parse("local/second-model").unwrap()),
            effort: None,
        })
        .await
        .expect("更新会话模型应成功");
    assert_ne!(initial.head.settings_version, updated.head.settings_version);

    let stale_err = client
        .submit(TurnRequest {
            turn_id: super::super::new_turn_id(),
            session: sid,
            workspace: workspace.clone(),
            input: "旧设置提交".into(),
            expected_settings: Some(initial.head.settings_version),
        })
        .await
        .expect_err("过期的 expected_settings 应被拒绝");
    assert_eq!(stale_err.kind, LocalErrorKind::Conflict);
    assert_eq!(
        stale_err.conflict,
        Some(ConflictReason::SettingsChanged),
        "expected_settings 不符必须返回 SettingsChanged"
    );

    let accepted_tid = super::super::new_turn_id();
    let accepted = client
        .submit(TurnRequest {
            turn_id: accepted_tid,
            session: sid,
            workspace: workspace.clone(),
            input: "新设置提交".into(),
            expected_settings: Some(updated.head.settings_version),
        })
        .await
        .expect("匹配的 expected_settings 应被接受");
    assert!(matches!(accepted, Acceptance::Accepted { .. }));
    idle(&backend).await;

    let reused_err = client
        .submit(TurnRequest {
            turn_id: accepted_tid,
            session: sid,
            workspace,
            input: "重复 TurnId".into(),
            expected_settings: Some(updated.head.settings_version),
        })
        .await
        .expect_err("重复 TurnId 应被拒绝");
    assert_eq!(reused_err.kind, LocalErrorKind::Conflict);
    assert_eq!(
        reused_err.conflict,
        Some(ConflictReason::TurnIdUsed),
        "重复 TurnId 必须返回 TurnIdUsed，与 SettingsChanged 区分"
    );

    assert_eq!(provider.finish().await.len(), 1);
    finish(&backend, task).await;
}

#[tokio::test]
async fn submit_with_empty_settings_model_returns_configuration_rejection() {
    let (dir, backend, client, task) = backend().await;
    let provider = tests::Provider::start().await;
    super::super::tests::configure(dir.path(), &provider.endpoint);
    write_agent(
        dir.path(),
        "modeless",
        "schema_version = 1\n[agent]\nname = 'modeless'\n[prompt]\nsystem = 'no default model'\n",
    );
    let workspace = canonical_dir(dir.path());
    let sid = super::super::new_session_id();

    let created = client
        .configure(ConfigureRequest {
            session: ConfigureSession::New(sid),
            workspace: workspace.clone(),
            agent: Some(AgentName::parse("modeless").unwrap()),
            model: None,
            effort: None,
        })
        .await
        .expect("未配置默认模型的 agent 仍可创建草稿会话");
    assert_eq!(created.head.model, None);
    assert_eq!(created.head.provider, None);

    let err = client
        .submit(TurnRequest {
            turn_id: super::super::new_turn_id(),
            session: sid,
            workspace,
            input: "尝试发送".into(),
            expected_settings: Some(created.head.settings_version),
        })
        .await
        .expect_err("设置中的模型为空时提交应返回配置拒绝");
    assert_eq!(err.kind, LocalErrorKind::BadRequest);
    assert!(
        err.message.contains("/model") && err.message.contains("--model"),
        "空模型配置拒绝应提示先用 /model 或 --model 选择模型，实际为 {:?}",
        err.message
    );

    let _ = provider.finish().await;
    finish(&backend, task).await;
}
