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
        while backend.has_active_work() {
            changed.changed().await.unwrap();
        }
    })
    .await;
}

async fn wait_cleanup(backend: &Backend) {
    let mut changed = backend.changed.subscribe();
    bounded(async {
        while backend.has_cleanups() {
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

async fn seed_draft(
    backend: &Backend,
    workspace: &std::path::Path,
    sid: SessionId,
    updated_at: lattice_session::store::Timestamp,
) {
    let root = crate::LocalWorkspaceRoot::from_path(workspace).unwrap();
    let project_path = root.project_path();
    let directory = super::super::auth::directory(backend.home.as_deref()).unwrap();
    let agent_name = AgentName::parse("local-test").unwrap();
    let loaded = crate::profile::load_for_turn(&agent_name, &root, &directory).unwrap();
    let (settings, _) = crate::configure::resolve_settings(
        crate::configure::SettingsBase::Agent(crate::configure::LoadedAgent {
            profile: crate::profile::TurnProfile::from_loaded(&loaded),
            source_path: loaded.path,
        }),
        None,
        None,
        |target| lattice_llm::model_supported_efforts(&directory, target),
    )
    .unwrap();
    let api = backend.stores.acquire(true).await.unwrap();
    tokio::task::spawn_blocking(move || {
        let lock = api.store.try_lock_session(sid).unwrap();
        api.store
            .create_session(&lock, project_path, settings, updated_at)
            .unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cleanup_drafts_after_configure_new_respects_expiry_update_and_outcome() {
    let (dir, backend, client, task) = backend().await;
    let provider = tests::Provider::start().await;
    super::super::tests::configure(dir.path(), &provider.endpoint);
    let workspace = canonical_dir(dir.path());

    // 会话库未初始化时，被拒绝的 configure(New) 应跳过清理且不创建会话库。
    let pre_init_rejected = client
        .configure(ConfigureRequest {
            session: ConfigureSession::New(super::super::new_session_id()),
            workspace: workspace.clone(),
            agent: None,
            model: None,
            effort: None,
        })
        .await
        .expect_err("缺少 agent 的 configure(New) 应被拒绝");
    assert_eq!(pre_init_rejected.kind, LocalErrorKind::BadRequest);
    idle(&backend).await;
    assert!(
        !backend.layout.lattice_root().join("lattice.redb").exists(),
        "会话库未初始化时触发清理不应创建会话库"
    );

    let base_now = chrono::Utc::now();
    let expired_a = super::super::new_session_id();
    let fresh = super::super::new_session_id();
    let postponed = super::super::new_session_id();
    seed_draft(
        &backend,
        &workspace,
        expired_a,
        base_now - chrono::Duration::hours(25),
    )
    .await;
    seed_draft(
        &backend,
        &workspace,
        fresh,
        base_now - chrono::Duration::hours(23),
    )
    .await;
    seed_draft(
        &backend,
        &workspace,
        postponed,
        base_now - chrono::Duration::hours(25),
    )
    .await;

    // configure(Existing) 刷新草稿的 updated_at 推迟过期，且自身不触发清理。
    client
        .configure(ConfigureRequest {
            session: ConfigureSession::Existing(postponed),
            workspace: workspace.clone(),
            agent: None,
            model: Some(ModelRef::parse("local/updated-model").unwrap()),
            effort: None,
        })
        .await
        .expect("configure(Existing) 更新草稿设置应成功");
    idle(&backend).await;
    assert!(
        client.session_head(expired_a).await.is_ok(),
        "configure(Existing) 不应触发草稿清理"
    );

    // 成功的 configure(New) 结束后应删除过期无观察者草稿，保留未过期与修改过设置的草稿。
    let trigger_new = super::super::new_session_id();
    client
        .configure(ConfigureRequest {
            session: ConfigureSession::New(trigger_new),
            workspace: workspace.clone(),
            agent: Some(AgentName::parse("local-test").unwrap()),
            model: None,
            effort: None,
        })
        .await
        .expect("configure(New) 应成功");
    idle(&backend).await;

    assert_eq!(
        client
            .session_head(expired_a)
            .await
            .expect_err("过期且无观察者的草稿应被清理")
            .kind,
        LocalErrorKind::NotFound,
        "已清理草稿查询会话头应返回 NotFound"
    );
    assert!(
        client.session_head(fresh).await.is_ok(),
        "未过期的草稿应保留"
    );
    assert!(
        client.session_head(postponed).await.is_ok(),
        "用 configure(Existing) 修改过设置的草稿应推迟过期并保留"
    );

    // configure(New) 返回 SessionExists 后也应触发清理。
    let expired_b = super::super::new_session_id();
    seed_draft(
        &backend,
        &workspace,
        expired_b,
        chrono::Utc::now() - chrono::Duration::hours(25),
    )
    .await;
    let exists_err = client
        .configure(ConfigureRequest {
            session: ConfigureSession::New(trigger_new),
            workspace: workspace.clone(),
            agent: Some(AgentName::parse("local-test").unwrap()),
            model: None,
            effort: None,
        })
        .await
        .expect_err("已存在会话的 configure(New) 应返回冲突");
    assert_eq!(exists_err.conflict, Some(ConflictReason::SessionExists));
    idle(&backend).await;
    assert_eq!(
        client
            .session_head(expired_b)
            .await
            .expect_err("SessionExists 之后仍应清理过期草稿")
            .kind,
        LocalErrorKind::NotFound,
        "configure(New) 返回 SessionExists 后过期草稿应被清理"
    );

    // configure(New) 被拒绝后也应触发清理。
    let expired_c = super::super::new_session_id();
    seed_draft(
        &backend,
        &workspace,
        expired_c,
        chrono::Utc::now() - chrono::Duration::hours(25),
    )
    .await;
    let rejected_err = client
        .configure(ConfigureRequest {
            session: ConfigureSession::New(super::super::new_session_id()),
            workspace,
            agent: None,
            model: None,
            effort: None,
        })
        .await
        .expect_err("缺少 agent 的 configure(New) 应被拒绝");
    assert_eq!(rejected_err.kind, LocalErrorKind::BadRequest);
    idle(&backend).await;
    assert_eq!(
        client
            .session_head(expired_c)
            .await
            .expect_err("configure(New) 被拒绝后仍应清理过期草稿")
            .kind,
        LocalErrorKind::NotFound,
        "configure(New) 被拒绝后过期草稿应被清理"
    );

    let _ = provider.finish().await;
    finish(&backend, task).await;
}

#[tokio::test]
async fn cleanup_drafts_skips_watched_draft_until_watch_disconnects() {
    let (dir, backend, client, task) = backend().await;
    let provider = tests::Provider::start().await;
    super::super::tests::configure(dir.path(), &provider.endpoint);
    let workspace = canonical_dir(dir.path());

    let expired = super::super::new_session_id();
    seed_draft(
        &backend,
        &workspace,
        expired,
        chrono::Utc::now() - chrono::Duration::hours(25),
    )
    .await;

    let mut watcher = client.watch(expired).await.expect("建立草稿 watch 应成功");
    let Some(ViewUpdate::Head(head)) = bounded(watcher.next()).await.unwrap() else {
        panic!("watch 首帧应为草稿会话头");
    };
    assert_eq!(head.session_id, expired);

    client
        .configure(ConfigureRequest {
            session: ConfigureSession::New(super::super::new_session_id()),
            workspace: workspace.clone(),
            agent: Some(AgentName::parse("local-test").unwrap()),
            model: None,
            effort: None,
        })
        .await
        .expect("首次触发 configure(New) 应成功");
    idle(&backend).await;
    assert!(
        client.session_head(expired).await.is_ok(),
        "有活跃 watch 的过期草稿不应被删除"
    );

    let (watch_exited, release_watch_exit) = tests::arm(&backend.gates.watch_done);
    drop(watcher);
    bounded(watch_exited).await.unwrap();
    release_watch_exit.send(()).unwrap();

    client
        .configure(ConfigureRequest {
            session: ConfigureSession::New(super::super::new_session_id()),
            workspace,
            agent: Some(AgentName::parse("local-test").unwrap()),
            model: None,
            effort: None,
        })
        .await
        .expect("watch 断开后再次 configure(New) 应成功");
    idle(&backend).await;
    assert_eq!(
        client
            .session_head(expired)
            .await
            .expect_err("watch 断开后再次触发清理应删除该草稿")
            .kind,
        LocalErrorKind::NotFound,
        "watch 断开后下一次 configure(New) 应删除过期草稿"
    );

    let _ = provider.finish().await;
    finish(&backend, task).await;
}

#[tokio::test]
async fn cleanup_drafts_skips_when_session_slot_or_execution_lock_is_busy() {
    let (dir, backend, client, task) = backend().await;
    let provider = tests::Provider::start().await;
    super::super::tests::configure(dir.path(), &provider.endpoint);
    let workspace = canonical_dir(dir.path());

    let slot_busy = super::super::new_session_id();
    let lock_busy = super::super::new_session_id();
    seed_draft(
        &backend,
        &workspace,
        slot_busy,
        chrono::Utc::now() - chrono::Duration::hours(25),
    )
    .await;
    seed_draft(
        &backend,
        &workspace,
        lock_busy,
        chrono::Utc::now() - chrono::Duration::hours(25),
    )
    .await;

    // 占用 slot_busy 的会话 Owner 槽（停在接受首轮之前，会话在库中仍是草稿）。
    let (reg_entered, reg_release) = tests::arm(&backend.gates.registered);
    let submit_client = client.clone();
    let submit_workspace = workspace.clone();
    let submitting = tokio::spawn(async move {
        submit_client
            .submit(TurnRequest {
                turn_id: super::super::new_turn_id(),
                session: slot_busy,
                workspace: submit_workspace,
                input: "占住会话槽".into(),
                expected_settings: None,
            })
            .await
    });
    bounded(reg_entered).await.unwrap();

    // 直接占用 lock_busy 的文件执行锁。
    let api = backend.stores.acquire(false).await.unwrap();
    let held_lock = api
        .store
        .try_lock_session(lock_busy)
        .expect("直接获取候选草稿的执行锁应成功");
    drop(api);

    client
        .configure(ConfigureRequest {
            session: ConfigureSession::New(super::super::new_session_id()),
            workspace: workspace.clone(),
            agent: Some(AgentName::parse("local-test").unwrap()),
            model: None,
            effort: None,
        })
        .await
        .expect("占槽与占锁期间 configure(New) 应成功返回");
    wait_cleanup(&backend).await;

    let api = backend.stores.acquire(false).await.unwrap();
    let (slot_exists, lock_exists) = api
        .read(move |read| {
            Ok((
                read.find_session(slot_busy)?.is_some(),
                read.find_session(lock_busy)?.is_some(),
            ))
        })
        .await
        .unwrap();
    drop(api);
    assert!(slot_exists, "会话槽被占用时清理应立即跳过并保留会话");
    assert!(lock_exists, "执行锁被占用时清理应立即跳过并保留会话");

    drop(held_lock);
    reg_release.send(()).unwrap();
    bounded(submitting)
        .await
        .unwrap()
        .expect("放行后的提交应被接受");
    idle(&backend).await;

    client
        .configure(ConfigureRequest {
            session: ConfigureSession::New(super::super::new_session_id()),
            workspace,
            agent: Some(AgentName::parse("local-test").unwrap()),
            model: None,
            effort: None,
        })
        .await
        .expect("释放锁后再次 configure(New) 应成功");
    idle(&backend).await;

    assert_eq!(
        client
            .session_head(lock_busy)
            .await
            .expect_err("执行锁释放后再次触发清理应删除过期草稿")
            .kind,
        LocalErrorKind::NotFound,
        "执行锁释放后的过期草稿应被清理"
    );
    assert!(
        client.session_head(slot_busy).await.is_ok(),
        "已完成提交的会话不再是草稿，应继续保留"
    );

    let _ = provider.finish().await;
    finish(&backend, task).await;
}

#[tokio::test]
async fn cleanup_drafts_and_watch_registration_serialize_in_both_orders() {
    let (dir, backend, client, task) = backend().await;
    let provider = tests::Provider::start().await;
    super::super::tests::configure(dir.path(), &provider.endpoint);
    let workspace = canonical_dir(dir.path());

    // 顺序一：扫描后 watch 先完成登记，随后清理在准入锁内看到活跃观察者并跳过。
    let watch_first = super::super::new_session_id();
    seed_draft(
        &backend,
        &workspace,
        watch_first,
        chrono::Utc::now() - chrono::Duration::hours(25),
    )
    .await;
    let (scanned_in, scanned_out) = tests::arm(&backend.gates.cleanup_scanned);
    client
        .configure(ConfigureRequest {
            session: ConfigureSession::New(super::super::new_session_id()),
            workspace: workspace.clone(),
            agent: Some(AgentName::parse("local-test").unwrap()),
            model: None,
            effort: None,
        })
        .await
        .expect("触发第一次清理的 configure(New) 应成功");
    bounded(scanned_in).await.unwrap();

    let (cap_in, cap_out) = tests::arm(&backend.gates.capture_before);
    let watch_client = client.clone();
    let watching_first = tokio::spawn(async move {
        let mut w = watch_client.watch(watch_first).await?;
        Ok::<_, LocalError>((w.next().await?, w))
    });
    bounded(cap_in).await.unwrap();

    scanned_out.send(()).unwrap();
    wait_cleanup(&backend).await;
    cap_out.send(()).unwrap();

    let (first_frame, watcher_handle) = bounded(watching_first)
        .await
        .unwrap()
        .expect("watch 先登记时连接应正常交付首帧");
    assert!(
        matches!(first_frame, Some(ViewUpdate::Head(head)) if head.session_id == watch_first),
        "watch 先登记后应收到该草稿的会话头"
    );
    assert!(
        client.session_head(watch_first).await.is_ok(),
        "watch 先登记时清理必须跳过该草稿"
    );
    drop(watcher_handle);

    // 顺序二：清理先占位，watch 在准入锁边界看到无身份 Owner 后等待其结束并得到 NotFound。
    let cleanup_first = super::super::new_session_id();
    seed_draft(
        &backend,
        &workspace,
        cleanup_first,
        chrono::Utc::now() - chrono::Duration::hours(25),
    )
    .await;
    let (claimed_in, claimed_out) = tests::arm(&backend.gates.cleanup_claimed);
    let (waiting_in, waiting_out) = tests::arm(&backend.gates.watch_waiting_owner);
    client
        .configure(ConfigureRequest {
            session: ConfigureSession::New(super::super::new_session_id()),
            workspace,
            agent: Some(AgentName::parse("local-test").unwrap()),
            model: None,
            effort: None,
        })
        .await
        .expect("触发第二次清理的 configure(New) 应成功");
    bounded(claimed_in).await.unwrap();

    let watch_client = client.clone();
    let watching_second = tokio::spawn(async move {
        let mut w = watch_client.watch(cleanup_first).await?;
        w.next().await
    });
    bounded(waiting_in).await.unwrap();

    waiting_out.send(()).unwrap();
    claimed_out.send(()).unwrap();

    let watch_err = bounded(watching_second)
        .await
        .unwrap()
        .expect_err("清理先占位并删除草稿后，等待中的 watch 应返回错误");
    assert_eq!(
        watch_err.kind,
        LocalErrorKind::NotFound,
        "清理先占位删除会话后，watch 重读必须返回 NotFound"
    );
    idle(&backend).await;

    let _ = provider.finish().await;
    finish(&backend, task).await;
}

#[tokio::test]
async fn cleanup_drafts_keeps_session_accepted_between_scan_and_delete() {
    let (dir, backend, client, task) = backend().await;
    let provider = tests::Provider::start().await;
    super::super::tests::configure(dir.path(), &provider.endpoint);
    let workspace = canonical_dir(dir.path());

    let candidate = super::super::new_session_id();
    seed_draft(
        &backend,
        &workspace,
        candidate,
        chrono::Utc::now() - chrono::Duration::hours(25),
    )
    .await;

    let (scanned_in, scanned_out) = tests::arm(&backend.gates.cleanup_scanned);
    client
        .configure(ConfigureRequest {
            session: ConfigureSession::New(super::super::new_session_id()),
            workspace: workspace.clone(),
            agent: Some(AgentName::parse("local-test").unwrap()),
            model: None,
            effort: None,
        })
        .await
        .expect("触发清理的 configure(New) 应成功");
    bounded(scanned_in).await.unwrap();

    // 扫描已把 candidate 列为候选，但在清理占位删除前先让它接受并完成一轮。
    let accepted = client
        .submit(TurnRequest {
            turn_id: super::super::new_turn_id(),
            session: candidate,
            workspace,
            input: "扫描后接受首轮".into(),
            expected_settings: None,
        })
        .await
        .expect("候选草稿在清理前提交首轮应被接受");
    assert!(matches!(accepted, Acceptance::Accepted { .. }));
    let mut changed = backend.changed.subscribe();
    bounded(async {
        while backend.status().owners != 0 {
            changed.changed().await.unwrap();
        }
    })
    .await;

    scanned_out.send(()).unwrap();
    idle(&backend).await;

    let log = client
        .log(candidate)
        .await
        .expect("扫描后已接受轮次的会话不应被清理删除");
    assert_eq!(log.nodes.len(), 3, "扫描后已接受轮次的会话树节点应完整保留");

    let _ = provider.finish().await;
    finish(&backend, task).await;
}
