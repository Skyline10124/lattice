// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 后台可选组件挂载、并发合并重新挂载、故障隔离与停机第 3 步收尾管理。
//!
//! 本模块只处理宿主提供的抽象组件注册项，不依赖具体业务组件（如 `lattice-im`）或平台文件监视机制。

use std::{
    cell::Cell,
    collections::BTreeMap,
    future::Future,
    mem::ManuallyDrop,
    pin::Pin,
    sync::{
        Arc, Mutex, Once, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use tokio::{sync::oneshot, sync::watch, task::AbortHandle};

use super::{
    server::{Backend, internal, lock},
    service::Service,
    types::{ComponentState, ComponentStatus, ImMountResult, LocalError, LocalErrorKind, Result},
};

/// 单个组件重新挂载停旧实例与后台停机第 3 步组件收尾共用的超时上限（10 秒）。
pub(crate) const COMPONENT_STOP_TIMEOUT: Duration = Duration::from_secs(10);

const PANIC_UNMOUNT_REASON: &str = "组件任务发生 panic";
const EARLY_EXIT_UNMOUNT_REASON: &str = "组件任务已退出";
const STOPPING_MESSAGE: &str = "后台正在停止";

thread_local! {
    static COMPONENT_PANIC_SCOPE_DEPTH: Cell<usize> = const { Cell::new(0) };
}

static COMPONENT_PANIC_HOOK_ONCE: Once = Once::new();

/// 安装受托管组件专用的脱敏 panic hook（幂等）。
///
/// 当线程处于组件执行作用域内发生 panic 时，仅经 `tracing::error!` 记录源码位置，
/// 不读取也不向 `stderr` 输出原始 panic 载荷，防止未脱敏的内部状态或凭据泄露；
/// 组件作用域之外的 panic 仍交由原有 hook 处理。
pub fn install_component_panic_hook() {
    COMPONENT_PANIC_HOOK_ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if COMPONENT_PANIC_SCOPE_DEPTH.with(|depth| depth.get() > 0) {
                if let Some(location) = info.location() {
                    tracing::error!(
                        file = location.file(),
                        line = location.line(),
                        column = location.column(),
                        "受托管组件执行期间发生 panic（已抑制原始载荷）"
                    );
                } else {
                    tracing::error!("受托管组件执行期间发生 panic（已抑制原始载荷）");
                }
                return;
            }
            previous(info);
        }));
    });
}

struct ComponentPanicScopeGuard;

impl ComponentPanicScopeGuard {
    fn enter() -> Self {
        install_component_panic_hook();
        COMPONENT_PANIC_SCOPE_DEPTH.with(|depth| depth.set(depth.get() + 1));
        Self
    }
}

impl Drop for ComponentPanicScopeGuard {
    fn drop(&mut self) {
        COMPONENT_PANIC_SCOPE_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

/// 在受托管组件 panic 载荷抑制作用域内同步执行闭包（适用于阻塞任务或单步轮询）。
pub fn with_component_panic_scope<R>(f: impl FnOnce() -> R) -> R {
    let _guard = ComponentPanicScopeGuard::enter();
    f()
}

/// 在每次 `poll` 与析构期间自动维护组件 panic 载荷抑制作用域的 Future 包装器。
pub struct ScopedComponentFuture<F> {
    inner: ManuallyDrop<F>,
}

impl<F: std::fmt::Debug> std::fmt::Debug for ScopedComponentFuture<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopedComponentFuture")
            .finish_non_exhaustive()
    }
}

impl<F: Future> Future for ScopedComponentFuture<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: `inner` 与 `Self` 结构性同生命期固定，不会从 `self` 中移出。
        let inner = unsafe { self.map_unchecked_mut(|this| &mut *this.inner) };
        with_component_panic_scope(|| inner.poll(cx))
    }
}

impl<F> Drop for ScopedComponentFuture<F> {
    fn drop(&mut self) {
        // SAFETY: `Drop::drop` 仅执行一次，此后不再访问 `self.inner`。
        with_component_panic_scope(|| unsafe {
            ManuallyDrop::drop(&mut self.inner);
        });
    }
}

/// 将组件主任务或其派生的子任务包装进组件 panic 载荷抑制作用域。
pub fn scoped_component_future<F: Future>(future: F) -> ScopedComponentFuture<F> {
    ScopedComponentFuture {
        inner: ManuallyDrop::new(future),
    }
}

fn stopping_error() -> LocalError {
    LocalError::new(LocalErrorKind::Stopping, STOPPING_MESSAGE)
}

async fn wait_stop(rx: &mut watch::Receiver<bool>) {
    loop {
        if *rx.borrow_and_update() {
            return;
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

/// 传递给组件运行任务的停止信号：在组件被重新挂载替换或后台进入停机时触发。
pub struct ComponentStop {
    rx: watch::Receiver<bool>,
    wait_fut: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl std::fmt::Debug for ComponentStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComponentStop")
            .field("stopped", &self.is_stopped())
            .finish()
    }
}

impl ComponentStop {
    fn new(mut rx: watch::Receiver<bool>) -> Self {
        let state_rx = rx.clone();
        let wait_fut = Box::pin(async move {
            wait_stop(&mut rx).await;
        });
        Self {
            rx: state_rx,
            wait_fut,
        }
    }

    fn is_stopped(&self) -> bool {
        *self.rx.borrow()
    }
}

impl Future for ComponentStop {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.wait_fut.as_mut().poll(cx)
    }
}

type ComponentRunFn = Box<
    dyn FnOnce(
            ComponentStop,
        ) -> Pin<Box<dyn Future<Output = std::result::Result<(), String>> + Send>>
        + Send,
>;

/// 只读构造通过、待由后台启动受监督异步任务的组件实例。
pub struct PreparedComponent {
    run: ComponentRunFn,
}

impl std::fmt::Debug for PreparedComponent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedComponent").finish_non_exhaustive()
    }
}

impl PreparedComponent {
    /// 包装组件运行闭包；闭包接收组件级停止信号 [`ComponentStop`]，返回脱敏后的中文错误原因（若有）。
    pub fn new<F, Fut>(run: F) -> Self
    where
        F: FnOnce(ComponentStop) -> Fut + Send + 'static,
        Fut: Future<Output = std::result::Result<(), String>> + Send + 'static,
    {
        Self {
            run: Box::new(move |stop| {
                Box::pin(scoped_component_future(async move { run(stop).await }))
            }),
        }
    }
}

/// 组件只读构造入口的返回结果。
#[derive(Debug)]
pub enum ComponentBuildOutcome {
    /// 配置与凭据齐备，启动该组件实例并记为已挂载。
    Component(PreparedComponent),
    /// 组件已启用，但当前缺少启动所需凭据等前置条件。
    Unavailable {
        /// 脱敏后的中文不可用原因。
        reason: String,
    },
    /// 组件配置、凭据或目录权限校验失败。
    InvalidConfiguration {
        /// 脱敏后的中文配置无效原因。
        reason: String,
    },
    /// 组件未启用，不出现在 `DaemonStatus.components` 中。
    Disabled,
}

struct RemountTarget {
    name: String,
    manager: OnceLock<Weak<ComponentManager>>,
}

/// 供宿主外部事件（如文件监视任务）请求重新挂载指定组件的句柄。
#[derive(Clone)]
pub struct ComponentRemountHandle {
    target: Arc<RemountTarget>,
}

impl std::fmt::Debug for ComponentRemountHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComponentRemountHandle")
            .field("name", &self.target.name)
            .finish()
    }
}

impl ComponentRemountHandle {
    /// 返回绑定的组件名称。
    pub fn name(&self) -> &str {
        &self.target.name
    }

    /// 请求重新挂载该组件；并发请求将按组件合并规则共享后续同一次挂载结果。
    pub async fn remount(&self) -> Result<ImMountResult> {
        let Some(weak) = self.target.manager.get() else {
            return Err(stopping_error());
        };
        let Some(manager) = weak.upgrade() else {
            return Err(stopping_error());
        };
        manager.remount(&self.target.name).await
    }

    #[cfg(test)]
    pub(crate) async fn wait_pending_waiters_for_test(&self, at_least: usize) {
        let Some(manager) = self.target.manager.get().and_then(Weak::upgrade) else {
            return;
        };
        let Some(slot) = manager.slots.get(&self.target.name) else {
            return;
        };
        let mut rx = slot.pending_waiters_tx.subscribe();
        while *rx.borrow_and_update() < at_least {
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    #[cfg(test)]
    pub(crate) async fn wait_idle_for_test(&self) {
        let Some(manager) = self.target.manager.get().and_then(Weak::upgrade) else {
            return;
        };
        let Some(slot) = manager.slots.get(&self.target.name) else {
            return;
        };
        let mut rx = slot.driver_state_tx.subscribe();
        while rx.borrow_and_update().1 {
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn install_post_unlock_hook_for_test<F, Fut>(&self, hook: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let Some(manager) = self.target.manager.get().and_then(Weak::upgrade) else {
            return;
        };
        let Some(slot) = manager.slots.get(&self.target.name) else {
            return;
        };
        *lock(&slot.post_unlock_hook) = Some(Box::new(move || Box::pin(hook())));
    }
}

type PrepareHook =
    Arc<dyn Fn(&ComponentRemountHandle) -> std::result::Result<(), String> + Send + Sync>;
type BuildHook = Arc<dyn Fn(Service) -> ComponentBuildOutcome + Send + Sync>;
#[cfg(test)]
type PostUnlockHook = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

/// 传给 `lattice_server::serve` 的组件注册项。
pub struct ComponentRegistration {
    name: String,
    prepare: Option<PrepareHook>,
    build: BuildHook,
    handle: ComponentRemountHandle,
}

impl std::fmt::Debug for ComponentRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComponentRegistration")
            .field("name", &self.name)
            .field("has_prepare", &self.prepare.is_some())
            .finish()
    }
}

impl ComponentRegistration {
    /// 以组件名与只读构造闭包创建注册项。
    ///
    /// 构造闭包将在阻塞线程池中执行，只允许读取本地文件，不得发起网络请求或自行常驻派生后台任务。
    pub fn new(
        name: impl Into<String>,
        build: impl Fn(Service) -> ComponentBuildOutcome + Send + Sync + 'static,
    ) -> Self {
        let name = name.into();
        let handle = ComponentRemountHandle {
            target: Arc::new(RemountTarget {
                name: name.clone(),
                manager: OnceLock::new(),
            }),
        };
        Self {
            name,
            prepare: None,
            build: Arc::new(build),
            handle,
        }
    }

    /// 附加宿主准备钩子（如确保 `0700` 目录存在并恢复 `inotify` 监视）。
    ///
    /// 准备钩子同样在阻塞线程池中于 `build` 之前执行；返回 `Err(reason)` 时跳过 `build` 并按配置无效处理。
    pub fn with_prepare(
        mut self,
        prepare: impl Fn(&ComponentRemountHandle) -> std::result::Result<(), String>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.prepare = Some(Arc::new(prepare));
        self
    }

    #[cfg(test)]
    pub(crate) fn remount_handle(&self) -> ComponentRemountHandle {
        self.handle.clone()
    }
}

#[derive(Clone)]
struct RunningInstance {
    generation: u64,
    stop_tx: watch::Sender<bool>,
    retired: Arc<AtomicBool>,
    inner_abort: AbortHandle,
    done_rx: watch::Receiver<bool>,
}

impl RunningInstance {
    fn is_finished(&self) -> bool {
        *self.done_rx.borrow() || self.inner_abort.is_finished()
    }

    async fn wait_done(&self) {
        let mut rx = self.done_rx.clone();
        wait_stop(&mut rx).await;
    }
}

#[derive(Default)]
struct SlotState {
    public_state: Option<ComponentState>,
    active: Option<RunningInstance>,
    retiring: Option<RunningInstance>,
}

#[derive(Default)]
struct RemountQueue {
    driver_generation: u64,
    running: bool,
    driver_abort: Option<AbortHandle>,
    pending_waiters: Vec<oneshot::Sender<Result<ImMountResult>>>,
}

struct ComponentSlot {
    name: String,
    prepare: Option<PrepareHook>,
    build: BuildHook,
    handle: ComponentRemountHandle,
    state: Mutex<SlotState>,
    queue: Mutex<RemountQueue>,
    pending_waiters_tx: watch::Sender<usize>,
    driver_state_tx: watch::Sender<(u64, bool)>,
    #[cfg(test)]
    post_unlock_hook: Mutex<Option<PostUnlockHook>>,
}

struct DriverExitGuard {
    slot: Arc<ComponentSlot>,
    driver_gen: u64,
}

impl Drop for DriverExitGuard {
    fn drop(&mut self) {
        let mut queue = lock(&self.slot.queue);
        if queue.driver_generation == self.driver_gen && queue.running {
            queue.running = false;
            queue.driver_abort = None;
            self.slot
                .driver_state_tx
                .send_replace((self.driver_gen, false));
        }
    }
}

impl ComponentSlot {
    fn on_instance_exited(
        &self,
        manager: &ComponentManager,
        generation: u64,
        retired: &AtomicBool,
        join_result: std::result::Result<std::result::Result<(), String>, tokio::task::JoinError>,
    ) {
        let mut state = lock(&self.state);
        if state
            .retiring
            .as_ref()
            .is_some_and(|inst| inst.generation == generation)
        {
            state.retiring = None;
        }
        let is_current_active = state
            .active
            .as_ref()
            .is_some_and(|inst| inst.generation == generation);
        if !is_current_active {
            return;
        }
        state.active = None;
        if retired.load(Ordering::Acquire) {
            return;
        }

        let stopping = manager.is_stopping();
        match join_result {
            Err(join_err) if join_err.is_panic() => {
                // 严禁拼接或回显原始 panic 载荷，防止未脱敏内部状态或秘密外泄。
                let reason = PANIC_UNMOUNT_REASON.to_owned();
                tracing::warn!(
                    component = %self.name,
                    "后台组件任务发生 panic，已切换为未挂载"
                );
                state.public_state = Some(ComponentState::Unmounted { reason });
            }
            Err(_cancelled) => {}
            Ok(Err(reason)) if !stopping => {
                tracing::warn!(
                    component = %self.name,
                    %reason,
                    "后台组件异常退出，已切换为未挂载"
                );
                state.public_state = Some(ComponentState::Unmounted { reason });
            }
            Ok(Ok(())) if !stopping => {
                tracing::warn!(
                    component = %self.name,
                    "后台组件任务未收到停止信号即提前退出，已切换为未挂载"
                );
                state.public_state = Some(ComponentState::Unmounted {
                    reason: EARLY_EXIT_UNMOUNT_REASON.to_owned(),
                });
            }
            Ok(_) => {}
        }
    }
}

pub(super) struct ComponentManager {
    backend: OnceLock<Weak<Backend>>,
    slots: BTreeMap<String, Arc<ComponentSlot>>,
    stopping: AtomicBool,
    stopping_tx: watch::Sender<bool>,
    next_generation: AtomicU64,
}

impl ComponentManager {
    pub(super) fn new(registrations: Vec<ComponentRegistration>) -> Arc<Self> {
        install_component_panic_hook();
        let mut slots = BTreeMap::new();
        let mut targets = Vec::with_capacity(registrations.len());
        for reg in registrations {
            targets.push(reg.handle.target.clone());
            let slot = Arc::new(ComponentSlot {
                name: reg.name.clone(),
                prepare: reg.prepare,
                build: reg.build,
                handle: reg.handle,
                state: Mutex::default(),
                queue: Mutex::default(),
                pending_waiters_tx: watch::channel(0).0,
                driver_state_tx: watch::channel((0, false)).0,
                #[cfg(test)]
                post_unlock_hook: Mutex::new(None),
            });
            let previous = slots.insert(reg.name, slot);
            debug_assert!(previous.is_none(), "组件注册名称不得重复");
        }
        let manager = Arc::new(Self {
            backend: OnceLock::new(),
            slots,
            stopping: AtomicBool::new(false),
            stopping_tx: watch::channel(false).0,
            next_generation: AtomicU64::new(1),
        });
        let weak = Arc::downgrade(&manager);
        for target in targets {
            let _ = target.manager.set(weak.clone());
        }
        manager
    }

    pub(super) fn bind_backend(&self, backend: &Arc<Backend>) {
        let _ = self.backend.set(Arc::downgrade(backend));
    }

    fn service(&self) -> Option<Service> {
        self.backend
            .get()
            .and_then(Weak::upgrade)
            .map(|backend| backend.service())
    }

    pub(super) fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }

    fn subscribe_stopping(&self) -> watch::Receiver<bool> {
        self.stopping_tx.subscribe()
    }

    /// 返回按 `name` 字典序排列的当前组件公开状态快照（`Disabled` 不包含在内）。
    pub(super) fn statuses(&self) -> Vec<ComponentStatus> {
        let mut out = Vec::new();
        for (name, slot) in &self.slots {
            if let Some(state) = lock(&slot.state).public_state.clone() {
                out.push(ComponentStatus {
                    name: name.clone(),
                    state,
                });
            }
        }
        out
    }

    /// 在后台监听就绪后并发启动全部已注册组件的初始挂载驱动（不阻塞连接接受与停机信号处理）。
    ///
    /// 启动前先同步标记槽位 `running = true`，保证紧随其后到达的 `remount` 请求必然排在初始挂载之后合并执行。
    pub(super) fn start_initial_mount(self: &Arc<Self>) {
        if self.is_stopping() {
            return;
        }
        for slot in self.slots.values() {
            let mut queue = lock(&slot.queue);
            if self.is_stopping() || queue.running {
                continue;
            }
            queue.driver_generation += 1;
            let driver_gen = queue.driver_generation;
            queue.running = true;
            slot.driver_state_tx.send_replace((driver_gen, true));
            let abort = self.spawn_driver(slot.clone(), driver_gen, Vec::new(), true);
            queue.driver_abort = Some(abort);
        }
    }

    /// 重新挂载指定名称的组件；未注册该组件时返回 `Disabled`，后台停机时返回 `Stopping`。
    pub(super) async fn remount(self: &Arc<Self>, name: &str) -> Result<ImMountResult> {
        if self.is_stopping() {
            return Err(stopping_error());
        }
        let Some(slot) = self.slots.get(name).cloned() else {
            return Ok(ImMountResult::Disabled);
        };
        let mut stop_rx = self.subscribe_stopping();
        let (tx, mut rx) = oneshot::channel();
        {
            let mut queue = lock(&slot.queue);
            if self.is_stopping() {
                return Err(stopping_error());
            }
            if queue.running {
                queue.pending_waiters.push(tx);
                slot.pending_waiters_tx
                    .send_replace(queue.pending_waiters.len());
            } else {
                queue.driver_generation += 1;
                let driver_gen = queue.driver_generation;
                queue.running = true;
                slot.driver_state_tx.send_replace((driver_gen, true));
                let abort = self.spawn_driver(slot.clone(), driver_gen, vec![tx], false);
                queue.driver_abort = Some(abort);
            }
        }
        tokio::select! {
            biased;
            res = &mut rx => match res {
                Ok(result) => result,
                Err(_) if self.is_stopping() => Err(stopping_error()),
                Err(_) => Err(internal()),
            },
            () = wait_stop(&mut stop_rx) => Err(stopping_error()),
        }
    }

    fn spawn_driver(
        self: &Arc<Self>,
        slot: Arc<ComponentSlot>,
        driver_gen: u64,
        mut current_batch: Vec<oneshot::Sender<Result<ImMountResult>>>,
        mut is_initial: bool,
    ) -> AbortHandle {
        let manager = self.clone();
        let exit_guard = DriverExitGuard {
            slot: slot.clone(),
            driver_gen,
        };
        tokio::spawn(async move {
            let _exit_guard = exit_guard;
            loop {
                let result = manager.execute_single_pass(&slot, is_initial).await;
                is_initial = false;
                for tx in current_batch.drain(..) {
                    let _ = tx.send(result.clone());
                }
                let next_batch = {
                    let mut queue = lock(&slot.queue);
                    if manager.is_stopping() {
                        let pending = std::mem::take(&mut queue.pending_waiters);
                        slot.pending_waiters_tx.send_replace(0);
                        if queue.driver_generation == driver_gen {
                            queue.running = false;
                            queue.driver_abort = None;
                            slot.driver_state_tx.send_replace((driver_gen, false));
                        }
                        for tx in pending {
                            let _ = tx.send(Err(stopping_error()));
                        }
                        break;
                    }
                    if queue.pending_waiters.is_empty() {
                        if queue.driver_generation == driver_gen {
                            queue.running = false;
                            queue.driver_abort = None;
                            slot.driver_state_tx.send_replace((driver_gen, false));
                        }
                        break;
                    }
                    let batch = std::mem::take(&mut queue.pending_waiters);
                    slot.pending_waiters_tx.send_replace(0);
                    batch
                };
                current_batch = next_batch;
            }
            #[cfg(test)]
            let hook = lock(&slot.post_unlock_hook).take();
            #[cfg(test)]
            if let Some(hook) = hook {
                hook().await;
            }
        })
        .abort_handle()
    }

    async fn execute_single_pass(
        self: &Arc<Self>,
        slot: &Arc<ComponentSlot>,
        is_initial: bool,
    ) -> Result<ImMountResult> {
        if self.is_stopping() {
            return Err(stopping_error());
        }
        let Some(service) = self.service() else {
            return Err(stopping_error());
        };

        let prepare = slot.prepare.clone();
        let build = slot.build.clone();
        let handle = slot.handle.clone();
        let outcome = match tokio::task::spawn_blocking(move || {
            with_component_panic_scope(|| {
                if let Some(prepare) = prepare
                    && let Err(reason) = prepare(&handle)
                {
                    return ComponentBuildOutcome::InvalidConfiguration { reason };
                }
                build(service)
            })
        })
        .await
        {
            Ok(outcome) => outcome,
            Err(join_err) if join_err.is_panic() => {
                tracing::warn!(
                    component = %slot.name,
                    "后台组件构造阶段发生 panic，按未挂载处理"
                );
                ComponentBuildOutcome::Unavailable {
                    reason: PANIC_UNMOUNT_REASON.to_owned(),
                }
            }
            Err(_) => return Err(internal()),
        };

        if self.is_stopping() {
            return Err(stopping_error());
        }

        // 若旧实例刚退出但监督协程尚未落锁清理，先等待监督协程完成状态归档，防止误判“实例仍在运行”。
        let finished_active = {
            let state = lock(&slot.state);
            state
                .active
                .as_ref()
                .filter(|inst| inst.is_finished())
                .cloned()
        };
        if let Some(inst) = finished_active {
            inst.wait_done().await;
        }

        if self.is_stopping() {
            return Err(stopping_error());
        }

        // 配置无效且当前有存活运行实例：保留旧实例继续运行，并附加配置错误原因。
        if let ComponentBuildOutcome::InvalidConfiguration { reason } = &outcome {
            let mut state = lock(&slot.state);
            if self.is_stopping() {
                return Err(stopping_error());
            }
            if state
                .active
                .as_ref()
                .is_some_and(|inst| !inst.is_finished())
            {
                tracing::warn!(
                    component = %slot.name,
                    %reason,
                    "重新挂载组件配置无效，保留上一份运行中的实例"
                );
                state.public_state = Some(ComponentState::Mounted {
                    configuration_error: Some(reason.clone()),
                });
                return Ok(ImMountResult::UsingPreviousConfiguration {
                    reason: reason.clone(),
                });
            }
        }

        // 其他情况：若存在存活旧实例，先通知其停止并等待至多 10 秒（后台停机交错时交给停机第 3 步统一收尾）。
        let to_retire = {
            let mut state = lock(&slot.state);
            if self.is_stopping() {
                return Err(stopping_error());
            }
            if let Some(old) = state.active.take() {
                old.retired.store(true, Ordering::Release);
                let _ = old.stop_tx.send(true);
                state.retiring = Some(old.clone());
                Some(old)
            } else {
                None
            }
        };

        if let Some(old) = to_retire {
            let mut backend_stop = self.subscribe_stopping();
            let retire_sleep = tokio::time::sleep(COMPONENT_STOP_TIMEOUT);
            tokio::pin!(retire_sleep);

            enum RetireOutcome {
                Done,
                TimedOut,
                BackendStopping,
            }

            let retire_outcome = tokio::select! {
                () = old.wait_done() => RetireOutcome::Done,
                () = &mut retire_sleep => RetireOutcome::TimedOut,
                () = wait_stop(&mut backend_stop) => RetireOutcome::BackendStopping,
            };

            match retire_outcome {
                RetireOutcome::BackendStopping => {
                    // 后台已进入停机：不启动新实例，也不叠加等待，旧实例留在 retiring 交由第 3 步共享 10 秒收尾。
                    return Err(stopping_error());
                }
                RetireOutcome::TimedOut => {
                    if self.is_stopping() {
                        return Err(stopping_error());
                    }
                    tracing::warn!(
                        component = %slot.name,
                        "等待旧组件实例收尾超过 10 秒，强制中止旧实例"
                    );
                    old.inner_abort.abort();
                    let mut stop_after_abort = self.subscribe_stopping();
                    tokio::select! {
                        () = old.wait_done() => {}
                        () = wait_stop(&mut stop_after_abort) => {
                            return Err(stopping_error());
                        }
                    }
                }
                RetireOutcome::Done => {}
            }
        }

        let mut state = lock(&slot.state);
        if self.is_stopping() {
            return Err(stopping_error());
        }
        state.retiring = None;

        match outcome {
            ComponentBuildOutcome::Component(prepared) => {
                let instance = self.spawn_instance(slot.clone(), prepared);
                state.active = Some(instance);
                state.public_state = Some(ComponentState::Mounted {
                    configuration_error: None,
                });
                if is_initial {
                    tracing::info!(component = %slot.name, "后台组件已完成初始挂载");
                } else {
                    tracing::info!(component = %slot.name, "后台组件已完成重新挂载");
                }
                Ok(ImMountResult::Mounted)
            }
            ComponentBuildOutcome::Unavailable { reason }
            | ComponentBuildOutcome::InvalidConfiguration { reason } => {
                tracing::warn!(
                    component = %slot.name,
                    %reason,
                    "后台组件未挂载"
                );
                state.public_state = Some(ComponentState::Unmounted {
                    reason: reason.clone(),
                });
                Ok(ImMountResult::Unmounted { reason })
            }
            ComponentBuildOutcome::Disabled => {
                state.public_state = None;
                if !is_initial {
                    tracing::info!(component = %slot.name, "后台组件已切换为未启用");
                }
                Ok(ImMountResult::Disabled)
            }
        }
    }

    fn spawn_instance(
        self: &Arc<Self>,
        slot: Arc<ComponentSlot>,
        prepared: PreparedComponent,
    ) -> RunningInstance {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let (stop_tx, stop_rx) = watch::channel(false);
        let retired = Arc::new(AtomicBool::new(false));
        let (done_tx, done_rx) = watch::channel(false);

        let inner_handle = tokio::spawn(scoped_component_future((prepared.run)(
            ComponentStop::new(stop_rx),
        )));
        let inner_abort = inner_handle.abort_handle();

        let manager = self.clone();
        let retired_for_supervisor = retired.clone();
        tokio::spawn(async move {
            let join_result = inner_handle.await;
            slot.on_instance_exited(&manager, generation, &retired_for_supervisor, join_result);
            let _ = done_tx.send_replace(true);
        });

        RunningInstance {
            generation,
            stop_tx,
            retired,
            inner_abort,
            done_rx,
        }
    }

    /// 停机第 1 步：标记进入停机，向全部运行中/正在退休的组件实例发出停止信号，并唤醒合并等待队列。
    pub(super) fn begin_stop(&self) {
        if self.stopping.swap(true, Ordering::AcqRel) {
            return;
        }
        self.stopping_tx.send_replace(true);
        for slot in self.slots.values() {
            {
                let state = lock(&slot.state);
                if let Some(active) = &state.active {
                    let _ = active.stop_tx.send(true);
                }
                if let Some(retiring) = &state.retiring {
                    let _ = retiring.stop_tx.send(true);
                }
            }
            let pending = {
                let mut queue = lock(&slot.queue);
                let taken = std::mem::take(&mut queue.pending_waiters);
                slot.pending_waiters_tx.send_replace(0);
                taken
            };
            for tx in pending {
                let _ = tx.send(Err(stopping_error()));
            }
        }
    }

    /// 停机第 3 步：在排空结束后，以单一共享 10 秒上限等待全部组件任务与在途挂载驱动收尾；
    /// 若当前无任何活动组件任务或驱动任务则立即返回（零额外等待）。
    /// 到期后仅触发 `abort()` 并让出一次调度，不再开启任何额外计时等待。
    pub(super) async fn shutdown(&self) {
        self.begin_stop();

        let mut instances = Vec::new();
        let mut driver_watches = Vec::new();
        for slot in self.slots.values() {
            {
                let state = lock(&slot.state);
                if let Some(active) = &state.active
                    && !*active.done_rx.borrow()
                {
                    let _ = active.stop_tx.send(true);
                    instances.push(active.clone());
                }
                if let Some(retiring) = &state.retiring
                    && !*retiring.done_rx.borrow()
                {
                    let _ = retiring.stop_tx.send(true);
                    instances.push(retiring.clone());
                }
            }
            let driver_rx = slot.driver_state_tx.subscribe();
            if driver_rx.borrow().1 {
                driver_watches.push(driver_rx);
            }
        }

        if instances.is_empty() && driver_watches.is_empty() {
            return;
        }

        let deadline = tokio::time::Instant::now() + COMPONENT_STOP_TIMEOUT;
        let wait_all = async {
            for inst in &instances {
                inst.wait_done().await;
            }
            for mut rx in driver_watches {
                while rx.borrow_and_update().1 {
                    if rx.changed().await.is_err() {
                        break;
                    }
                }
            }
        };

        if tokio::time::timeout_at(deadline, wait_all).await.is_err() {
            for inst in &instances {
                if !*inst.done_rx.borrow() {
                    tracing::warn!("停机第 3 步等待组件收尾超过 10 秒，强制中止组件任务");
                    inst.inner_abort.abort();
                }
            }
            for slot in self.slots.values() {
                if let Some(abort) = lock(&slot.queue).driver_abort.take() {
                    abort.abort();
                }
            }
            tokio::task::yield_now().await;
        }
    }
}
