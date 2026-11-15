// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 组件托管管理器、并发合并重新挂载、故障隔离、停机第 3 步交错与 IM IPC 集成测试。

use std::{
    os::unix::fs::PermissionsExt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use lattice_local::LocalClient;
use lattice_session::StateLayout;
use tokio::sync::{Notify, mpsc, oneshot};

use super::{
    ComponentBuildOutcome, ComponentRegistration, ComponentRemountHandle, PreparedComponent,
    install_component_panic_hook, serve_for_test_with_components,
    types::{
        ComponentState, ComponentStatus, ImCredentialStatus, ImMountResult, ImPlatformAuthStatus,
        LocalErrorKind,
    },
};

async fn wait_ready(client: &LocalClient) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match client.status().await {
            Ok(_) => return,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(err) => panic!("测试后台未能在期限内就绪：{err:?}"),
        }
    }
}

async fn wait_handles_idle(handles: &[ComponentRemountHandle]) {
    for handle in handles {
        handle.wait_idle_for_test().await;
    }
}

#[tokio::test]
async fn status_components_empty_when_disabled_and_sorted_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(dir.path());
    let (stop_tx, stop_rx) = mpsc::channel(4);

    let components = vec![
        ComponentRegistration::new("zeta", |_service| {
            ComponentBuildOutcome::Component(PreparedComponent::new(|stop| async move {
                stop.await;
                Ok(())
            }))
        }),
        ComponentRegistration::new("im", |_service| ComponentBuildOutcome::Disabled),
        ComponentRegistration::new("alpha", |_service| ComponentBuildOutcome::Unavailable {
            reason: "缺少凭据".into(),
        }),
        ComponentRegistration::new("beta", |_service| {
            ComponentBuildOutcome::InvalidConfiguration {
                reason: "配置文件损坏".into(),
            }
        }),
    ];
    let handles: Vec<_> = components
        .iter()
        .map(ComponentRegistration::remount_handle)
        .collect();

    let serve_layout = layout.clone();
    let home = Some(dir.path().to_path_buf());
    let handle = tokio::spawn(async move {
        serve_for_test_with_components(
            serve_layout,
            home,
            Duration::from_secs(60),
            stop_rx,
            components,
        )
        .await
    });

    let client = LocalClient::new(layout);
    wait_ready(&client).await;
    wait_handles_idle(&handles).await;

    let status = client.status().await.unwrap();
    assert_eq!(
        status.components,
        vec![
            ComponentStatus {
                name: "alpha".into(),
                state: ComponentState::Unmounted {
                    reason: "缺少凭据".into(),
                },
            },
            ComponentStatus {
                name: "beta".into(),
                state: ComponentState::Unmounted {
                    reason: "配置文件损坏".into(),
                },
            },
            ComponentStatus {
                name: "zeta".into(),
                state: ComponentState::Mounted {
                    configuration_error: None,
                },
            },
        ],
        "Disabled 组件不得出现在列表，其余组件必须按名称字典序排列"
    );

    stop_tx.send(()).await.unwrap();
    let exit = handle.await.unwrap().unwrap();
    assert!(exit.failure.is_none());
}

#[tokio::test]
async fn invalid_config_preserves_running_instance_or_unmounts_when_none_running() {
    #[derive(Clone)]
    enum Mode {
        PrepareFail(&'static str),
        Invalid(&'static str),
        Unavailable(&'static str),
        Disabled,
        Valid(Arc<AtomicBool>),
    }

    let dir = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(dir.path());
    let (stop_tx, stop_rx) = mpsc::channel(4);

    let mode = Arc::new(Mutex::new(Mode::Invalid("初始配置语法错误")));
    let mode_for_prepare = mode.clone();
    let mode_for_build = mode.clone();

    let reg = ComponentRegistration::new("im", move |_service| {
        match mode_for_build.lock().unwrap().clone() {
            Mode::PrepareFail(_) => unreachable!("prepare 失败时不应调用 build"),
            Mode::Invalid(reason) => ComponentBuildOutcome::InvalidConfiguration {
                reason: reason.into(),
            },
            Mode::Unavailable(reason) => ComponentBuildOutcome::Unavailable {
                reason: reason.into(),
            },
            Mode::Disabled => ComponentBuildOutcome::Disabled,
            Mode::Valid(stopped_flag) => {
                ComponentBuildOutcome::Component(PreparedComponent::new(move |stop| async move {
                    stop.await;
                    stopped_flag.store(true, Ordering::Release);
                    Ok(())
                }))
            }
        }
    })
    .with_prepare(
        move |_handle| match mode_for_prepare.lock().unwrap().clone() {
            Mode::PrepareFail(reason) => Err(reason.into()),
            _ => Ok(()),
        },
    );
    let remount_handle = reg.remount_handle();

    let serve_layout = layout.clone();
    let home = Some(dir.path().to_path_buf());
    let handle = tokio::spawn(async move {
        serve_for_test_with_components(
            serve_layout,
            home,
            Duration::from_secs(60),
            stop_rx,
            vec![reg],
        )
        .await
    });

    let client = LocalClient::new(layout);
    wait_ready(&client).await;
    remount_handle.wait_idle_for_test().await;

    // 1. 初始无运行实例时，配置无效列为 Unmounted。
    assert_eq!(
        client.status().await.unwrap().components,
        vec![ComponentStatus {
            name: "im".into(),
            state: ComponentState::Unmounted {
                reason: "初始配置语法错误".into(),
            },
        }]
    );

    // 2. 修复后重新挂载成功。
    let inst1_stopped = Arc::new(AtomicBool::new(false));
    *mode.lock().unwrap() = Mode::Valid(inst1_stopped.clone());
    assert_eq!(client.im_reload().await.unwrap(), ImMountResult::Mounted);
    assert_eq!(
        client.status().await.unwrap().components,
        vec![ComponentStatus {
            name: "im".into(),
            state: ComponentState::Mounted {
                configuration_error: None,
            },
        }]
    );

    // 3. 运行中配置写坏（以及 prepare 目录权限检查失败）：保留旧实例继续运行！
    *mode.lock().unwrap() = Mode::Invalid("workspace 路径不存在");
    assert_eq!(
        client.im_reload().await.unwrap(),
        ImMountResult::UsingPreviousConfiguration {
            reason: "workspace 路径不存在".into(),
        }
    );
    assert!(
        !inst1_stopped.load(Ordering::Acquire),
        "配置无效时绝不能停止正在运行的旧实例"
    );
    assert_eq!(
        client.status().await.unwrap().components,
        vec![ComponentStatus {
            name: "im".into(),
            state: ComponentState::Mounted {
                configuration_error: Some("workspace 路径不存在".into()),
            },
        }]
    );

    *mode.lock().unwrap() = Mode::PrepareFail("im 目录权限过宽");
    assert_eq!(
        client.im_reload().await.unwrap(),
        ImMountResult::UsingPreviousConfiguration {
            reason: "im 目录权限过宽".into(),
        }
    );
    assert!(
        !inst1_stopped.load(Ordering::Acquire),
        "宿主目录校验失败时同样保留正在运行的旧实例"
    );

    // 4. 切换为 Unavailable：属于有意停用前置缺失，正常停止旧实例并转为 Unmounted。
    *mode.lock().unwrap() = Mode::Unavailable("缺少 Telegram 凭据");
    assert_eq!(
        client.im_reload().await.unwrap(),
        ImMountResult::Unmounted {
            reason: "缺少 Telegram 凭据".into(),
        }
    );
    assert!(inst1_stopped.load(Ordering::Acquire));

    // 5. 切换为 Disabled：从 components 中移除。
    *mode.lock().unwrap() = Mode::Disabled;
    assert_eq!(client.im_reload().await.unwrap(), ImMountResult::Disabled);
    assert!(client.status().await.unwrap().components.is_empty());

    stop_tx.send(()).await.unwrap();
    let exit = handle.await.unwrap().unwrap();
    assert!(exit.failure.is_none());
}

#[tokio::test]
async fn remount_coalesces_concurrent_requests_into_single_followup_pass() {
    let dir = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(dir.path());
    let (stop_tx, stop_rx) = mpsc::channel(4);

    let build_count = Arc::new(AtomicUsize::new(0));
    let pass1_entered = Arc::new(Notify::new());
    let (pass1_release_tx, pass1_release_rx) = oneshot::channel::<()>();
    let pass1_gate = Arc::new(Mutex::new(Some(pass1_release_rx)));
    let current_state = Arc::new(Mutex::new("v0"));

    let build_count_cl = build_count.clone();
    let pass1_entered_cl = pass1_entered.clone();
    let pass1_gate_cl = pass1_gate.clone();
    let state_cl = current_state.clone();

    let reg = ComponentRegistration::new("im", move |_service| {
        let snapshot = *state_cl.lock().unwrap();
        let n = build_count_cl.fetch_add(1, Ordering::SeqCst) + 1;
        if n == 2 {
            // 第 2 次构造（即第一次 reload A）先读取 v1，再阻塞在闸门上等待并发请求 B/C/D 到达合并队列。
            pass1_entered_cl.notify_waiters();
            if let Some(rx) = pass1_gate_cl.lock().unwrap().take() {
                let _ = rx.blocking_recv();
            }
        }
        match snapshot {
            "v0" => ComponentBuildOutcome::Disabled,
            "v1" => ComponentBuildOutcome::Unavailable {
                reason: "第一轮读取结果".into(),
            },
            "v2" => ComponentBuildOutcome::Component(PreparedComponent::new(|stop| async move {
                stop.await;
                Ok(())
            })),
            _ => unreachable!(),
        }
    });

    let remount_handle = reg.remount_handle();
    let serve_layout = layout.clone();
    let home = Some(dir.path().to_path_buf());
    let handle = tokio::spawn(async move {
        serve_for_test_with_components(
            serve_layout,
            home,
            Duration::from_secs(60),
            stop_rx,
            vec![reg],
        )
        .await
    });

    let client = LocalClient::new(layout);
    wait_ready(&client).await;
    remount_handle.wait_idle_for_test().await;
    assert_eq!(build_count.load(Ordering::SeqCst), 1);

    // 启动 reload A（读取 v1 并在构造内暂停）。
    *current_state.lock().unwrap() = "v1";
    let client_a = client.clone();
    let reload_a = tokio::spawn(async move { client_a.im_reload().await.unwrap() });
    pass1_entered.notified().await;

    // 在 A 执行期间，将磁盘/配置状态切换为 v2，并并发发起 B、C、D 三个请求。
    *current_state.lock().unwrap() = "v2";
    let client_b = client.clone();
    let client_c = client.clone();
    let handle_d = remount_handle.clone();
    let reload_b = tokio::spawn(async move { client_b.im_reload().await.unwrap() });
    let reload_c = tokio::spawn(async move { client_c.im_reload().await.unwrap() });
    let reload_d = tokio::spawn(async move { handle_d.remount().await.unwrap() });

    // 确定性等待 B/C/D 全部进入合并等待队列后再放行 A（零固定时长睡眠）。
    tokio::time::timeout(
        Duration::from_secs(5),
        remount_handle.wait_pending_waiters_for_test(3),
    )
    .await
    .expect("B/C/D 三个并发请求应全部进入合并等待队列");
    let _ = pass1_release_tx.send(());

    let res_a = reload_a.await.unwrap();
    let (res_b, res_c, res_d) = tokio::join!(reload_b, reload_c, reload_d);

    assert_eq!(
        res_a,
        ImMountResult::Unmounted {
            reason: "第一轮读取结果".into(),
        }
    );
    assert_eq!(res_b.unwrap(), ImMountResult::Mounted);
    assert_eq!(res_c.unwrap(), ImMountResult::Mounted);
    assert_eq!(res_d.unwrap(), ImMountResult::Mounted);
    assert_eq!(
        build_count.load(Ordering::SeqCst),
        3,
        "初始挂载 1 次 + A 1 次 + B/C/D 合并后的 1 次 = 总计恰好 3 次构造"
    );

    stop_tx.send(()).await.unwrap();
    let exit = handle.await.unwrap().unwrap();
    assert!(exit.failure.is_none());
}

#[tokio::test(start_paused = true)]
async fn old_instance_exits_or_aborts_at_10s_before_new_instance_starts() {
    let dir = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(dir.path());
    let (stop_tx, stop_rx) = mpsc::channel(4);

    let generation = Arc::new(AtomicUsize::new(0));
    let inst1_alive = Arc::new(AtomicBool::new(false));
    let inst2_started = Arc::new(AtomicBool::new(false));
    let inst2_saw_inst1_alive = Arc::new(AtomicBool::new(false));

    let gen_cl = generation.clone();
    let inst1_alive_cl = inst1_alive.clone();
    let inst2_started_cl = inst2_started.clone();
    let inst2_saw_cl = inst2_saw_inst1_alive.clone();

    let reg = ComponentRegistration::new("im", move |_service| {
        let g = gen_cl.fetch_add(1, Ordering::SeqCst) + 1;
        if g == 1 {
            let alive = inst1_alive_cl.clone();
            ComponentBuildOutcome::Component(PreparedComponent::new(move |_stop| async move {
                struct AliveGuard(Arc<AtomicBool>);
                impl Drop for AliveGuard {
                    fn drop(&mut self) {
                        self.0.store(false, Ordering::Release);
                    }
                }
                alive.store(true, Ordering::Release);
                let _guard = AliveGuard(alive);
                // 故意忽略 stop 信号，永久挂起以验证 10 秒超时强制中止。
                std::future::pending::<()>().await;
                Ok(())
            }))
        } else {
            let alive = inst1_alive_cl.clone();
            let started = inst2_started_cl.clone();
            let saw = inst2_saw_cl.clone();
            ComponentBuildOutcome::Component(PreparedComponent::new(move |stop| async move {
                if alive.load(Ordering::Acquire) {
                    saw.store(true, Ordering::Release);
                }
                started.store(true, Ordering::Release);
                stop.await;
                Ok(())
            }))
        }
    });
    let remount_handle = reg.remount_handle();

    let serve_layout = layout.clone();
    let home = Some(dir.path().to_path_buf());
    let handle = tokio::spawn(async move {
        serve_for_test_with_components(
            serve_layout,
            home,
            Duration::from_secs(60),
            stop_rx,
            vec![reg],
        )
        .await
    });

    let client = LocalClient::new(layout);
    wait_ready(&client).await;
    remount_handle.wait_idle_for_test().await;
    tokio::task::yield_now().await;
    assert!(inst1_alive.load(Ordering::Acquire));

    let handle_reload = remount_handle.clone();
    let reload_task = tokio::spawn(async move { handle_reload.remount().await });

    // 推进 9 秒：旧实例尚未到 10 秒超时，新实例绝不能提前启动。
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(9)).await;
    tokio::task::yield_now().await;
    assert!(
        !reload_task.is_finished(),
        "未满 10 秒时重新挂载仍在等待旧实例"
    );
    assert!(
        !inst2_started.load(Ordering::Acquire),
        "旧实例退出前不得启动新实例"
    );

    // 再推进 1 秒到达精确 10 秒上限：旧实例被 abort 释放后新实例启动。
    tokio::time::advance(Duration::from_secs(1)).await;
    let res = reload_task.await.unwrap().unwrap();
    assert_eq!(res, ImMountResult::Mounted);
    assert!(inst2_started.load(Ordering::Acquire));
    assert!(
        !inst2_saw_inst1_alive.load(Ordering::Acquire),
        "新实例启动时旧实例必须已经彻底退出并完成 Drop"
    );

    stop_tx.send(()).await.unwrap();
    let exit = handle.await.unwrap().unwrap();
    assert!(exit.failure.is_none());
}

#[tokio::test]
async fn panic_and_error_isolation_redact_payload_and_never_claim_previous_config() {
    #[derive(Clone)]
    enum Action {
        PanicWithSecret,
        InvalidConfig,
        ReturnError,
        Healthy,
    }

    let dir = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(dir.path());
    let (stop_tx, stop_rx) = mpsc::channel(4);

    let action = Arc::new(Mutex::new(Action::PanicWithSecret));
    let trigger_panic = Arc::new(Notify::new());
    let action_cl = action.clone();
    let trigger_cl = trigger_panic.clone();

    let reg = ComponentRegistration::new("im", move |_service| {
        let current = action_cl.lock().unwrap().clone();
        match current {
            Action::PanicWithSecret => {
                let trigger = trigger_cl.clone();
                ComponentBuildOutcome::Component(PreparedComponent::new(move |_stop| async move {
                    trigger.notified().await;
                    panic!("UNREDACTED_SECRET_TOKEN_987654:ABCDEF");
                }))
            }
            Action::InvalidConfig => ComponentBuildOutcome::InvalidConfiguration {
                reason: "配置文件缺少必填项".into(),
            },
            Action::ReturnError => {
                ComponentBuildOutcome::Component(PreparedComponent::new(|_stop| async move {
                    Err("台账文件校验失败".into())
                }))
            }
            Action::Healthy => {
                ComponentBuildOutcome::Component(PreparedComponent::new(|stop| async move {
                    stop.await;
                    Ok(())
                }))
            }
        }
    });
    let remount_handle = reg.remount_handle();

    let serve_layout = layout.clone();
    let home = Some(dir.path().to_path_buf());
    let handle = tokio::spawn(async move {
        serve_for_test_with_components(
            serve_layout,
            home,
            Duration::from_secs(60),
            stop_rx,
            vec![reg],
        )
        .await
    });

    let client = LocalClient::new(layout);
    wait_ready(&client).await;
    remount_handle.wait_idle_for_test().await;

    // 触发组件 panic。
    trigger_panic.notify_waiters();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let panic_reason = loop {
        let status = client.status().await.unwrap();
        if let Some(ComponentStatus {
            state: ComponentState::Unmounted { reason },
            ..
        }) = status.components.first()
        {
            break reason.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "组件 panic 后应在期限内切换为 Unmounted"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert!(
        panic_reason.contains("panic"),
        "未挂载原因应明确包含 panic：{panic_reason}"
    );
    assert!(
        !panic_reason.contains("UNREDACTED_SECRET_TOKEN"),
        "严禁泄露原始 panic 载荷：{panic_reason}"
    );

    // 关键回归验证：旧实例已因 panic 死亡后，遇到无效配置绝不能谎称 UsingPreviousConfiguration！
    *action.lock().unwrap() = Action::InvalidConfig;
    assert_eq!(
        client.im_reload().await.unwrap(),
        ImMountResult::Unmounted {
            reason: "配置文件缺少必填项".into(),
        }
    );

    // 组件返回 Err(reason) 时同样隔离故障，后台照常服务。
    *action.lock().unwrap() = Action::ReturnError;
    assert_eq!(client.im_reload().await.unwrap(), ImMountResult::Mounted);
    let err_reason = loop {
        let status = client.status().await.unwrap();
        if let Some(ComponentStatus {
            state: ComponentState::Unmounted { reason },
            ..
        }) = status.components.first()
        {
            break reason.clone();
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(err_reason, "台账文件校验失败");

    // 随后执行 reload 恢复为健康挂载。
    *action.lock().unwrap() = Action::Healthy;
    assert_eq!(client.im_reload().await.unwrap(), ImMountResult::Mounted);
    assert_eq!(
        client.status().await.unwrap().components,
        vec![ComponentStatus {
            name: "im".into(),
            state: ComponentState::Mounted {
                configuration_error: None,
            },
        }]
    );

    stop_tx.send(()).await.unwrap();
    let exit = handle.await.unwrap().unwrap();
    assert!(exit.failure.is_none());
}

#[tokio::test(start_paused = true)]
async fn shutdown_during_remount_rejects_waiters_and_shares_single_10s_step3_deadline() {
    struct DropTimestamp(Arc<Mutex<Option<tokio::time::Instant>>>);
    impl Drop for DropTimestamp {
        fn drop(&mut self) {
            *self.0.lock().unwrap() = Some(tokio::time::Instant::now());
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(dir.path());
    let (stop_tx, stop_rx) = mpsc::channel(4);

    let im_gen = Arc::new(AtomicUsize::new(0));
    let candidate_started = Arc::new(AtomicBool::new(false));
    let im_retiring_notified = Arc::new(Notify::new());
    let im_dropped_at = Arc::new(Mutex::new(None));
    let aux_dropped_at = Arc::new(Mutex::new(None));

    let im_gen_cl = im_gen.clone();
    let candidate_started_cl = candidate_started.clone();
    let retiring_notify_cl = im_retiring_notified.clone();
    let im_dropped_cl = im_dropped_at.clone();

    let im_reg = ComponentRegistration::new("im", move |_service| {
        let g = im_gen_cl.fetch_add(1, Ordering::SeqCst) + 1;
        if g == 1 {
            let notify = retiring_notify_cl.clone();
            let dropped = im_dropped_cl.clone();
            ComponentBuildOutcome::Component(PreparedComponent::new(move |stop| async move {
                let _guard = DropTimestamp(dropped);
                stop.await;
                notify.notify_waiters();
                // 收到停止信号后挂起不退，模拟慢收尾旧实例。
                std::future::pending::<()>().await;
                Ok(())
            }))
        } else {
            let started = candidate_started_cl.clone();
            ComponentBuildOutcome::Component(PreparedComponent::new(move |stop| async move {
                started.store(true, Ordering::Release);
                stop.await;
                Ok(())
            }))
        }
    });

    // 第二个组件同样在停机时挂起，验证停机第 3 步所有组件共享同一个 10 秒上限。
    let aux_dropped_cl = aux_dropped_at.clone();
    let aux_reg = ComponentRegistration::new("aux", move |_service| {
        let dropped = aux_dropped_cl.clone();
        ComponentBuildOutcome::Component(PreparedComponent::new(move |_stop| async move {
            let _guard = DropTimestamp(dropped);
            std::future::pending::<()>().await;
            Ok(())
        }))
    });

    let remount_handle = im_reg.remount_handle();
    let aux_handle = aux_reg.remount_handle();

    let serve_layout = layout.clone();
    let home = Some(dir.path().to_path_buf());
    let handle = tokio::spawn(async move {
        serve_for_test_with_components(
            serve_layout,
            home,
            Duration::from_secs(60),
            stop_rx,
            vec![im_reg, aux_reg],
        )
        .await
    });

    let client = LocalClient::new(layout);
    wait_ready(&client).await;
    remount_handle.wait_idle_for_test().await;
    aux_handle.wait_idle_for_test().await;

    // 发起 reload A（进入等待旧实例收尾）与合并排队的 reload B（使用进程内句柄避免内核 UnixStream epoll 触发 Tokio 暂停时钟自动快进）。
    let handle_a = remount_handle.clone();
    let reload_a = tokio::spawn(async move { handle_a.remount().await });
    im_retiring_notified.notified().await;

    let handle_b = remount_handle.clone();
    let reload_b = tokio::spawn(async move { handle_b.remount().await });
    remount_handle.wait_pending_waiters_for_test(1).await;

    // 先在重新挂载中等待 6 秒（仍未满 10 秒），此时后台收到停机请求！
    tokio::time::advance(Duration::from_secs(6)).await;
    tokio::task::yield_now().await;
    assert!(!reload_a.is_finished());
    assert!(!reload_b.is_finished());

    let stop_started_at = tokio::time::Instant::now();
    stop_tx.send(()).await.unwrap();

    // 进行中的 reload A 与合并排队的 reload B 都必须立即（0ms 模拟时间）返回 Stopping。
    let err_a = reload_a.await.unwrap().unwrap_err();
    let err_b = reload_b.await.unwrap().unwrap_err();
    assert_eq!(err_a.kind, LocalErrorKind::Stopping);
    assert_eq!(err_b.kind, LocalErrorKind::Stopping);
    assert_eq!(
        stop_started_at.elapsed(),
        Duration::ZERO,
        "进入停机时在途与合并等待的重新挂载必须立即返回 Stopping"
    );

    // 停机期间再次请求重新挂载也立即返回 Stopping。
    let err_c = remount_handle.remount().await.unwrap_err();
    assert_eq!(err_c.kind, LocalErrorKind::Stopping);

    // 先推进 9 秒：两个组件均尚未到达第 3 步的 10 秒截止点。
    tokio::time::advance(Duration::from_secs(9)).await;
    tokio::task::yield_now().await;
    assert!(im_dropped_at.lock().unwrap().is_none());
    assert!(aux_dropped_at.lock().unwrap().is_none());

    // 再推进 1 秒到达第 3 步的单一 10 秒截止点（而非 6+10=16 秒或 10+1=11 秒）：两个挂起组件同时被中止且后台收尾不再附加等待。
    tokio::time::advance(Duration::from_secs(1)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }

    let im_elapsed = im_dropped_at
        .lock()
        .unwrap()
        .expect("im 组件应已在 10 秒截止点被中止释放")
        .duration_since(stop_started_at);
    let aux_elapsed = aux_dropped_at
        .lock()
        .unwrap()
        .expect("aux 组件应已在 10 秒截止点被中止释放")
        .duration_since(stop_started_at);
    assert_eq!(im_elapsed, Duration::from_secs(10));
    assert_eq!(aux_elapsed, Duration::from_secs(10));
    assert!(
        !candidate_started.load(Ordering::Acquire),
        "重新挂载期间进入停机后绝不能再启动新候选实例"
    );

    tokio::time::resume();
    let exit = handle.await.unwrap().unwrap();
    assert!(exit.failure.is_none());
}

#[tokio::test]
async fn im_auth_ipc_roundtrip_redacts_secrets_and_succeeds_with_unmounted_when_stopping() {
    let dir = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(dir.path());
    let (stop_tx, stop_rx) = mpsc::channel(4);

    let config_dir = dir.path().join(".lattice");
    let config_dir_for_build = config_dir.clone();
    let reg = ComponentRegistration::new("im", move |_service| {
        let store = crate::ImAuthStore::new(config_dir_for_build.clone());
        match store.get(crate::TELEGRAM_PLATFORM) {
            Ok(Some(_)) => {
                ComponentBuildOutcome::Component(PreparedComponent::new(|stop| async move {
                    stop.await;
                    Ok(())
                }))
            }
            Ok(None) => ComponentBuildOutcome::Unavailable {
                reason: "缺少 Telegram 凭据".into(),
            },
            Err(err) => ComponentBuildOutcome::InvalidConfiguration {
                reason: err.to_string(),
            },
        }
    });

    let step3_entered = Arc::new(Notify::new());
    let step3_release = Arc::new(Notify::new());
    let step3_entered_cl = step3_entered.clone();
    let step3_release_cl = step3_release.clone();
    let hold_reg = ComponentRegistration::new("hold_step3", move |_service| {
        let entered = step3_entered_cl.clone();
        let release = step3_release_cl.clone();
        ComponentBuildOutcome::Component(PreparedComponent::new(move |stop| async move {
            stop.await;
            entered.notify_waiters();
            release.notified().await;
            Ok(())
        }))
    });
    let handles = [reg.remount_handle(), hold_reg.remount_handle()];

    let serve_layout = layout.clone();
    let home = Some(dir.path().to_path_buf());
    let handle = tokio::spawn(async move {
        serve_for_test_with_components(
            serve_layout,
            home,
            Duration::from_secs(60),
            stop_rx,
            vec![reg, hold_reg],
        )
        .await
    });

    let client = LocalClient::new(layout);
    wait_ready(&client).await;
    wait_handles_idle(&handles).await;

    // 1. 初始状态为 Missing。
    let initial = client.im_auth_status().await.unwrap();
    assert_eq!(
        initial.platforms,
        vec![ImPlatformAuthStatus {
            platform: "telegram".into(),
            credential: ImCredentialStatus::Missing,
        }]
    );

    // 2. 非法平台与含控制字符的非法密钥均返回 BadRequest，且绝不回显平台参数或密钥明文。
    let secret_platform = "discord_secret_arg_42";
    let secret_token = "123456:SUPER_SECRET_BOT_TOKEN_XYZ";
    let bad_platform_err = client
        .im_auth_set(secret_platform, secret_token)
        .await
        .unwrap_err();
    assert_eq!(bad_platform_err.kind, LocalErrorKind::BadRequest);
    assert!(!bad_platform_err.message.contains(secret_platform));
    assert!(!bad_platform_err.message.contains(secret_token));

    let bad_token_marker = "CTRL_TOKEN_MARKER_999";
    let bad_token_value = format!("bad\x07{bad_token_marker}");
    let bad_token_err = client
        .im_auth_set("telegram", &bad_token_value)
        .await
        .unwrap_err();
    assert_eq!(bad_token_err.kind, LocalErrorKind::BadRequest);
    assert!(!bad_token_err.message.contains(bad_token_marker));

    // 3. 写入合法凭据：创建 0700 目录与 0600 auth.json，并自动触发 IM 重新挂载为 Mounted。
    let set_reply = client.im_auth_set("telegram", secret_token).await.unwrap();
    assert_eq!(set_reply.mount, ImMountResult::Mounted);
    let auth_path = config_dir.join("im/auth.json");
    let mode = std::fs::metadata(&auth_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "im/auth.json 权限必须为 0600");

    let present = client.im_auth_status().await.unwrap();
    assert_eq!(
        present.platforms,
        vec![ImPlatformAuthStatus {
            platform: "telegram".into(),
            credential: ImCredentialStatus::Present,
        }]
    );

    // 4. 删除凭据：首次 removed=true 并卸载为 Unmounted，再次删除 removed=false。
    let rm1 = client.im_auth_remove("telegram").await.unwrap();
    assert!(rm1.removed);
    assert_eq!(
        rm1.mount,
        ImMountResult::Unmounted {
            reason: "缺少 Telegram 凭据".into(),
        }
    );
    let rm2 = client.im_auth_remove("telegram").await.unwrap();
    assert!(!rm2.removed);

    // 5. 停机尾段期间执行 ImAuthSet / ImAuthRemove：凭据落盘成功后返回 Ok 响应并携带 Unmounted 说明（CLI 退出码为 0），绝不返回错误导致误报失败。
    stop_tx.send(()).await.unwrap();
    step3_entered.notified().await;

    let set_stopping = client
        .im_auth_set("telegram", secret_token)
        .await
        .expect("凭据写入成功后即使后台正在停止也应返回 Ok 响应");
    match &set_stopping.mount {
        ImMountResult::Unmounted { reason } => {
            assert!(
                reason.contains("后台正在停止"),
                "停机期间写入凭据应说明因停机未重新挂载：{reason}"
            );
            assert!(!reason.contains(secret_token));
        }
        other => panic!("停机期间写入凭据后 mount 应为 Unmounted，实际：{other:?}"),
    }
    assert_eq!(
        client.im_auth_status().await.unwrap().platforms[0].credential,
        ImCredentialStatus::Present
    );

    let rm_stopping = client
        .im_auth_remove("telegram")
        .await
        .expect("凭据删除成功后即使后台正在停止也应返回 Ok 响应");
    assert!(rm_stopping.removed);
    match &rm_stopping.mount {
        ImMountResult::Unmounted { reason } => {
            assert!(reason.contains("后台正在停止"));
        }
        other => panic!("停机期间删除凭据后 mount 应为 Unmounted，实际：{other:?}"),
    }
    let rm_stopping_again = client.im_auth_remove("telegram").await.unwrap();
    assert!(!rm_stopping_again.removed);
    assert_eq!(
        client.im_auth_status().await.unwrap().platforms[0].credential,
        ImCredentialStatus::Missing
    );

    step3_release.notify_waiters();
    let exit = handle.await.unwrap().unwrap();
    assert!(exit.failure.is_none());
}

#[tokio::test]
async fn blocked_initial_mount_does_not_block_status_or_shutdown_and_caps_step3_at_10s() {
    let dir = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(dir.path());
    let (stop_tx, stop_rx) = mpsc::channel(4);

    let build_entered = Arc::new(Notify::new());
    let (gate_tx, gate_rx) = oneshot::channel::<()>();
    let gate_holder = Arc::new(Mutex::new(Some(gate_rx)));

    let entered_cl = build_entered.clone();
    let gate_cl = gate_holder.clone();
    let reg = ComponentRegistration::new("im", move |_service| {
        entered_cl.notify_one();
        if let Some(rx) = gate_cl.lock().unwrap().take() {
            let _ = rx.blocking_recv();
        }
        ComponentBuildOutcome::Component(PreparedComponent::new(|stop| async move {
            stop.await;
            Ok(())
        }))
    });
    let remount_handle = reg.remount_handle();

    let serve_layout = layout.clone();
    let home = Some(dir.path().to_path_buf());
    let mut handle = tokio::spawn(async move {
        serve_for_test_with_components(
            serve_layout,
            home,
            Duration::from_secs(60),
            stop_rx,
            vec![reg],
        )
        .await
    });

    // 即使初始组件构造阻塞在阻塞线程池中，后台也已经进入 accept 循环并立即响应 Status 请求。
    let client = LocalClient::new(layout);
    wait_ready(&client).await;
    build_entered.notified().await;
    let status = client.status().await.unwrap();
    assert!(
        status.components.is_empty(),
        "初始挂载未完成前 Status 请求应正常返回且不阻塞"
    );

    // 暂停模拟时钟后发送停机信号：停机响应同样不被初始挂载阻塞，且第 3 步严格以 10 秒为上限（不叠加额外 1 秒）。
    tokio::time::pause();
    let stop_started = tokio::time::Instant::now();
    stop_tx.send(()).await.unwrap();
    tokio::task::yield_now().await;

    let err = remount_handle.remount().await.unwrap_err();
    assert_eq!(err.kind, LocalErrorKind::Stopping);
    assert_eq!(stop_started.elapsed(), Duration::ZERO);

    tokio::time::advance(Duration::from_secs(9)).await;
    tokio::task::yield_now().await;
    assert!(
        !handle.is_finished(),
        "未满 10 秒时后台仍在第 3 步等待在途初始挂载驱动"
    );

    tokio::time::advance(Duration::from_secs(1)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(stop_started.elapsed(), Duration::from_secs(10));

    // 在精确满 10 秒虚拟时间后恢复真实时钟，此时阻塞线程仍未放行（gate_tx 未发送），
    // 限定 500ms 真实耗时内后台必须中止在途挂载驱动并完成退场，证明第 3 步严格在 10 秒封顶且不附加 1 秒等待。
    tokio::time::resume();
    let exit = tokio::time::timeout(Duration::from_millis(500), &mut handle)
        .await
        .expect("停机第 3 步满 10 秒后必须立即中止在途挂载驱动并退场，不得再额外等待 1 秒")
        .unwrap()
        .unwrap();
    let _ = gate_tx.send(());
    assert!(exit.failure.is_none());
}

#[tokio::test]
async fn component_panic_payload_never_leaks_to_subprocess_stderr() {
    const SUBPROC_ENV: &str = "LATTICE_TEST_COMPONENT_PANIC_SUBPROCESS";
    const BUILD_SECRET: &str = "RAW_BUILD_PANIC_SECRET_112233:TOKEN";
    const RUN_SECRET: &str = "RAW_RUN_PANIC_SECRET_445566:TOKEN";
    const OUTSIDE_MARKER: &str = "NON_COMPONENT_PANIC_DELEGATES_TO_DEFAULT_HOOK";

    match std::env::var(SUBPROC_ENV).as_deref() {
        Ok("component") => {
            let filter = tracing_subscriber::EnvFilter::try_from_env("LATTICE_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("trace"));
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .with_ansi(false)
                .compact()
                .try_init();
            install_component_panic_hook();

            let dir = tempfile::tempdir().unwrap();
            let layout = StateLayout::in_state_root(dir.path());
            let (stop_tx, stop_rx) = mpsc::channel(4);

            let phase = Arc::new(AtomicUsize::new(0));
            let phase_cl = phase.clone();
            let reg = ComponentRegistration::new("im", move |_service| {
                let p = phase_cl.fetch_add(1, Ordering::SeqCst);
                if p == 0 {
                    panic!("{BUILD_SECRET}");
                } else {
                    ComponentBuildOutcome::Component(PreparedComponent::new(|_stop| async move {
                        panic!("{RUN_SECRET}");
                    }))
                }
            });
            let remount_handle = reg.remount_handle();

            let serve_layout = layout.clone();
            let home = Some(dir.path().to_path_buf());
            let handle = tokio::spawn(async move {
                serve_for_test_with_components(
                    serve_layout,
                    home,
                    Duration::from_secs(60),
                    stop_rx,
                    vec![reg],
                )
                .await
            });

            let client = LocalClient::new(layout);
            wait_ready(&client).await;
            remount_handle.wait_idle_for_test().await;

            // 第 0 轮：构造闭包发生 panic，状态应隔离为 Unmounted。
            let status0 = client.status().await.unwrap();
            assert_eq!(
                status0.components,
                vec![ComponentStatus {
                    name: "im".into(),
                    state: ComponentState::Unmounted {
                        reason: "组件任务发生 panic".into(),
                    },
                }]
            );

            // 第 1 轮：运行任务发生 panic，状态同样在期限内隔离为 Unmounted。
            assert_eq!(client.im_reload().await.unwrap(), ImMountResult::Mounted);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                let st = client.status().await.unwrap();
                if matches!(
                    st.components.first(),
                    Some(ComponentStatus {
                        state: ComponentState::Unmounted { reason },
                        ..
                    }) if reason.contains("panic")
                ) {
                    break;
                }
                assert!(tokio::time::Instant::now() < deadline);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }

            stop_tx.send(()).await.unwrap();
            let exit = handle.await.unwrap().unwrap();
            assert!(exit.failure.is_none());
            return;
        }
        Ok("outside") => {
            install_component_panic_hook();
            let _ = std::panic::catch_unwind(|| {
                panic!("{OUTSIDE_MARKER}");
            });
            return;
        }
        _ => {}
    }

    let exe = std::env::current_exe().unwrap();

    // 1. 子进程触发组件构造期与运行期 panic：stderr 必须输出脱敏位置日志且完全不包含原始 panic 载荷。
    let out_component = std::process::Command::new(&exe)
        .args([
            "--exact",
            "local::components_tests::component_panic_payload_never_leaks_to_subprocess_stderr",
            "--nocapture",
        ])
        .env(SUBPROC_ENV, "component")
        .env("LATTICE_LOG", "trace")
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let stdout_comp = String::from_utf8_lossy(&out_component.stdout);
    let stderr_comp = String::from_utf8_lossy(&out_component.stderr);
    assert!(
        out_component.status.success(),
        "组件 panic 隔离子进程应正常退出，stdout={stdout_comp}\nstderr={stderr_comp}"
    );
    for secret in [BUILD_SECRET, RUN_SECRET, "112233", "445566"] {
        assert!(
            !stdout_comp.contains(secret) && !stderr_comp.contains(secret),
            "组件 panic 原始载荷不得泄露到子进程输出（命中 {secret}）：\nstdout={stdout_comp}\nstderr={stderr_comp}"
        );
    }
    assert!(
        stderr_comp.contains("受托管组件执行期间发生 panic（已抑制原始载荷）"),
        "stderr 应包含脱敏后的组件 panic 告警日志，实际：{stderr_comp}"
    );

    // 2. 子进程在组件作用域之外发生 panic：仍交由原有 hook 输出，不误吞宿主自身 panic。
    let out_outside = std::process::Command::new(&exe)
        .args([
            "--exact",
            "local::components_tests::component_panic_payload_never_leaks_to_subprocess_stderr",
            "--nocapture",
        ])
        .env(SUBPROC_ENV, "outside")
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let stderr_out = String::from_utf8_lossy(&out_outside.stderr);
    assert!(out_outside.status.success());
    assert!(
        stderr_out.contains(OUTSIDE_MARKER),
        "组件作用域之外的 panic 应继续走原有 hook：{stderr_out}"
    );
}

#[tokio::test]
async fn im_auth_mutations_blocked_in_prepare_or_build_return_ok_unmounted_on_stop() {
    let secret_token = "123456:BLOCKED_BUILD_STOP_TOKEN_ABC";

    // 场景 A：ImAuthSet 写盘成功后卡在组件 build 阻塞池，同时并发 ImReload 排队；此时进入停机。
    {
        let dir = tempfile::tempdir().unwrap();
        let layout = StateLayout::in_state_root(dir.path());
        let config_dir = dir.path().join(".lattice");
        let (stop_tx, stop_rx) = mpsc::channel(4);

        let pass = Arc::new(AtomicUsize::new(0));
        let build_entered = Arc::new(Notify::new());
        let (gate_tx, gate_rx) = oneshot::channel::<()>();
        let gate_holder = Arc::new(Mutex::new(Some(gate_rx)));

        let pass_cl = pass.clone();
        let entered_cl = build_entered.clone();
        let gate_cl = gate_holder.clone();
        let reg = ComponentRegistration::new("im", move |_service| {
            let n = pass_cl.fetch_add(1, Ordering::SeqCst) + 1;
            if n == 2 {
                entered_cl.notify_one();
                if let Some(rx) = gate_cl.lock().unwrap().take() {
                    let _ = rx.blocking_recv();
                }
            }
            ComponentBuildOutcome::Disabled
        });
        let remount_handle = reg.remount_handle();

        let serve_layout = layout.clone();
        let home = Some(dir.path().to_path_buf());
        let mut handle = tokio::spawn(async move {
            serve_for_test_with_components(
                serve_layout,
                home,
                Duration::from_secs(60),
                stop_rx,
                vec![reg],
            )
            .await
        });

        let client = LocalClient::new(layout);
        wait_ready(&client).await;
        remount_handle.wait_idle_for_test().await;

        let client_set = client.clone();
        let set_task =
            tokio::spawn(async move { client_set.im_auth_set("telegram", secret_token).await });
        build_entered.notified().await;

        let client_reload = client.clone();
        let reload_task = tokio::spawn(async move { client_reload.im_reload().await });
        remount_handle.wait_pending_waiters_for_test(1).await;

        stop_tx.send(()).await.unwrap();

        let set_reply = tokio::time::timeout(Duration::from_secs(1), set_task)
            .await
            .expect("停机时卡在 build 的 ImAuthSet 必须立即返回，不得阻塞排空")
            .unwrap()
            .expect("凭据已落盘成功时绝不能因驱动中止而返回 Internal 错误");
        match &set_reply.mount {
            ImMountResult::Unmounted { reason } => {
                assert!(reason.contains("后台正在停止"), "实际原因：{reason}");
                assert!(!reason.contains(secret_token));
            }
            other => panic!("预期返回 Unmounted，实际：{other:?}"),
        }
        assert_eq!(
            crate::ImAuthStore::new(config_dir)
                .get("telegram")
                .unwrap()
                .as_ref()
                .map(crate::ImToken::as_str),
            Some(secret_token),
            "磁盘上的新凭据必须已持久保存"
        );

        let reload_err = tokio::time::timeout(Duration::from_secs(1), reload_task)
            .await
            .expect("并发排队的 ImReload 必须立即返回")
            .unwrap()
            .unwrap_err();
        assert_eq!(reload_err.kind, LocalErrorKind::Stopping);

        tokio::time::pause();
        assert!(
            !handle.is_finished(),
            "未满 10 秒时第 3 步仍在等待阻塞中的驱动任务"
        );
        tokio::time::advance(Duration::from_secs(10)).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        tokio::time::resume();
        let exit = tokio::time::timeout(Duration::from_millis(500), &mut handle)
            .await
            .expect("第 3 步满 10 秒后应中止驱动并完成停机")
            .unwrap()
            .unwrap();
        let _ = gate_tx.send(());
        assert!(exit.failure.is_none());
    }

    // 场景 B：ImAuthRemove 删盘成功后卡在组件 prepare 阻塞池；此时进入停机。
    {
        let dir = tempfile::tempdir().unwrap();
        let layout = StateLayout::in_state_root(dir.path());
        let config_dir = dir.path().join(".lattice");
        crate::ImAuthStore::new(config_dir.clone())
            .set("telegram", secret_token)
            .unwrap();
        let (stop_tx, stop_rx) = mpsc::channel(4);

        let prep_pass = Arc::new(AtomicUsize::new(0));
        let prepare_entered = Arc::new(Notify::new());
        let (gate_tx, gate_rx) = oneshot::channel::<()>();
        let gate_holder = Arc::new(Mutex::new(Some(gate_rx)));

        let prep_pass_cl = prep_pass.clone();
        let entered_cl = prepare_entered.clone();
        let gate_cl = gate_holder.clone();
        let reg = ComponentRegistration::new("im", |_service| {
            ComponentBuildOutcome::Component(PreparedComponent::new(|stop| async move {
                stop.await;
                Ok(())
            }))
        })
        .with_prepare(move |_handle| {
            let n = prep_pass_cl.fetch_add(1, Ordering::SeqCst) + 1;
            if n == 2 {
                entered_cl.notify_one();
                if let Some(rx) = gate_cl.lock().unwrap().take() {
                    let _ = rx.blocking_recv();
                }
            }
            Ok(())
        });
        let remount_handle = reg.remount_handle();

        let serve_layout = layout.clone();
        let home = Some(dir.path().to_path_buf());
        let mut handle = tokio::spawn(async move {
            serve_for_test_with_components(
                serve_layout,
                home,
                Duration::from_secs(60),
                stop_rx,
                vec![reg],
            )
            .await
        });

        let client = LocalClient::new(layout);
        wait_ready(&client).await;
        remount_handle.wait_idle_for_test().await;

        let client_rm = client.clone();
        let rm_task = tokio::spawn(async move { client_rm.im_auth_remove("telegram").await });
        prepare_entered.notified().await;

        stop_tx.send(()).await.unwrap();

        let rm_reply = tokio::time::timeout(Duration::from_secs(1), rm_task)
            .await
            .expect("停机时卡在 prepare 的 ImAuthRemove 必须立即返回")
            .unwrap()
            .expect("凭据删除成功后绝不能返回 Internal 错误");
        assert!(rm_reply.removed);
        match &rm_reply.mount {
            ImMountResult::Unmounted { reason } => {
                assert!(reason.contains("后台正在停止"), "实际原因：{reason}");
            }
            other => panic!("预期返回 Unmounted，实际：{other:?}"),
        }
        assert!(
            crate::ImAuthStore::new(config_dir)
                .get("telegram")
                .unwrap()
                .is_none(),
            "磁盘上的凭据必须已被删除"
        );

        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(10)).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        tokio::time::resume();
        let exit = tokio::time::timeout(Duration::from_millis(500), &mut handle)
            .await
            .expect("第 3 步满 10 秒后应中止驱动并完成停机")
            .unwrap()
            .unwrap();
        let _ = gate_tx.send(());
        assert!(exit.failure.is_none());
    }
}

#[tokio::test]
async fn old_driver_exit_guard_never_clobbers_new_driver_running_state() {
    let dir = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(dir.path());
    let (stop_tx, stop_rx) = mpsc::channel(4);

    let pass = Arc::new(AtomicUsize::new(0));
    let pass3_entered = Arc::new(Notify::new());
    let (pass3_gate_tx, pass3_gate_rx) = oneshot::channel::<()>();
    let pass3_gate = Arc::new(Mutex::new(Some(pass3_gate_rx)));

    let pass_cl = pass.clone();
    let pass3_entered_cl = pass3_entered.clone();
    let pass3_gate_cl = pass3_gate.clone();
    let reg = ComponentRegistration::new("im", move |_service| {
        let n = pass_cl.fetch_add(1, Ordering::SeqCst) + 1;
        if n == 3 {
            pass3_entered_cl.notify_one();
            if let Some(rx) = pass3_gate_cl.lock().unwrap().take() {
                let _ = rx.blocking_recv();
            }
        }
        ComponentBuildOutcome::Disabled
    });
    let remount_handle = reg.remount_handle();

    let serve_layout = layout.clone();
    let home = Some(dir.path().to_path_buf());
    let mut handle = tokio::spawn(async move {
        serve_for_test_with_components(
            serve_layout,
            home,
            Duration::from_secs(60),
            stop_rx,
            vec![reg],
        )
        .await
    });

    let client = LocalClient::new(layout);
    wait_ready(&client).await;
    remount_handle.wait_idle_for_test().await;

    // 在第 2 轮驱动解锁队列之后、退出守卫 Drop 之前设置异步断点。
    let hook_entered = Arc::new(Notify::new());
    let hook_release = Arc::new(Notify::new());
    let hook_entered_cl = hook_entered.clone();
    let hook_release_cl = hook_release.clone();
    remount_handle.install_post_unlock_hook_for_test(move || async move {
        hook_entered_cl.notify_one();
        hook_release_cl.notified().await;
    });

    assert_eq!(
        remount_handle.remount().await.unwrap(),
        ImMountResult::Disabled
    );
    hook_entered.notified().await;

    // 在旧驱动尚未执行退出守卫 Drop 的窗口内启动新驱动（第 3 轮），并让新驱动阻塞在 build 中。
    let handle_new = remount_handle.clone();
    let reload_new = tokio::spawn(async move { handle_new.remount().await });
    pass3_entered.notified().await;

    // 放行旧驱动使其完成退出守卫 Drop：绝不能把正在运行的新驱动覆盖为空闲！
    hook_release.notify_one();
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }

    assert!(
        tokio::time::timeout(
            Duration::from_millis(20),
            remount_handle.wait_idle_for_test(),
        )
        .await
        .is_err(),
        "旧驱动退出守卫 Drop 后，仍在运行的新驱动不得被误判为空闲"
    );

    // 进入停机：第 3 步必须识别到新驱动仍在运行并等待满 10 秒上限，绝不能因误判空闲而在 0 秒提前退场。
    tokio::time::pause();
    stop_tx.send(()).await.unwrap();
    let err_new = reload_new.await.unwrap().unwrap_err();
    assert_eq!(err_new.kind, LocalErrorKind::Stopping);

    tokio::time::advance(Duration::from_secs(9)).await;
    tokio::task::yield_now().await;
    assert!(
        !handle.is_finished(),
        "未满 10 秒时停机第 3 步必须继续等待在途的新驱动任务"
    );

    tokio::time::advance(Duration::from_secs(1)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    tokio::time::resume();
    let exit = tokio::time::timeout(Duration::from_millis(500), &mut handle)
        .await
        .expect("第 3 步满 10 秒后应中止新驱动并完成停机")
        .unwrap()
        .unwrap();
    let _ = pass3_gate_tx.send(());
    assert!(exit.failure.is_none());
}
