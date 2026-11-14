// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::driver::{
    CleanupError, CleanupOperation, CommandOutcome, DaemonChild, DriverError, HarnessIntervention,
    ProcessGroupCleanup, ProcessInspectionOperation, RunningCommand, process_exited, process_stat,
    processes_in_pgid, reject_own_process_group, signal_process_group,
};

/// 显式启动后台并等待就绪的单次上限；实际截止时间取它与场景剩余预算中的较小值。
pub(crate) const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
/// 轮询后台就绪与组退空时的采样间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// 强制 `SIGKILL` 之后核验组已空的独立有界等待；只用于安全清理，不放宽正常 `daemon stop` 的预算。
const FORCED_CLEANUP_GRACE: Duration = Duration::from_secs(2);
/// 收尾 `daemon stop` 未干净返回（`not_running` 或失败）时，等待自有后台组退出的上限，不超过剩余正常预算。
const STOP_DRAIN_WINDOW: Duration = Duration::from_secs(2);
/// `daemon stop` 干净返回、实例锁已放开后，等待自有后台组退空的上限；超时即判失败。
const DEPARTURE_EXIT_WINDOW: Duration = Duration::from_secs(5);
/// 子进程已确认退出后回收 `CommandOutcome` 的有界等待。
const REAP_GRACE: Duration = Duration::from_millis(250);

fn sleep_poll(deadline: Instant) -> bool {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return false;
    }
    std::thread::sleep(POLL_INTERVAL.min(remaining));
    true
}

/// 显式启动或重启后台时的可选参数与环境覆盖。
#[derive(Debug, Clone, Default)]
pub struct DaemonOptions {
    drain_timeout: Option<String>,
    env_set: BTreeMap<String, String>,
    env_unset: BTreeSet<String>,
}

impl DaemonOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// 为 `lattice-daemon` 指定 `--drain-timeout` 参数。
    pub fn drain_timeout(mut self, value: impl Into<String>) -> Self {
        self.drain_timeout = Some(value.into());
        self
    }

    /// 在场景环境白名单快照之上覆盖后台专用的环境变量，不改动前端环境。
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        let key = key.into();
        self.env_unset.remove(&key);
        self.env_set.insert(key, value.into());
        self
    }

    /// 从后台启动的环境快照中移除指定变量，不改动前端环境。
    pub fn unset_env(mut self, key: impl Into<String>) -> Self {
        let key = key.into();
        self.env_set.remove(&key);
        self.env_unset.insert(key);
        self
    }

    pub(crate) fn apply_env(&self, snapshot: &mut HashMap<String, String>) {
        for key in &self.env_unset {
            snapshot.remove(key);
        }
        for (key, value) in &self.env_set {
            snapshot.insert(key.clone(), value.clone());
        }
    }
}

/// 后台生命周期管理错误。
#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("场景后台已在运行（pgid={pgid}），需要更换配置时请调用 restart_daemon")]
    AlreadyRunning { pgid: u32 },
    #[error("当前没有受管理的运行中后台")]
    NotRunning,
    #[error("启动后台前发现状态根已被非自有后台持有实例锁")]
    UnownedOwnerPresent,
    #[error(
        "状态根已被非自有后台占用（监听 pid={listener_pid:?}，自有子进程 pid={child_pid}，子进程退出码={exit_code:?}，信号={signal:?}，摘要={summary}，stderr={stderr:?}）"
    )]
    UnownedDaemonReady {
        listener_pid: Option<u32>,
        child_pid: u32,
        summary: String,
        exit_code: Option<i32>,
        signal: Option<i32>,
        stderr: String,
    },
    #[error("启动后台前场景预算已耗尽")]
    BudgetExhausted,
    #[error(
        "后台在就绪前提前退出（退出码={exit_code:?}，信号={signal:?}，摘要={summary}，stderr={stderr:?}）"
    )]
    EarlyExit {
        summary: String,
        exit_code: Option<i32>,
        signal: Option<i32>,
        stderr: String,
    },
    #[error("后台未能在截止时间（{timeout:?}）内就绪（最后探测={last_status}，stderr={stderr:?}）")]
    ReadyTimeout {
        timeout: Duration,
        last_status: String,
        stderr: String,
    },
    #[error("重启前停止旧后台失败: {0:?}")]
    StopFailed(Vec<String>),
    #[error(transparent)]
    Driver(#[from] DriverError),
    #[error("后台进程操作失败: {0}")]
    Io(#[from] std::io::Error),
}

/// 场景后台的归属生命周期状态：
/// - `Idle`：尚未启动，或未保存任何后台状态；
/// - `Unreaped`：组首领尚未被 `wait` 回收，内核锁定数值 `pgid` 不会被其他新进程组复用，允许凭数值进程组号发信号；
/// - `ReapedResiduals`：组首领已回收且退出结果已取出保存，数值进程组授权已撤销，仅允许经已确认的 `pidfd` 成员处理；
/// - `Reaped`：组首领已回收且组内已无自有成员残留，数值进程组授权已撤销，仅保留退出结果与 `stderr` 供读取。
enum DaemonState {
    Idle,
    Unreaped(UnreapedDaemon),
    ReapedResiduals(ResidualDaemon),
    Reaped(ReapedDaemon),
}

/// 组首领尚未被 `wait` 回收的自有后台状态。
struct UnreapedDaemon {
    cli_binary: PathBuf,
    child: DaemonChild,
    pgid: u32,
    leader: OwnedProcess,
    members: Vec<OwnedProcess>,
}

/// 组首领已回收但组内仍有残留成员（或暂无法经 `/proc` 确认退空）的自有后台状态。
struct ResidualDaemon {
    pgid: u32,
    members: Vec<OwnedProcess>,
    outcome: Option<CommandOutcome>,
    stderr: Vec<u8>,
}

/// 组首领与自有组成员均已收尾完毕的后台状态。
struct ReapedDaemon {
    outcome: Option<CommandOutcome>,
    stderr: Vec<u8>,
}

/// 场景后台生命周期控制器：只管理显式启动的直接子进程及其进程组，并在收尾时检查残留后台。
pub(crate) struct ScenarioDaemon {
    state_root: PathBuf,
    state: Mutex<DaemonState>,
}

/// `pidfd` 绑定一个内核进程实例；退出后即变为可读并失效于新进程，不会因数值进程号被复用而误指。
struct OwnedProcess {
    pid: u32,
    fd: OwnedFd,
}

impl OwnedProcess {
    fn open(pid: u32) -> std::io::Result<Self> {
        // SAFETY: `pidfd_open` 不接收指针；成功时返回本函数独占的新文件描述符。
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            pid,
            // SAFETY: 系统调用成功，文件描述符的唯一所有权移交给 `OwnedFd`。
            fd: unsafe { OwnedFd::from_raw_fd(fd as i32) },
        })
    }

    /// 经 `poll(pidfd, POLLIN)` 非阻塞检查该进程实例是否已退出（含僵尸态），绝不调用 `wait` 回收子进程。
    fn has_exited(&self) -> std::io::Result<bool> {
        self.poll_exited(0)
    }

    /// 在给定截止时间前等待该进程实例退出，不调用 `wait` 回收子进程。
    fn wait_exited(&self, deadline: Option<Instant>) -> std::io::Result<bool> {
        loop {
            let timeout_ms = match deadline {
                None => -1,
                Some(until) => {
                    let remaining = until.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return self.poll_exited(0);
                    }
                    remaining.as_millis().min(i32::MAX as u128) as i32
                }
            };
            if self.poll_exited(timeout_ms)? {
                return Ok(true);
            }
            if deadline.is_some_and(|until| Instant::now() >= until) {
                return self.poll_exited(0);
            }
        }
    }

    fn poll_exited(&self, timeout_ms: i32) -> std::io::Result<bool> {
        loop {
            let mut poll = libc::pollfd {
                fd: self.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `poll` 指向有效栈结构，仅查询该 `pidfd` 绑定的内核进程实例是否已退出。
            let result = unsafe { libc::poll(&mut poll, 1, timeout_ms) };
            if result < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    if timeout_ms > 0 {
                        return Ok(false);
                    }
                    continue;
                }
                return Err(error);
            }
            if poll.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
                return Err(std::io::Error::other("后台 pidfd 状态检查失败"));
            }
            return Ok(result > 0 && (poll.revents & libc::POLLIN != 0));
        }
    }

    fn in_group(&self, pgid: u32) -> std::io::Result<bool> {
        let stat = process_stat(self.pid)
            .map_err(|error| std::io::Error::new(error.kind, error.message))?;
        let exited = self.has_exited()?;
        // 必须在读取 `/proc` 后复核旧实例仍未退出，防止元数据来自复用后的新进程。
        Ok(!exited && stat.is_some_and(|stat| stat.pgid == pgid))
    }

    fn send_signal(&self, signal: libc::c_int) -> std::io::Result<()> {
        // SAFETY: 空 `siginfo` 与 `flags=0` 使用默认信号语义，只作用于该 `pidfd` 绑定的内核进程实例。
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.fd.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            // `pidfd` 的 `ESRCH` 只表示此实例已退出，不会改指向其他进程。
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }

    fn kill(&self) -> std::io::Result<()> {
        self.send_signal(libc::SIGKILL)
    }
}

fn capture_members(pgid: u32, members: &[u32]) -> std::io::Result<Vec<OwnedProcess>> {
    let mut captured = Vec::new();
    for &pid in members {
        let member = match OwnedProcess::open(pid) {
            Ok(member) => member,
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => continue,
            Err(error) => return Err(error),
        };
        if member.in_group(pgid)? {
            captured.push(member);
        }
    }
    Ok(captured)
}

fn retain_live_members(members: &mut Vec<OwnedProcess>) {
    members.retain(|member| !matches!(member.has_exited(), Ok(true)));
}

/// 统一收敛已回收组首领后的后台状态：无存活成员时转为 `Reaped`，否则保留 `ReapedResiduals`。
fn reaped_or_residuals(
    pgid: u32,
    outcome: Option<CommandOutcome>,
    stderr: Vec<u8>,
    mut members: Vec<OwnedProcess>,
) -> DaemonState {
    retain_live_members(&mut members);
    if members.is_empty() {
        DaemonState::Reaped(ReapedDaemon { outcome, stderr })
    } else {
        DaemonState::ReapedResiduals(ResidualDaemon {
            pgid,
            members,
            outcome,
            stderr,
        })
    }
}

fn reconcile_residual_state(state: &mut DaemonState) -> bool {
    let DaemonState::ReapedResiduals(r) = std::mem::replace(state, DaemonState::Idle) else {
        return false;
    };
    *state = reaped_or_residuals(r.pgid, r.outcome, r.stderr, r.members);
    matches!(state, DaemonState::ReapedResiduals(_))
}

fn merge_owned_processes(
    existing: Vec<OwnedProcess>,
    newly_captured: Vec<OwnedProcess>,
) -> Vec<OwnedProcess> {
    let seen: BTreeSet<u32> = newly_captured.iter().map(|proc| proc.pid).collect();
    let mut merged = newly_captured;
    for proc in existing {
        if !seen.contains(&proc.pid) && !matches!(proc.has_exited(), Ok(true)) {
            merged.push(proc);
        }
    }
    merged
}

/// 仅对组首领尚未回收的 `UnreapedDaemon` 刷新已确认成员集合：
/// 内核保证未回收组首领的数值 `pgid` 不会被其他新进程组复用。
fn refresh_unreaped_members(owned: &mut UnreapedDaemon, pids: &[u32]) -> std::io::Result<()> {
    reject_own_process_group(owned.pgid)?;
    let captured = capture_members(owned.pgid, pids)?;
    let previous = std::mem::take(&mut owned.members);
    owned.members = merge_owned_processes(previous, captured);
    Ok(())
}

/// 组首领已回收时复核候选进程组归属：数值 `pgid` 已可能被内核复用，
/// 先记录仍存活于同一 `pgid` 的具体见证，捕获全部候选 `pidfd` 后再次确认同一个见证仍存活于该组；
/// 复核失败时丢弃本次新捕获的成员，只处理此前已确认的成员。
fn confirm_residual_members(
    pgid: u32,
    witnesses: &[OwnedProcess],
    pids: &[u32],
) -> std::io::Result<Option<Vec<OwnedProcess>>> {
    reject_own_process_group(pgid)?;
    let mut pre_verified = Vec::new();
    for witness in witnesses {
        if witness.in_group(pgid)? {
            pre_verified.push(witness);
        }
    }
    if pre_verified.is_empty() {
        return Ok(None);
    }
    let captured = capture_members(pgid, pids)?;
    for witness in pre_verified {
        if witness.in_group(pgid)? {
            return Ok(Some(captured));
        }
    }
    Ok(None)
}

enum ResidualRefresh {
    GroupEmpty,
    Confirmed(Vec<u32>),
    Unconfirmed(Vec<u32>),
    InspectFailed(CleanupError),
    ConfirmFailed(Vec<u32>, std::io::Error),
}

/// 统一的残留进程组刷新：扫描 `/proc` → 见证前后复核并捕获 → 合并成员 → 过滤已退出句柄。
fn refresh_residual_members(
    pgid: u32,
    members: &mut Vec<OwnedProcess>,
    inspect: &mut impl FnMut(u32) -> Result<Vec<u32>, CleanupError>,
) -> ResidualRefresh {
    let result = match inspect(pgid) {
        Ok(pids) if pids.is_empty() => ResidualRefresh::GroupEmpty,
        Ok(pids) => match confirm_residual_members(pgid, members, &pids) {
            Ok(Some(confirmed)) => {
                let existing = std::mem::take(members);
                *members = merge_owned_processes(existing, confirmed);
                ResidualRefresh::Confirmed(pids)
            }
            Ok(None) => ResidualRefresh::Unconfirmed(pids),
            Err(error) => ResidualRefresh::ConfirmFailed(pids, error),
        },
        Err(error) => ResidualRefresh::InspectFailed(error),
    };
    retain_live_members(members);
    result
}

struct ForcedCleanupReport {
    next_state: DaemonState,
    reaped_outcome: Option<CommandOutcome>,
    owned_survivors: BTreeSet<u32>,
    errors: Vec<String>,
    pidfd_error: Option<std::io::Error>,
}

impl ForcedCleanupReport {
    fn new(next_state: DaemonState) -> Self {
        Self {
            next_state,
            reaped_outcome: None,
            owned_survivors: BTreeSet::new(),
            errors: Vec::new(),
            pidfd_error: None,
        }
    }

    fn record_pidfd_error(&mut self, message: String, error: std::io::Error) {
        self.errors.push(message);
        if self.pidfd_error.is_none() {
            self.pidfd_error = Some(error);
        }
    }
}

fn kill_and_wait_pidfds(
    members: &mut Vec<OwnedProcess>,
    grace: Duration,
    report: &mut ForcedCleanupReport,
    mut cleanup_errors: Option<&mut Vec<CleanupError>>,
) {
    let mut kill_failed = BTreeSet::new();
    for member in members.iter() {
        if let Err(error) = member.kill() {
            kill_failed.insert(member.pid);
            if let Some(out) = cleanup_errors.as_deref_mut() {
                out.push(CleanupError::from_io(
                    CleanupOperation::SignalProcessGroup(libc::SIGKILL),
                    &error,
                ));
            }
            report.record_pidfd_error(
                format!(
                    "经 pidfd 向自有后台成员发送 SIGKILL 失败: pid={} {error}",
                    member.pid
                ),
                error,
            );
        }
    }
    let deadline = Instant::now() + grace;
    for member in members.iter() {
        if !kill_failed.contains(&member.pid)
            && let Err(error) = member.wait_exited(Some(deadline))
        {
            report.record_pidfd_error(
                format!(
                    "经 pidfd 等待自有后台成员退出失败: pid={} {error}",
                    member.pid
                ),
                error,
            );
        }
    }
    members.retain(|member| match member.has_exited() {
        Ok(true) => false,
        Ok(false) => true,
        Err(error) => {
            report.record_pidfd_error(
                format!(
                    "经 pidfd 检查自有后台成员退出状态失败: pid={} {error}",
                    member.pid
                ),
                error,
            );
            true
        }
    });
}

/// 组首领已回收后，在宽限窗口内持续复核并清理可能刚派生出的同组后代。
fn settle_killed_residuals(
    pgid: u32,
    members: &mut Vec<OwnedProcess>,
    grace_deadline: Instant,
    inspect: &mut impl FnMut(u32) -> Result<Vec<u32>, CleanupError>,
    report: &mut ForcedCleanupReport,
    mut cleanup_errors: Option<&mut Vec<CleanupError>>,
) {
    let read_proc = CleanupOperation::InspectProcessGroup(ProcessInspectionOperation::ReadProc);
    loop {
        match refresh_residual_members(pgid, members, inspect) {
            ResidualRefresh::GroupEmpty | ResidualRefresh::Unconfirmed(_) => break,
            ResidualRefresh::Confirmed(survivors) => {
                kill_and_wait_pidfds(members, Duration::from_millis(50), report, None);
                if !sleep_poll(grace_deadline) {
                    report.owned_survivors.extend(survivors.iter().copied());
                    report.errors.push(format!(
                        "强制清理后后台进程组仍有残留成员: pgid={pgid} members={survivors:?}"
                    ));
                    break;
                }
            }
            ResidualRefresh::ConfirmFailed(survivors, error) => {
                if let Some(out) = cleanup_errors.as_deref_mut() {
                    out.push(CleanupError::from_io(read_proc, &error));
                }
                report.record_pidfd_error(
                    format!(
                        "强制清理后复核后台进程组归属失败: pgid={pgid} {error}；未确认候选成员={survivors:?}"
                    ),
                    error,
                );
                break;
            }
            ResidualRefresh::InspectFailed(error) => {
                report.errors.push(format!(
                    "强制清理后检查后台进程组失败: pgid={pgid} {}",
                    error.message
                ));
                if let Some(out) = cleanup_errors.as_deref_mut()
                    && !out.iter().any(|existing| existing.message == error.message)
                {
                    out.push(error);
                }
                break;
            }
        }
    }
}

/// 检查与成员捕获成功后才回收组首领；失败时保留 `Unreaped`，供后续清理继续持有整组归属。
fn reap_exited_leader(
    state: &mut DaemonState,
    inspect: &mut impl FnMut(u32) -> Result<Vec<u32>, CleanupError>,
) -> Result<CommandOutcome, DriverError> {
    let DaemonState::Unreaped(owned) = state else {
        unreachable!("只回收未回收态的组首领")
    };
    let pids =
        inspect(owned.pgid).map_err(|error| std::io::Error::new(error.kind, error.message))?;
    refresh_unreaped_members(owned, &pids)?;
    let DaemonState::Unreaped(mut owned) = std::mem::replace(state, DaemonState::Idle) else {
        unreachable!()
    };
    let pgid = owned.pgid;
    let leader_pid = owned.child.pid();
    let stderr = owned.child.stderr_snapshot();
    let outcome = owned.child.reap_exited(ProcessGroupCleanup::default());
    owned.members.retain(|m| m.pid != leader_pid);
    // 输出收集失败也不能丢掉已确认的存活后代。
    *state = reaped_or_residuals(
        pgid,
        outcome.as_ref().ok().cloned(),
        outcome
            .as_ref()
            .map_or(stderr, |outcome| outcome.stderr.clone()),
        owned.members,
    );
    outcome
}

fn reap_unreaped_state(
    state: &mut DaemonState,
    inspect: &mut impl FnMut(u32) -> Result<Vec<u32>, CleanupError>,
    context: &str,
    errors: &mut Vec<String>,
) {
    if let Err(error) = reap_exited_leader(state, inspect) {
        errors.push(format!("{context}: {error}"));
    }
}

/// 强制终止尚未回收的自有后台：仅在回收组首领前可向数值 `pgid` 发 `SIGKILL`，
/// 同时经 `pidfd` 终止并等待已确认成员退出，随后回收组首领并在宽限期内收尾残留后代。
fn force_clean_unreaped(
    mut owned: UnreapedDaemon,
    inspect: &mut impl FnMut(u32) -> Result<Vec<u32>, CleanupError>,
) -> Result<ForcedCleanupReport, DriverError> {
    owned
        .child
        .record_intervention(HarnessIntervention::ForcedKill);
    let mut report = ForcedCleanupReport::new(DaemonState::Idle);
    let mut cleanup = ProcessGroupCleanup {
        forced: true,
        ..ProcessGroupCleanup::default()
    };
    let pgid = owned.pgid;
    let leader_pid = owned.child.pid();
    let read_proc = CleanupOperation::InspectProcessGroup(ProcessInspectionOperation::ReadProc);
    let inspected_ok = match inspect(pgid) {
        Ok(pids) => {
            report.errors.push(format!(
                "自有后台需要强制清理: pgid={pgid} members={pids:?}"
            ));
            match refresh_unreaped_members(&mut owned, &pids) {
                Ok(()) => true,
                Err(error) => {
                    cleanup
                        .errors
                        .push(CleanupError::from_io(read_proc, &error));
                    report.record_pidfd_error(
                        format!("强制清理前刷新自有后台成员失败: pgid={pgid} {error}"),
                        error,
                    );
                    false
                }
            }
        }
        Err(error) => {
            report.errors.push(format!(
                "强制清理前检查后台进程组失败: pgid={pgid} {}；无法确认其余同组成员，仍在回收自有组首领前向整组发送 SIGKILL",
                error.message
            ));
            cleanup.errors.push(error);
            false
        }
    };

    // 直接子进程尚未回收，数值 pgid 仍由它占用；扫描失败不会撤销这份归属。
    // 先终止整组，才能覆盖就绪后派生、尚未进入 pidfd 缓存的后代。
    if let Err(error) = signal_process_group(pgid, libc::SIGKILL)
        && error.raw_os_error() != Some(libc::ESRCH)
    {
        report.errors.push(format!(
            "向自有后台进程组发送 SIGKILL 失败: pgid={pgid} {error}"
        ));
        cleanup.errors.push(CleanupError::from_io(
            CleanupOperation::SignalProcessGroup(libc::SIGKILL),
            &error,
        ));
    }
    if let Err(error) = owned.leader.kill() {
        cleanup
            .errors
            .push(CleanupError::from_io(CleanupOperation::KillChild, &error));
        report.record_pidfd_error(
            format!("经 pidfd 向自有后台组首领发送 SIGKILL 失败: pid={leader_pid} {error}"),
            error,
        );
    }

    owned.members.retain(|member| member.pid != leader_pid);
    let grace_deadline = Instant::now() + FORCED_CLEANUP_GRACE;
    kill_and_wait_pidfds(
        &mut owned.members,
        FORCED_CLEANUP_GRACE,
        &mut report,
        Some(&mut cleanup.errors),
    );

    let mut outcome = owned.child.force_reap(cleanup)?;
    if inspected_ok || !owned.members.is_empty() {
        settle_killed_residuals(
            pgid,
            &mut owned.members,
            grace_deadline,
            inspect,
            &mut report,
            Some(&mut outcome.cleanup.errors),
        );
    }

    if !report.owned_survivors.is_empty() {
        outcome.cleanup.surviving_pids = report.owned_survivors.iter().copied().collect();
    } else if !owned.members.is_empty() {
        let pids: Vec<u32> = owned.members.iter().map(|m| m.pid).collect();
        report.owned_survivors.extend(pids.iter().copied());
        report.errors.push(format!(
            "强制清理后后台进程组仍有残留成员: pgid={pgid} members={pids:?}"
        ));
        outcome.cleanup.surviving_pids = pids;
    }

    report.next_state = reaped_or_residuals(
        pgid,
        Some(outcome.clone()),
        outcome.stderr.clone(),
        owned.members,
    );
    report.reaped_outcome = Some(outcome);
    Ok(report)
}

fn force_clean_residuals(
    mut residual: ResidualDaemon,
    inspect: &mut impl FnMut(u32) -> Result<Vec<u32>, CleanupError>,
) -> ForcedCleanupReport {
    let pgid = residual.pgid;
    let mut report = ForcedCleanupReport::new(DaemonState::Idle);
    let mut should_settle = false;
    match refresh_residual_members(pgid, &mut residual.members, inspect) {
        ResidualRefresh::GroupEmpty => {
            if !residual.members.is_empty() {
                let pids: Vec<u32> = residual.members.iter().map(|m| m.pid).collect();
                report.errors.push(format!(
                    "自有后台组首领已回收但仍有已确认成员未退出，经 pidfd 强制清理: pgid={pgid} pids={pids:?}"
                ));
            }
        }
        ResidualRefresh::Confirmed(pgid_pids) => {
            report.errors.push(format!(
                "自有后台需要强制清理: pgid={pgid} members={pgid_pids:?}"
            ));
            should_settle = true;
        }
        ResidualRefresh::Unconfirmed(pgid_pids) => {
            report.owned_survivors.extend(pgid_pids.iter().copied());
            report.errors.push(format!(
                "后台进程组归属未确认，拒绝发送信号: pgid={pgid} members={pgid_pids:?}"
            ));
        }
        ResidualRefresh::ConfirmFailed(pgid_pids, error) => {
            report.owned_survivors.extend(pgid_pids.iter().copied());
            report.record_pidfd_error(
                format!(
                    "复核后台进程组归属失败: pgid={pgid} {error}；仅经已持有 pidfd 终止确定自有的进程，无法确认候选成员={pgid_pids:?}"
                ),
                error,
            );
        }
        ResidualRefresh::InspectFailed(error) => {
            report.errors.push(format!(
                "检查自有后台进程组失败: pgid={pgid} {}；仅经已持有 pidfd 终止确定自有的进程，无法扫描确认其余同组成员",
                error.message
            ));
        }
    }

    if !residual.members.is_empty() || should_settle {
        kill_and_wait_pidfds(
            &mut residual.members,
            FORCED_CLEANUP_GRACE,
            &mut report,
            None,
        );
    }
    if should_settle {
        let grace_deadline = Instant::now() + FORCED_CLEANUP_GRACE;
        settle_killed_residuals(
            pgid,
            &mut residual.members,
            grace_deadline,
            inspect,
            &mut report,
            None,
        );
    }

    report.next_state =
        reaped_or_residuals(pgid, residual.outcome, residual.stderr, residual.members);
    if let DaemonState::ReapedResiduals(still_alive) = &report.next_state {
        let surviving: Vec<u32> = still_alive.members.iter().map(|m| m.pid).collect();
        report.owned_survivors.extend(surviving.iter().copied());
        report.errors.push(format!(
            "经 pidfd 强制清理后仍有已确认自有成员未退出: pgid={pgid} pids={surviving:?}"
        ));
    }
    report
}

/// 供 `cleanup`、`kill_and_wait` 与 `Drop` 共用的强制清理核心。
fn force_clean_daemon_state(
    state: DaemonState,
    inspect: &mut impl FnMut(u32) -> Result<Vec<u32>, CleanupError>,
) -> Result<ForcedCleanupReport, DriverError> {
    match state {
        DaemonState::Idle | DaemonState::Reaped(_) => Ok(ForcedCleanupReport::new(state)),
        DaemonState::Unreaped(owned) => force_clean_unreaped(owned, inspect),
        DaemonState::ReapedResiduals(residual) => Ok(force_clean_residuals(residual, inspect)),
    }
}

fn unowned_ready_error(
    listener_pid: Option<u32>,
    child_pid: u32,
    outcome: CommandOutcome,
) -> DaemonError {
    DaemonError::UnownedDaemonReady {
        listener_pid,
        child_pid,
        summary: outcome.summary().to_string(),
        exit_code: outcome.exit_code(),
        signal: outcome.signal(),
        stderr: String::from_utf8_lossy(&outcome.stderr).into_owned(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SocketOwner {
    Owned,
    Unowned { listener_pid: u32 },
    NotListening,
}

fn unix_sockaddr(path: &Path) -> std::io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: `sockaddr_un` 为纯数据结构体，全零初始化保证末尾 `NUL` 终止字节有效。
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.is_empty() || bytes.contains(&0) || bytes.len() >= addr.sun_path.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "api.sock 路径无法编码为 Unix 域套接字地址",
        ));
    }
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    // SAFETY: 已校验 `bytes.len() < addr.sun_path.len()`，源与目标不重叠且单字节对齐。
    unsafe {
        std::ptr::copy_nonoverlapping(
            bytes.as_ptr().cast::<libc::c_char>(),
            addr.sun_path.as_mut_ptr(),
            bytes.len(),
        );
    }
    let len = std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1;
    Ok((addr, len as libc::socklen_t))
}

/// 以非阻塞方式连接 `api.sock` 并经 `SO_PEERCRED` 核对监听端是否为自有直接子进程；
/// 监听队列已满返回 `EAGAIN` 时视为尚未就绪，在共享截止时间内重试而不阻塞。
fn verify_socket_owner(
    api_sock: &Path,
    expected_pid: u32,
    deadline: Instant,
) -> std::io::Result<SocketOwner> {
    let (addr, addr_len) = unix_sockaddr(api_sock)?;
    loop {
        // SAFETY: 创建带 `O_NONBLOCK | O_CLOEXEC` 的本地流套接字，成功时立即交由 `OwnedFd` 管理。
        let raw_fd = unsafe {
            libc::socket(
                libc::AF_UNIX,
                libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                0,
            )
        };
        if raw_fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `raw_fd` 为刚创建成功的独占文件描述符。
        let stream = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        // SAFETY: `stream` 为有效套接字，`addr` 在 `addr_len` 字节内已完整初始化。
        let rc = unsafe {
            libc::connect(
                stream.as_raw_fd(),
                (&addr as *const libc::sockaddr_un).cast(),
                addr_len,
            )
        };
        if rc == 0 {
            // SAFETY: `ucred` 为纯数值结构体，零初始化有效。
            let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
            let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
            // SAFETY: `stream` 为已连接的 Unix 域流套接字描述符，`cred` 与 `len` 指向栈上有效内存。
            let opt_rc = unsafe {
                libc::getsockopt(
                    stream.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_PEERCRED,
                    (&mut cred as *mut libc::ucred).cast(),
                    &mut len,
                )
            };
            if opt_rc != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if len as usize != std::mem::size_of::<libc::ucred>() {
                return Err(std::io::Error::other("读取 api.sock 对端凭据长度异常"));
            }
            let listener_pid = u32::try_from(cred.pid).unwrap_or(0);
            return Ok(if expected_pid > 0 && listener_pid == expected_pid {
                SocketOwner::Owned
            } else {
                SocketOwner::Unowned { listener_pid }
            });
        }
        let error = std::io::Error::last_os_error();
        match error.kind() {
            std::io::ErrorKind::Interrupted if Instant::now() < deadline => continue,
            std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::NotFound
            | std::io::ErrorKind::ConnectionRefused => return Ok(SocketOwner::NotListening),
            std::io::ErrorKind::WouldBlock => {
                if !sleep_poll(deadline) {
                    return Ok(SocketOwner::NotListening);
                }
            }
            _ if error.raw_os_error() == Some(libc::EINPROGRESS) => {
                if !sleep_poll(deadline) {
                    return Ok(SocketOwner::NotListening);
                }
            }
            _ => return Err(error),
        }
    }
}

fn classify_startup_exit(
    state_root: &Path,
    api_sock: &Path,
    child_pid: u32,
    deadline: Instant,
    outcome: CommandOutcome,
) -> DaemonError {
    match verify_socket_owner(api_sock, child_pid, deadline) {
        Ok(SocketOwner::Unowned { listener_pid }) => {
            unowned_ready_error(Some(listener_pid), child_pid, outcome)
        }
        _ if owner_lock_held(state_root).unwrap_or(false) => {
            unowned_ready_error(None, child_pid, outcome)
        }
        _ => DaemonError::EarlyExit {
            summary: outcome.summary().to_string(),
            exit_code: outcome.exit_code(),
            signal: outcome.signal(),
            stderr: String::from_utf8_lossy(&outcome.stderr).into_owned(),
        },
    }
}

fn abort_startup_io(
    guard: &mut DaemonState,
    startup_owned: UnreapedDaemon,
    error: std::io::Error,
) -> DaemonError {
    match force_clean_unreaped(startup_owned, &mut processes_in_pgid) {
        Ok(report) => {
            *guard = report.next_state;
            DaemonError::Io(error)
        }
        Err(driver_err) => DaemonError::Driver(driver_err),
    }
}

fn isolated_daemon_cli(
    binary: &Path,
    state_root: &Path,
    home_leaf: &str,
    args: &[&str],
) -> Command {
    let mut command = Command::new(binary);
    command
        .args(args)
        .current_dir("/")
        .env_clear()
        .env("XDG_STATE_HOME", state_root)
        .env("HOME", state_root.join(home_leaf));
    command
}

impl ScenarioDaemon {
    pub(crate) fn new(state_root: PathBuf) -> Self {
        Self {
            state_root,
            state: Mutex::new(DaemonState::Idle),
        }
    }

    /// 以直接子进程方式启动 `lattice-daemon`，在共享截止时间内轮询公开 `daemon status` 直至就绪，
    /// 并核对 `api.sock` 的监听者确为本轮启动的自有直接子进程。
    pub(crate) fn start(
        &self,
        cli_binary: &Path,
        daemon_binary: &Path,
        project_dir: &Path,
        env_snapshot: HashMap<String, String>,
        options: &DaemonOptions,
        deadline: Instant,
    ) -> Result<(), DaemonError> {
        let started = Instant::now();
        if started >= deadline {
            return Err(DaemonError::BudgetExhausted);
        }
        let mut guard = self.state.lock().expect("自有后台锁");
        match &*guard {
            DaemonState::Unreaped(current) => {
                return Err(DaemonError::AlreadyRunning { pgid: current.pgid });
            }
            DaemonState::ReapedResiduals(residual) => {
                return Err(DaemonError::AlreadyRunning {
                    pgid: residual.pgid,
                });
            }
            DaemonState::Idle | DaemonState::Reaped(_) => {}
        }
        if owner_lock_held(&self.state_root)? {
            return Err(DaemonError::UnownedOwnerPresent);
        }
        *guard = DaemonState::Idle;

        let mut command = Command::new(daemon_binary);
        command
            .arg("--state-root")
            .arg(&self.state_root)
            .current_dir(project_dir)
            .env_clear();
        if let Some(drain_timeout) = &options.drain_timeout {
            command.args(["--drain-timeout", drain_timeout]);
        }
        for (key, value) in &env_snapshot {
            command.env(key, value);
        }

        let child = DaemonChild::spawn(command)?;
        let child_pid = child.pid();
        let pgid = child.pgid();
        let api_sock = self.state_root.join("lattice/local/api.sock");

        let leader = match OwnedProcess::open(child_pid) {
            Ok(leader) => leader,
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
                let outcome = child.reap_exited(ProcessGroupCleanup::default())?;
                *guard = reaped_or_residuals(
                    pgid,
                    Some(outcome.clone()),
                    outcome.stderr.clone(),
                    Vec::new(),
                );
                return Err(classify_startup_exit(
                    &self.state_root,
                    &api_sock,
                    child_pid,
                    deadline,
                    outcome,
                ));
            }
            Err(error) => {
                let _ = signal_process_group(pgid, libc::SIGKILL);
                let _ = child.force_reap(ProcessGroupCleanup {
                    forced: true,
                    ..ProcessGroupCleanup::default()
                });
                return Err(DaemonError::Io(error));
            }
        };

        let mut startup_owned = UnreapedDaemon {
            cli_binary: cli_binary.to_path_buf(),
            child,
            pgid,
            leader,
            members: Vec::new(),
        };

        let mut last_status = String::from("尚未完成首次探测");
        let mut leader_exited = false;
        loop {
            match startup_owned.leader.has_exited() {
                Ok(true) => {
                    leader_exited = true;
                    break;
                }
                Ok(false) if Instant::now() >= deadline => break,
                Ok(false) => {}
                Err(error) => return Err(abort_startup_io(&mut guard, startup_owned, error)),
            }

            match probe_daemon_ready(cli_binary, &self.state_root, deadline) {
                Ok(ProbeState::Ready) => {
                    match verify_socket_owner(&api_sock, child_pid, deadline) {
                        Ok(SocketOwner::Unowned { listener_pid }) => {
                            let reap_until = Instant::now()
                                + deadline
                                    .saturating_duration_since(Instant::now())
                                    .clamp(REAP_GRACE, Duration::from_secs(1));
                            let outcome = if matches!(
                                startup_owned.leader.wait_exited(Some(reap_until)),
                                Ok(true)
                            ) {
                                *guard = DaemonState::Unreaped(startup_owned);
                                reap_exited_leader(&mut guard, &mut processes_in_pgid)?
                            } else {
                                let r =
                                    force_clean_unreaped(startup_owned, &mut processes_in_pgid)?;
                                *guard = r.next_state;
                                r.reaped_outcome.expect("已回收未回收态后台")
                            };
                            return Err(unowned_ready_error(
                                Some(listener_pid),
                                child_pid,
                                outcome,
                            ));
                        }
                        Ok(SocketOwner::NotListening) => {
                            last_status = String::from("api.sock 尚未接受连接");
                        }
                        Ok(SocketOwner::Owned) => {
                            if startup_owned.leader.has_exited().unwrap_or(false) {
                                leader_exited = true;
                                break;
                            }
                            let res = processes_in_pgid(pgid)
                                .map_err(|e| std::io::Error::new(e.kind, e.message))
                                .and_then(|pids| {
                                    refresh_unreaped_members(&mut startup_owned, &pids)
                                });
                            if let Err(error) = res {
                                return Err(abort_startup_io(&mut guard, startup_owned, error));
                            }
                            if !startup_owned.members.iter().any(|m| m.pid == child_pid)
                                && let Ok(extra) = OwnedProcess::open(child_pid)
                            {
                                startup_owned.members.push(extra);
                            }
                            *guard = DaemonState::Unreaped(startup_owned);
                            return Ok(());
                        }
                        Err(error) => {
                            return Err(abort_startup_io(&mut guard, startup_owned, error));
                        }
                    }
                }
                Ok(ProbeState::Pending(summary)) => last_status = summary,
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                    last_status = error.to_string();
                    break;
                }
                Err(error) => return Err(abort_startup_io(&mut guard, startup_owned, error)),
            }

            if startup_owned.leader.has_exited().unwrap_or(false) {
                leader_exited = true;
                break;
            }
            if !sleep_poll(deadline) {
                break;
            }
        }

        if leader_exited || startup_owned.leader.has_exited().unwrap_or(false) {
            *guard = DaemonState::Unreaped(startup_owned);
            let outcome = reap_exited_leader(&mut guard, &mut processes_in_pgid)?;
            return Err(classify_startup_exit(
                &self.state_root,
                &api_sock,
                child_pid,
                deadline,
                outcome,
            ));
        }
        let report = force_clean_unreaped(startup_owned, &mut processes_in_pgid)?;
        let killed = report.reaped_outcome.expect("已回收未回收态后台");
        let stderr = String::from_utf8_lossy(&killed.stderr).into_owned();
        *guard = report.next_state;
        Err(DaemonError::ReadyTimeout {
            timeout: deadline.saturating_duration_since(started),
            last_status,
            stderr,
        })
    }

    /// 返回当前存活的自有后台组首领列表（最多一个）；组首领退出或已回收后返回空列表。
    pub(crate) fn owners(&self) -> std::io::Result<Vec<u32>> {
        let mut guard = self.state.lock().expect("自有后台锁");
        let DaemonState::Unreaped(owned) = &mut *guard else {
            return Ok(Vec::new());
        };
        if !owned.leader.in_group(owned.pgid)? {
            return Ok(Vec::new());
        }
        let pids = processes_in_pgid(owned.pgid)
            .map_err(|error| std::io::Error::new(error.kind, error.message))?;
        refresh_unreaped_members(owned, &pids)?;
        Ok(vec![owned.pgid])
    }

    pub(crate) fn pgid(&self) -> Option<u32> {
        match &*self.state.lock().expect("自有后台锁") {
            DaemonState::Unreaped(owned) => Some(owned.pgid),
            DaemonState::Idle | DaemonState::ReapedResiduals(_) | DaemonState::Reaped(_) => None,
        }
    }

    pub(crate) fn terminate(&self) -> Result<(), DaemonError> {
        self.send_control_signal(libc::SIGTERM, HarnessIntervention::TerminateRequested)
    }

    pub(crate) fn interrupt(&self) -> Result<(), DaemonError> {
        self.send_control_signal(libc::SIGINT, HarnessIntervention::InterruptRequested)
    }

    fn send_control_signal(
        &self,
        signal: libc::c_int,
        intervention: HarnessIntervention,
    ) -> Result<(), DaemonError> {
        let mut guard = self.state.lock().expect("自有后台锁");
        match &mut *guard {
            DaemonState::Unreaped(owned) => {
                signal_process_group(owned.pgid, signal)?;
                owned.child.record_intervention(intervention);
                Ok(())
            }
            DaemonState::ReapedResiduals(residual) => {
                let _ = refresh_residual_members(
                    residual.pgid,
                    &mut residual.members,
                    &mut processes_in_pgid,
                );
                if !reconcile_residual_state(&mut guard) {
                    return Err(DaemonError::NotRunning);
                }
                let DaemonState::ReapedResiduals(residual) = &*guard else {
                    unreachable!()
                };
                for member in &residual.members {
                    member.send_signal(signal)?;
                }
                Ok(())
            }
            DaemonState::Idle | DaemonState::Reaped(_) => Err(DaemonError::NotRunning),
        }
    }

    /// 经组首领 `pidfd` 非阻塞检查直接子进程是否已退出，绝不调用 `wait` 提前回收子进程。
    pub(crate) fn has_exited(&self) -> Result<bool, DaemonError> {
        let guard = self.state.lock().expect("自有后台锁");
        match &*guard {
            DaemonState::Idle => Err(DaemonError::NotRunning),
            DaemonState::Unreaped(owned) => Ok(owned.leader.has_exited()?),
            DaemonState::ReapedResiduals(_) | DaemonState::Reaped(_) => Ok(true),
        }
    }

    pub(crate) fn stderr_snapshot(&self) -> Vec<u8> {
        match &*self.state.lock().expect("自有后台锁") {
            DaemonState::Idle => Vec::new(),
            DaemonState::Unreaped(owned) => owned.child.stderr_snapshot(),
            DaemonState::ReapedResiduals(residual) => residual.stderr.clone(),
            DaemonState::Reaped(reaped) => reaped.stderr.clone(),
        }
    }

    pub(crate) fn take_outcome(&self) -> Option<CommandOutcome> {
        match &mut *self.state.lock().expect("自有后台锁") {
            DaemonState::Idle | DaemonState::Unreaped(_) => None,
            DaemonState::ReapedResiduals(residual) => residual.outcome.take(),
            DaemonState::Reaped(reaped) => reaped.outcome.take(),
        }
    }

    pub(crate) fn wait(&self, timeout: Option<Duration>) -> Result<CommandOutcome, DaemonError> {
        let mut guard = self.state.lock().expect("自有后台锁");
        match &mut *guard {
            DaemonState::Idle => Err(DaemonError::NotRunning),
            DaemonState::Reaped(reaped) => reaped.outcome.take().ok_or(DaemonError::NotRunning),
            DaemonState::ReapedResiduals(residual) => {
                residual.outcome.take().ok_or(DaemonError::NotRunning)
            }
            DaemonState::Unreaped(owned) => {
                let deadline = timeout.map(|limit| Instant::now() + limit);
                let exited = owned.leader.wait_exited(deadline)?;
                if exited {
                    return Ok(reap_exited_leader(&mut guard, &mut processes_in_pgid)?);
                }
                let DaemonState::Unreaped(mut owned) =
                    std::mem::replace(&mut *guard, DaemonState::Idle)
                else {
                    unreachable!()
                };
                if let Some(limit) = timeout {
                    owned
                        .child
                        .record_intervention(HarnessIntervention::TimedOut(limit));
                }
                let report = force_clean_unreaped(owned, &mut processes_in_pgid)?;
                *guard = report.next_state;
                if let Some(error) = report.pidfd_error {
                    return Err(DaemonError::Io(error));
                }
                Ok(report.reaped_outcome.expect("已回收未回收态后台"))
            }
        }
    }

    pub(crate) fn kill_and_wait(&self) -> Result<CommandOutcome, DaemonError> {
        self.kill_and_wait_with_inspection(&mut processes_in_pgid)
    }

    fn kill_and_wait_with_inspection(
        &self,
        inspect: &mut impl FnMut(u32) -> Result<Vec<u32>, CleanupError>,
    ) -> Result<CommandOutcome, DaemonError> {
        let mut guard = self.state.lock().expect("自有后台锁");
        let state = std::mem::replace(&mut *guard, DaemonState::Idle);
        let report = force_clean_daemon_state(state, inspect)?;
        *guard = report.next_state;
        if let Some(error) = report.pidfd_error {
            return Err(DaemonError::Io(error));
        }
        report.reaped_outcome.ok_or(DaemonError::NotRunning)
    }

    /// `daemon stop` 返回后的退场核验：立即检查 `owner.lock` 与 `api.sock` 已释放，
    /// 并在 `DEPARTURE_EXIT_WINDOW` 内等待自有后台组退空，同时检查无残留后台。
    pub(crate) fn departed(&self) -> Result<(), Vec<String>> {
        let mut guard = self.state.lock().expect("自有后台锁");
        let mut errors = Vec::new();
        let mut owned_survivors = BTreeSet::new();
        self.wait_for_departure_locked(
            &mut guard,
            &mut owned_survivors,
            &mut errors,
            &mut processes_in_pgid,
        );
        self.check_residual_daemons(&owned_survivors, &mut errors);
        finish(errors)
    }

    fn wait_for_departure_locked(
        &self,
        state: &mut DaemonState,
        owned_survivors: &mut BTreeSet<u32>,
        errors: &mut Vec<String>,
        inspect: &mut impl FnMut(u32) -> Result<Vec<u32>, CleanupError>,
    ) {
        match owner_lock_held(&self.state_root) {
            Ok(false) => {}
            Ok(true) => errors.push("daemon stop 返回时 owner.lock 仍被持有".into()),
            Err(error) => errors.push(format!("检查 owner.lock 失败: {error}")),
        }
        match std::fs::symlink_metadata(self.state_root.join("lattice/local/api.sock")) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => errors.push("daemon stop 返回时 api.sock 仍存在".into()),
            Err(error) => errors.push(format!("检查 api.sock 失败: {error}")),
        }

        match state {
            DaemonState::Idle | DaemonState::Reaped(_) => {}
            DaemonState::Unreaped(owned) => {
                let pgid = owned.pgid;
                let leader_pid = owned.child.pid();
                let deadline = Instant::now() + DEPARTURE_EXIT_WINDOW;
                let mut emptied = false;
                loop {
                    match inspect(pgid) {
                        Ok(members) if members.is_empty() => {
                            let _ = owned.leader.wait_exited(Some(Instant::now() + REAP_GRACE));
                            if owned.leader.has_exited().unwrap_or(false) {
                                emptied = true;
                                break;
                            }
                            if Instant::now() >= deadline {
                                owned_survivors.insert(leader_pid);
                                errors.push(format!(
                                    "daemon stop 返回后自有后台组首领未在 {DEPARTURE_EXIT_WINDOW:?} 内退出: pgid={pgid}"
                                ));
                                break;
                            }
                        }
                        Ok(members) => {
                            let _ = refresh_unreaped_members(owned, &members);
                            if Instant::now() >= deadline {
                                owned_survivors.extend(members.iter().copied());
                                errors.push(format!(
                                    "daemon stop 返回后自有后台进程组未在 {DEPARTURE_EXIT_WINDOW:?} 内退空: pgid={pgid} members={members:?}"
                                ));
                                break;
                            }
                        }
                        Err(error) => {
                            owned_survivors.insert(leader_pid);
                            owned_survivors.extend(owned.members.iter().map(|m| m.pid));
                            errors.push(format!(
                                "检查自有后台进程组失败: pgid={pgid} {}",
                                error.message
                            ));
                            break;
                        }
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
                if emptied {
                    reap_unreaped_state(state, inspect, "回收已退场后台子进程失败", errors);
                }
            }
            DaemonState::ReapedResiduals(_) => {
                let deadline = Instant::now() + DEPARTURE_EXIT_WINDOW;
                while let DaemonState::ReapedResiduals(residual) = state {
                    let pgid = residual.pgid;
                    match refresh_residual_members(pgid, &mut residual.members, inspect) {
                        ResidualRefresh::GroupEmpty | ResidualRefresh::Unconfirmed(_)
                            if residual.members.is_empty() =>
                        {
                            reconcile_residual_state(state);
                            break;
                        }
                        ResidualRefresh::Unconfirmed(_) => break,
                        ResidualRefresh::GroupEmpty if Instant::now() >= deadline => {
                            let pids: Vec<u32> = residual.members.iter().map(|m| m.pid).collect();
                            owned_survivors.extend(pids.iter().copied());
                            errors.push(format!(
                                "daemon stop 返回后自有后台已确认成员未在 {DEPARTURE_EXIT_WINDOW:?} 内退出: pgid={pgid} pids={pids:?}"
                            ));
                            break;
                        }
                        ResidualRefresh::Confirmed(pgid_pids) if Instant::now() >= deadline => {
                            owned_survivors.extend(pgid_pids.iter().copied());
                            errors.push(format!(
                                "daemon stop 返回后自有后台进程组未在 {DEPARTURE_EXIT_WINDOW:?} 内退空: pgid={pgid} members={pgid_pids:?}"
                            ));
                            break;
                        }
                        ResidualRefresh::GroupEmpty | ResidualRefresh::Confirmed(_) => {}
                        ResidualRefresh::ConfirmFailed(_, error) => {
                            owned_survivors.extend(residual.members.iter().map(|m| m.pid));
                            errors.push(format!(
                                "复核自有后台残留进程组归属失败: pgid={pgid} {error}"
                            ));
                            break;
                        }
                        ResidualRefresh::InspectFailed(error) => {
                            owned_survivors.extend(residual.members.iter().map(|m| m.pid));
                            errors.push(format!(
                                "检查自有后台进程组失败: pgid={pgid} {}",
                                error.message
                            ));
                            break;
                        }
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
            }
        }
    }

    /// 停止并清理自有后台进程组，随后核验本场景状态根下不存在任何残留后台进程。
    /// 只停止与清理，绝不隐式重启，也不重置场景预算。
    pub(crate) fn cleanup(&self, normal_deadline: Instant) -> Result<(), Vec<String>> {
        self.cleanup_with_inspection(normal_deadline, &mut processes_in_pgid)
    }

    fn cleanup_with_inspection(
        &self,
        normal_deadline: Instant,
        inspect: &mut impl FnMut(u32) -> Result<Vec<u32>, CleanupError>,
    ) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        let mut guard = self.state.lock().expect("自有后台锁");
        let mut owned_survivors = BTreeSet::new();

        self.try_normal_stop_unreaped(&mut guard, normal_deadline, inspect, &mut errors);

        if let DaemonState::Unreaped(owned) = &*guard
            && owned.leader.has_exited().unwrap_or(false)
            && matches!(inspect(owned.pgid), Ok(members) if members.is_empty())
        {
            reap_unreaped_state(&mut guard, inspect, "回收已退空后台子进程失败", &mut errors);
        }

        if matches!(
            &*guard,
            DaemonState::Unreaped(_) | DaemonState::ReapedResiduals(_)
        ) {
            let state = std::mem::replace(&mut *guard, DaemonState::Idle);
            match force_clean_daemon_state(state, inspect) {
                Ok(report) => {
                    *guard = report.next_state;
                    owned_survivors.extend(report.owned_survivors);
                    errors.extend(report.errors);
                }
                Err(error) => errors.push(format!("回收强制终止的后台子进程失败: {error}")),
            }
        }

        self.check_residual_daemons(&owned_survivors, &mut errors);
        finish(errors)
    }

    /// 尝试对未回收的自有后台发起正常 `daemon stop`；若后台组退空则直接收敛为 `Reaped`。
    fn try_normal_stop_unreaped(
        &self,
        state: &mut DaemonState,
        normal_deadline: Instant,
        inspect: &mut impl FnMut(u32) -> Result<Vec<u32>, CleanupError>,
        errors: &mut Vec<String>,
    ) {
        let DaemonState::Unreaped(owned) = state else {
            return;
        };
        let pgid = owned.pgid;
        let initial_members = match inspect(pgid) {
            Ok(members) => members,
            Err(error) => {
                errors.push(format!(
                    "检查自有后台进程组失败: pgid={pgid} {}",
                    error.message
                ));
                return;
            }
        };

        if initial_members.is_empty() && owned.leader.has_exited().unwrap_or(false) {
            reap_unreaped_state(state, inspect, "回收已退出后台子进程失败", errors);
            return;
        }
        if let Err(error) = refresh_unreaped_members(owned, &initial_members) {
            errors.push(format!("刷新自有后台进程组成员失败: pgid={pgid} {error}"));
            return;
        }
        match owned.leader.in_group(pgid) {
            Ok(true) => {}
            Ok(false) => {
                errors.push(format!(
                    "自有后台组首领已退出但组内仍有存活成员: pgid={pgid} members={initial_members:?}"
                ));
                return;
            }
            Err(error) => {
                errors.push(format!("检查自有后台组首领失败: pgid={pgid} {error}"));
                return;
            }
        }
        if Instant::now() >= normal_deadline {
            errors.push("正常停止后台前场景预算已耗尽".into());
            return;
        }

        let command = isolated_daemon_cli(
            &owned.cli_binary,
            &self.state_root,
            "cleanup-home",
            &["--json", "daemon", "stop"],
        );
        let stop = RunningCommand::spawn_until(command, normal_deadline)
            .and_then(|running| running.wait(None).map_err(std::io::Error::other));
        let returned = match stop {
            Ok(outcome) if outcome.is_clean_exit(0) => true,
            Ok(outcome) if already_stopped(&outcome) => false,
            Ok(outcome) => {
                errors.push(format!("正常停止后台失败: {}", outcome.summary()));
                false
            }
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                errors.push("正常停止后台前场景预算已耗尽".into());
                false
            }
            Err(error) => {
                errors.push(format!("执行 daemon stop 失败: {error}"));
                false
            }
        };
        if Instant::now() >= normal_deadline && !errors.iter().any(|e| e.contains("场景预算")) {
            errors.push("正常停止后台期间场景预算已耗尽".into());
        }

        if returned {
            let mut departure_survivors = BTreeSet::new();
            self.wait_for_departure_locked(state, &mut departure_survivors, errors, inspect);
            return;
        }

        let drain_deadline = (Instant::now() + STOP_DRAIN_WINDOW).min(normal_deadline);
        while Instant::now() < drain_deadline {
            let DaemonState::Unreaped(owned) = state else {
                break;
            };
            match inspect(pgid) {
                Ok(remaining)
                    if remaining.is_empty() && owned.leader.has_exited().unwrap_or(false) =>
                {
                    if Instant::now() >= normal_deadline
                        && !errors.iter().any(|e| e.contains("场景预算"))
                    {
                        errors.push("等待后台排空期间场景预算已耗尽".into());
                    }
                    reap_unreaped_state(state, inspect, "回收排空退出后台子进程失败", errors);
                    return;
                }
                Ok(remaining) => {
                    let _ = refresh_unreaped_members(owned, &remaining);
                }
                Err(error) => {
                    errors.push(format!(
                        "等待后台排空时检查进程组失败: pgid={pgid} {}",
                        error.message
                    ));
                    return;
                }
            }
            let _ = sleep_poll(drain_deadline);
        }
    }

    fn check_residual_daemons(&self, excluded_pids: &BTreeSet<u32>, errors: &mut Vec<String>) {
        match scan_residual_daemons(&self.state_root) {
            Ok(pids) => {
                let residuals: Vec<u32> = pids
                    .into_iter()
                    .filter(|pid| !excluded_pids.contains(pid))
                    .collect();
                if !residuals.is_empty() {
                    errors.push(format!(
                        "发现不属于测试套件的残留 `lattice-daemon` 后台进程: pids={residuals:?}"
                    ));
                }
            }
            Err(error) => errors.push(format!("扫描残留后台进程失败: {error}")),
        }
    }
}

impl Drop for ScenarioDaemon {
    fn drop(&mut self) {
        let state = std::mem::replace(self.state.get_mut().expect("自有后台锁"), DaemonState::Idle);
        if let Ok(report) = force_clean_daemon_state(state, &mut processes_in_pgid) {
            *self.state.get_mut().expect("自有后台锁") = report.next_state;
        }
    }
}

enum ProbeState {
    Ready,
    Pending(String),
}

fn probe_daemon_ready(
    binary: &Path,
    state_root: &Path,
    deadline: Instant,
) -> std::io::Result<ProbeState> {
    let command = isolated_daemon_cli(
        binary,
        state_root,
        "probe-home",
        &["--json", "daemon", "status"],
    );
    let outcome = RunningCommand::spawn_until(command, deadline)?
        .wait(None)
        .map_err(std::io::Error::other)?;
    if outcome.is_clean_exit(0)
        && let Ok(status) = serde_json::from_slice::<serde_json::Value>(&outcome.stdout)
        && status["stopping"] == serde_json::Value::Bool(false)
    {
        return Ok(ProbeState::Ready);
    }
    Ok(ProbeState::Pending(outcome.summary().to_string()))
}

/// 按 `/proc/<pid>/cmdline` 扫描指向本场景状态根的存活 `lattice-daemon` 进程，仅用于残留检查。
fn scan_residual_daemons(state_root: &Path) -> std::io::Result<Vec<u32>> {
    let expected = state_root.as_os_str().as_bytes();
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let stat =
            process_stat(pid).map_err(|error| std::io::Error::new(error.kind, error.message))?;
        if stat.is_none() {
            continue;
        }
        let args = match std::fs::read(entry.path().join("cmdline")) {
            Ok(args) => args,
            Err(error) if process_exited(&error) => continue,
            Err(error) => return Err(error),
        };
        if matches_daemon_cmdline(&args, expected) {
            found.push(pid);
        }
    }
    found.sort_unstable();
    Ok(found)
}

/// 识别 `/proc/<pid>/cmdline` 是否为 `lattice-daemon --state-root <场景状态根>` 进程。
/// 只去掉末尾的空段，不过滤中间的空参数。
fn matches_daemon_cmdline(cmdline: &[u8], expected_state_root: &[u8]) -> bool {
    if cmdline.is_empty() {
        return false;
    }
    let mut fields: Vec<&[u8]> = cmdline.split(|byte| *byte == 0).collect();
    if fields.last().is_some_and(|part| part.is_empty()) {
        fields.pop();
    }
    if fields.len() < 3 {
        return false;
    }
    let exe_name = fields[0].rsplit(|byte| *byte == b'/').next().unwrap_or(b"");
    if exe_name != b"lattice-daemon" {
        return false;
    }
    let mut rest = &fields[1..];
    let mut saw_state_root = false;
    let mut saw_drain_timeout = false;
    while !rest.is_empty() {
        let [flag, value, tail @ ..] = rest else {
            return false;
        };
        if value.is_empty() {
            return false;
        }
        match *flag {
            b"--state-root" => {
                if saw_state_root || *value != expected_state_root {
                    return false;
                }
                saw_state_root = true;
            }
            b"--drain-timeout" => {
                if saw_drain_timeout {
                    return false;
                }
                saw_drain_timeout = true;
            }
            _ => return false,
        }
        rest = tail;
    }
    saw_state_root
}

/// 以非阻塞 `flock` 试探存储布局公开的实例锁并立即放开；锁文件不存在视为已释放。
fn owner_lock_held(state_root: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(state_root.join("lattice/local/owner.lock"))
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    // SAFETY: `flock` 作用于本函数刚打开的文件描述；显式 `LOCK_UN` 避免并行测试线程 `fork` 后、`exec` 前短暂共享描述而延长试探锁（同 `ArtifactClaim`，#228）。
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        return Ok(false);
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        Ok(true)
    } else {
        Err(error)
    }
}

fn finish(errors: Vec<String>) -> Result<(), Vec<String>> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// `daemon stop` 干净退出 1 且报告 `not_running`：后台已不再接收连接。
fn already_stopped(outcome: &CommandOutcome) -> bool {
    outcome.is_clean_exit(1)
        && serde_json::from_slice::<serde_json::Value>(&outcome.stdout)
            .is_ok_and(|line| line["type"] == "error" && line["kind"] == "not_running")
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Stdio};

    use super::*;

    fn short_tempdir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("lsh-")
            .tempdir_in("/tmp")
            .expect("在 /tmp 创建短路径临时目录")
    }

    /// 编译一次复用的最小 ELF `lattice-daemon` 替身二进制，使 `/proc/<pid>/cmdline` 与真实后台一致。
    fn fake_daemon_binary() -> &'static Path {
        static BIN: std::sync::OnceLock<(tempfile::TempDir, PathBuf)> = std::sync::OnceLock::new();
        &BIN.get_or_init(|| {
            let dir = short_tempdir();
            let src = dir.path().join("fake_daemon.rs");
            let out = dir.path().join("lattice-daemon");
            std::fs::write(
                &src,
                r#"
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

fn main() {
    if let Some(coord) = std::env::var_os("FAKE_DAEMON_COORD").map(PathBuf::from) {
        std::fs::write(coord.join("serve-started"), b"").expect("写入启动标记");
        while !coord.join("status-probed").exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(50));
        eprintln!("错误：状态根已被另一个后台占用（对端进程号不可用）");
        std::process::exit(3);
    }
    if std::env::var_os("FAKE_DAEMON_SPAWN_CHILD").is_some() {
        let _ = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("启动同组子进程");
    }
    if let Some(ready) = std::env::var_os("FAKE_DAEMON_READY") {
        std::fs::write(ready, b"").expect("写入就绪标记");
    }
    if let Some(secs) = std::env::var("FAKE_DAEMON_SLEEP_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        std::thread::sleep(Duration::from_secs(secs));
    }
    if let Some(trigger) = std::env::var_os("FAKE_DAEMON_TRIGGER").map(PathBuf::from) {
        while !trigger.exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
"#,
            )
            .expect("写入假后台源码");
            let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
            let status = Command::new(rustc)
                .arg("--edition=2024")
                .arg(&src)
                .arg("-o")
                .arg(&out)
                .status()
                .expect("调用 rustc 编译假后台");
            assert!(status.success(), "编译假后台二进制失败: {status}");
            let canon = std::fs::canonicalize(&out).expect("规范化假后台二进制路径");
            (dir, canon)
        })
        .1
    }

    /// 直接子进程形态的受控假后台：启动在独立进程组中，派生同组子进程后可按触发文件先行退出组首领。
    struct DirectFakeDaemon {
        _dir: tempfile::TempDir,
        trigger: PathBuf,
        guard_members: Vec<OwnedProcess>,
    }

    impl DirectFakeDaemon {
        fn spawn_into(own_session: bool) -> (Self, ScenarioDaemon, u32) {
            let dir = short_tempdir();
            let cli_binary = std::fs::canonicalize("/bin/false").expect("解析假 CLI 路径");
            let daemon_binary = fake_daemon_binary();
            let state_root = dir.path().join("state");
            std::fs::create_dir_all(&state_root).expect("创建状态根");
            let ready = dir.path().join("ready");
            let trigger = dir.path().join("trigger");

            let deadline = Instant::now() + Duration::from_secs(5);
            let mut command = Command::new(daemon_binary);
            command
                .arg("--state-root")
                .arg(&state_root)
                .current_dir(dir.path())
                .env("FAKE_DAEMON_SPAWN_CHILD", "1")
                .env("FAKE_DAEMON_READY", &ready)
                .env("FAKE_DAEMON_TRIGGER", &trigger);
            if own_session {
                // SAFETY: `fork` 后仅调用异步信号安全的 `setsid`。
                unsafe {
                    command.pre_exec(|| {
                        if libc::setsid() < 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
            }
            let child = DaemonChild::spawn(command).expect("启动假后台失败");
            while !ready.exists() {
                assert!(Instant::now() < deadline, "假后台未在截止时间内就绪");
                std::thread::sleep(Duration::from_millis(5));
            }
            let pgid = child.pgid();
            let leader = OwnedProcess::open(child.pid()).expect("打开组首领 pidfd");
            let pids = processes_in_pgid(pgid).expect("读取假后台进程组");
            let members = capture_members(pgid, &pids).expect("捕获假后台组成员");
            let guard_members = capture_members(pgid, &pids).expect("捕获兜底清理句柄");

            let daemon = ScenarioDaemon::new(state_root);
            *daemon.state.lock().expect("自有后台锁") = DaemonState::Unreaped(UnreapedDaemon {
                cli_binary,
                child,
                pgid,
                leader,
                members,
            });
            (
                Self {
                    _dir: dir,
                    trigger,
                    guard_members,
                },
                daemon,
                pgid,
            )
        }

        fn trigger_leader_exit(&self, pgid: u32) {
            std::fs::write(&self.trigger, b"exit").expect("写入退出触发文件");
            let deadline = Instant::now() + Duration::from_secs(2);
            while process_stat(pgid).expect("查询首领状态").is_some() {
                assert!(Instant::now() < deadline, "假后台首领未在期限内退出");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    impl Drop for DirectFakeDaemon {
        fn drop(&mut self) {
            for member in &self.guard_members {
                let _ = member.kill();
            }
        }
    }

    /// 非自有残留后台夹具：以独立会话运行 `lattice-daemon --state-root`，模拟测试套件之外遗留的后台。
    struct UnownedResidualDaemon {
        _dir: tempfile::TempDir,
        state_root: PathBuf,
        child: Child,
        members: Vec<OwnedProcess>,
    }

    impl UnownedResidualDaemon {
        fn spawn() -> Self {
            let dir = short_tempdir();
            let daemon_binary = fake_daemon_binary();
            let state_root = dir.path().join("state");
            std::fs::create_dir_all(&state_root).expect("创建状态根");
            let ready = dir.path().join("ready");
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut command = Command::new(daemon_binary);
            command
                .arg("--state-root")
                .arg(&state_root)
                .current_dir(dir.path())
                .env("FAKE_DAEMON_READY", &ready)
                .env("FAKE_DAEMON_SLEEP_SECS", "30")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            // SAFETY: `fork` 后仅调用异步信号安全的 `setsid`。
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let child = command.spawn().expect("启动非自有假后台失败");
            while !ready.exists() {
                assert!(Instant::now() < deadline, "非自有假后台未就绪");
                std::thread::sleep(Duration::from_millis(5));
            }
            let pid = child.id();
            let members = capture_members(pid, &processes_in_pgid(pid).expect("查询进程组"))
                .expect("捕获非自有实例");
            Self {
                _dir: dir,
                state_root,
                child,
                members,
            }
        }
    }

    impl Drop for UnownedResidualDaemon {
        fn drop(&mut self) {
            for member in &self.members {
                let _ = member.kill();
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    fn wait_group_empty(pgid: u32) -> Vec<u32> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let members = processes_in_pgid(pgid).expect("检查进程组");
            if members.is_empty() || Instant::now() >= deadline {
                return members;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn direct_child_group_is_cleaned_after_its_leader_exits() {
        let (fixture, daemon, pgid) = DirectFakeDaemon::spawn_into(false);
        assert_eq!(daemon.owners().expect("查询存活首领"), vec![pgid]);

        fixture.trigger_leader_exit(pgid);
        assert!(daemon.owners().expect("首领退出后不应仍在列").is_empty());
        let orphans = processes_in_pgid(pgid).expect("查询残留成员");
        assert!(
            !orphans.is_empty() && !orphans.contains(&pgid),
            "夹具应留下不含组首领的同组子进程: {orphans:?}"
        );

        let errors = daemon
            .cleanup(Instant::now() + Duration::from_secs(30))
            .expect_err("组首领提前退出并触发强制清理时必须报告错误");
        assert!(
            errors.iter().any(|error| error.contains("组首领已退出")),
            "{errors:?}"
        );
        assert!(
            errors.iter().any(|error| error.contains("强制清理")),
            "{errors:?}"
        );
        assert!(
            !errors.iter().any(|error| error.contains("仍有残留成员")),
            "{errors:?}"
        );
        assert!(
            wait_group_empty(pgid).is_empty(),
            "清理后自有进程组不应残留任何成员"
        );
    }

    #[test]
    fn failed_reap_inspection_preserves_late_descendants_for_cleanup_and_drop() {
        for explicit_cleanup in [false, true] {
            let (fixture, daemon, pgid) = DirectFakeDaemon::spawn_into(false);
            // 就绪时可能只登记了组首领，后续派生的同组后代尚未被查询捕获。
            {
                let mut state = daemon.state.lock().expect("自有后台锁");
                let DaemonState::Unreaped(owned) = &mut *state else {
                    unreachable!()
                };
                owned.members.retain(|member| member.pid == pgid);
            }
            fixture.trigger_leader_exit(pgid);
            let mut fail_scan = |_| {
                Err(CleanupError {
                    operation: CleanupOperation::InspectProcessGroup(
                        ProcessInspectionOperation::ReadProc,
                    ),
                    kind: std::io::ErrorKind::PermissionDenied,
                    raw_os_error: Some(libc::EACCES),
                    message: "回收前模拟进程组扫描失败".into(),
                })
            };
            let mut errors = Vec::new();
            let retained_child = {
                let mut state = daemon.state.lock().expect("自有后台锁");
                reap_unreaped_state(&mut state, &mut fail_scan, "回收失败", &mut errors);
                matches!(*state, DaemonState::Unreaped(_))
            };
            // 显式清理继续模拟扫描故障；另一分支由恢复扫描后的 Drop 清理。
            let cleanup = explicit_cleanup
                .then(|| daemon.cleanup_with_inspection(Instant::now(), &mut fail_scan));
            drop(daemon);
            // 先观察清理结果，再让 fixture 的兜底 Drop 运行，避免兜底掩盖泄漏。
            let survivors = wait_group_empty(pgid);
            assert!(
                survivors.is_empty(),
                "扫描失败后不得遗漏未缓存的同组后代（显式清理={explicit_cleanup}）: {survivors:?}"
            );
            assert!(retained_child, "扫描失败时必须保留未回收的首进程句柄");
            assert!(
                errors
                    .iter()
                    .any(|error| error.contains("回收前模拟进程组扫描失败")),
                "必须报告原始扫描错误: {errors:?}"
            );
            if let Some(cleanup) = cleanup {
                assert!(cleanup.is_err(), "后代被强制清理时不得报告干净停止");
            }
        }
    }

    #[test]
    fn group_inspection_error_cleans_confirmed_members_via_pidfd_and_reports_failure() {
        let (fixture, daemon, pgid) = DirectFakeDaemon::spawn_into(false);
        fixture.trigger_leader_exit(pgid);
        let orphans = processes_in_pgid(pgid).expect("查询残留成员");
        assert!(!orphans.is_empty());

        let mut fail_scan = |_| {
            Err(CleanupError {
                operation: CleanupOperation::InspectProcessGroup(
                    ProcessInspectionOperation::ReadProc,
                ),
                kind: std::io::ErrorKind::PermissionDenied,
                raw_os_error: Some(libc::EACCES),
                message: "受控模拟的进程组检查错误".into(),
            })
        };
        let errors = daemon
            .cleanup_with_inspection(Instant::now(), &mut fail_scan)
            .expect_err("进程组检查失败时必须照样报告错误");
        assert!(
            errors
                .iter()
                .any(|error| error.contains("受控模拟的进程组检查错误")),
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("无法确认其余同组成员")),
            "{errors:?}"
        );
        assert!(
            wait_group_empty(pgid).is_empty(),
            "检查出错时也必须经已持有 pidfd 终止并回收确定自有的成员"
        );
        assert!(
            daemon.take_outcome().is_some(),
            "组首领应在 pidfd 清理中一并完成回收"
        );
    }

    #[test]
    fn stale_reaped_group_witness_does_not_kill_unconfirmed_members() {
        let dir = short_tempdir();
        let daemon_binary = fake_daemon_binary();
        let state_root = dir.path().join("state");
        std::fs::create_dir_all(&state_root).expect("创建状态根");
        let ready = dir.path().join("ready");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut command = Command::new(daemon_binary);
        command
            .arg("--state-root")
            .arg(&state_root)
            .current_dir(dir.path())
            .env("FAKE_DAEMON_SPAWN_CHILD", "1")
            .env("FAKE_DAEMON_READY", &ready)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut leader_child = command.spawn().expect("启动替代组失败");
        while !ready.exists() {
            assert!(Instant::now() < deadline, "替代组未就绪");
            std::thread::sleep(Duration::from_millis(5));
        }
        let pgid = leader_child.id();
        let status = leader_child.wait().expect("回收替代组首领");
        assert!(status.success());
        let orphans = processes_in_pgid(pgid).expect("查询替代组孤儿");
        assert!(!orphans.is_empty() && !orphans.contains(&pgid));
        let orphan_guards = capture_members(pgid, &orphans).expect("捕获替代组清理句柄");

        let (old_fixture, old_daemon, old_pgid) = DirectFakeDaemon::spawn_into(false);
        let mut stale_witness = OwnedProcess::open(old_pgid).expect("打开旧代首领实例");
        old_fixture.trigger_leader_exit(old_pgid);
        let _ = old_daemon.cleanup(Instant::now());
        assert!(wait_group_empty(old_pgid).is_empty());

        stale_witness.pid = orphans[0];
        let daemon = ScenarioDaemon::new(state_root);
        *daemon.state.lock().expect("自有后台锁") = DaemonState::ReapedResiduals(ResidualDaemon {
            pgid,
            members: vec![stale_witness],
            outcome: None,
            stderr: Vec::new(),
        });

        let errors = daemon
            .cleanup(Instant::now())
            .expect_err("归属无法由存活实例见证时必须报错");
        let remaining = processes_in_pgid(pgid).expect("复核替代组成员");
        for guard in &orphan_guards {
            let _ = guard.kill();
        }
        let _ = wait_group_empty(pgid);
        assert_eq!(
            remaining, orphans,
            "旧代已失效的见证句柄不得授权杀死当前进程组成员: {errors:?}"
        );
        assert!(
            errors.iter().any(|error| error.contains("归属未确认")),
            "{errors:?}"
        );
    }

    #[test]
    fn inspection_delay_cannot_renew_normal_stop_budget() {
        let (_fixture, daemon, pgid) = DirectFakeDaemon::spawn_into(false);
        let deadline = Instant::now() + Duration::from_millis(100);
        let mut scanned = false;
        let mut inspect = |target_pgid| {
            if !scanned {
                assert!(Instant::now() < deadline, "首次检查启动时预算尚未到期");
                scanned = true;
                std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            }
            processes_in_pgid(target_pgid)
        };
        let errors = daemon
            .cleanup_with_inspection(deadline, &mut inspect)
            .expect_err("检查耗尽预算后应报告正常停止失败");
        assert!(scanned);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("正常停止后台前场景预算已耗尽")),
            "{errors:?}"
        );
        assert!(
            !errors
                .iter()
                .any(|error| error.starts_with("正常停止后台失败")
                    || error.starts_with("执行 daemon stop")),
            "预算耗尽后不得再启动正常 daemon stop 命令: {errors:?}"
        );
        assert!(wait_group_empty(pgid).is_empty());
    }

    #[test]
    fn exhausted_normal_budget_is_recorded_before_forced_cleanup() {
        let (_fixture, daemon, pgid) = DirectFakeDaemon::spawn_into(false);
        assert_eq!(daemon.owners().expect("查询存活首领"), vec![pgid]);

        let started = Instant::now();
        let errors = daemon
            .cleanup(Instant::now())
            .expect_err("预算耗尽时必须报告正常停止失败并执行强制清理");
        assert!(
            errors
                .iter()
                .any(|error| error.contains("正常停止后台前场景预算已耗尽")),
            "{errors:?}"
        );
        assert!(
            !errors
                .iter()
                .any(|error| error.starts_with("正常停止后台失败")
                    || error.starts_with("执行 daemon stop")),
            "零剩余预算时不得运行 daemon stop: {errors:?}"
        );
        assert!(
            errors.iter().any(|error| error.contains("强制清理")),
            "{errors:?}"
        );
        assert!(
            started.elapsed() < FORCED_CLEANUP_GRACE + Duration::from_secs(3),
            "强制清理等待超时: {:?}",
            started.elapsed()
        );
        assert!(wait_group_empty(pgid).is_empty());
    }

    #[test]
    fn unowned_residual_daemon_is_reported_without_killing() {
        let residual = UnownedResidualDaemon::spawn();
        let pid = residual.child.id();
        let daemon = ScenarioDaemon::new(residual.state_root.clone());

        let errors = daemon
            .cleanup(Instant::now() + Duration::from_secs(5))
            .expect_err("未归属的残留后台必须让清理检查失败");
        assert!(
            errors
                .iter()
                .any(|error| error.contains("不属于测试套件的残留 `lattice-daemon` 后台进程")),
            "{errors:?}"
        );
        assert!(
            process_stat(pid).expect("检查非自有后台状态").is_some(),
            "残留检查只负责发现并报错，不得误杀非自有进程"
        );
    }

    #[test]
    fn daemon_cmdline_matches_drain_timeout_in_either_order_and_rejects_malformed_argv() {
        fn nul_cmdline(parts: &[&[u8]]) -> Vec<u8> {
            let mut out = Vec::new();
            for part in parts {
                out.extend_from_slice(part);
                out.push(0);
            }
            out
        }

        let root = b"/tmp/scenario-state-root";
        for accepted in [
            nul_cmdline(&[b"/bin/lattice-daemon", b"--state-root", root]),
            nul_cmdline(&[b"lattice-daemon", b"--state-root", root]),
            nul_cmdline(&[
                b"/bin/lattice-daemon",
                b"--state-root",
                root,
                b"--drain-timeout",
                b"300ms",
            ]),
            nul_cmdline(&[
                b"/bin/lattice-daemon",
                b"--drain-timeout",
                b"not-validated-here",
                b"--state-root",
                root,
            ]),
        ] {
            assert!(
                super::matches_daemon_cmdline(&accepted, root),
                "应识别合法后台命令行参数: {accepted:?}"
            );
        }

        for rejected in [
            Vec::new(),
            nul_cmdline(&[b"/bin/lattice-daemon"]),
            nul_cmdline(&[b"/bin/lattice", b"--state-root", root]),
            nul_cmdline(&[b"/bin/lattice", b"daemon", b"serve", b"--state-root", root]),
            nul_cmdline(&[b"/bin/lattice-daemon/", b"--state-root", root]),
            nul_cmdline(&[b"/bin/lattice-daemon", b"--drain-timeout", b"60s"]),
            nul_cmdline(&[b"/bin/lattice-daemon", b"--state-root", b"/tmp/other-root"]),
            nul_cmdline(&[b"/bin/lattice-daemon", b"--state-root", root, b"--verbose"]),
            nul_cmdline(&[
                b"/bin/lattice-daemon",
                b"--state-root",
                root,
                b"--unknown",
                b"1",
            ]),
            nul_cmdline(&[
                b"/bin/lattice-daemon",
                b"--state-root",
                root,
                b"--state-root",
                root,
            ]),
            nul_cmdline(&[
                b"/bin/lattice-daemon",
                b"--state-root",
                root,
                b"--drain-timeout",
                b"1s",
                b"--drain-timeout",
                b"2s",
            ]),
            nul_cmdline(&[
                b"/bin/lattice-daemon",
                b"--state-root",
                root,
                b"--drain-timeout",
            ]),
            nul_cmdline(&[
                b"/bin/lattice-daemon",
                b"--drain-timeout",
                b"1s",
                b"--state-root",
            ]),
            nul_cmdline(&[
                b"/bin/lattice-daemon",
                b"--state-root",
                root,
                b"extra-positional",
            ]),
            nul_cmdline(&[
                b"/bin/lattice-daemon",
                b"--state-root",
                root,
                b"extra-a",
                b"extra-b",
            ]),
            nul_cmdline(&[b"/bin/lattice-daemon", b"", b"--state-root", root]),
            nul_cmdline(&[b"/bin/lattice-daemon", b"--state-root", root, b""]),
            nul_cmdline(&[
                b"/bin/lattice-daemon",
                b"--state-root",
                root,
                b"--drain-timeout",
                b"",
            ]),
        ] {
            assert!(
                !super::matches_daemon_cmdline(&rejected, root),
                "不应识别异常命令行参数: {rejected:?}"
            );
        }
    }

    const SOCKET_HELPER_SOCK_ENV: &str = "LATTICE_TEST_SOCKET_PATH";
    const SOCKET_HELPER_LOCK_ENV: &str = "LATTICE_TEST_LOCK_PATH";
    const SOCKET_HELPER_READY_ENV: &str = "LATTICE_TEST_READY_PATH";
    const SOCKET_HELPER_STOP_ENV: &str = "LATTICE_TEST_STOP_PATH";

    #[test]
    #[ignore = "供套接字测试自执行复用的辅助入口"]
    fn socket_listener_helper() {
        let Some(sock_path) = std::env::var_os(SOCKET_HELPER_SOCK_ENV).map(PathBuf::from) else {
            return;
        };
        let _lock_guard = std::env::var_os(SOCKET_HELPER_LOCK_ENV).map(|lock_path| {
            let lock_path = PathBuf::from(lock_path);
            if let Some(parent) = lock_path.parent() {
                std::fs::create_dir_all(parent).expect("创建锁目录");
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&lock_path)
                .expect("打开实例锁");
            // SAFETY: `file` 在整个辅助进程存活期间保持打开以维持独占锁。
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            assert_eq!(rc, 0, "辅助进程获取 owner.lock 失败");
            file
        });
        if let Some(parent) = sock_path.parent() {
            std::fs::create_dir_all(parent).expect("创建套接字目录");
        }
        let _ = std::fs::remove_file(&sock_path);
        let listener = std::os::unix::net::UnixListener::bind(&sock_path).expect("绑定测试套接字");
        listener.set_nonblocking(true).expect("设置非阻塞监听");
        if let Some(ready) = std::env::var_os(SOCKET_HELPER_READY_ENV) {
            std::fs::write(ready, b"ready").expect("写入就绪标记");
        }
        let stop_path = std::env::var_os(SOCKET_HELPER_STOP_ENV).map(PathBuf::from);
        let stop_at = Instant::now() + Duration::from_secs(30);
        while stop_path.as_ref().is_none_or(|p| !p.exists()) && Instant::now() < stop_at {
            while listener.accept().is_ok() {}
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = std::fs::remove_file(&sock_path);
    }

    #[test]
    fn socket_peer_pid_matches_owned_child_and_rejects_other_listener() {
        let dir = short_tempdir();
        let sock_path = dir.path().join("api.sock");
        let ready_path = dir.path().join("listener-ready");
        let stop_path = dir.path().join("listener-stop");
        let exe = std::env::current_exe().expect("当前测试二进制");

        assert_eq!(
            verify_socket_owner(
                &sock_path,
                std::process::id(),
                Instant::now() + Duration::from_secs(1)
            )
            .expect("未创建套接字核验"),
            SocketOwner::NotListening
        );

        let mut listener_child = Command::new(&exe)
            .args([
                "--exact",
                "daemon::tests::socket_listener_helper",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(SOCKET_HELPER_SOCK_ENV, &sock_path)
            .env(SOCKET_HELPER_READY_ENV, &ready_path)
            .env(SOCKET_HELPER_STOP_ENV, &stop_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("启动监听子进程");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready_path.exists() {
            assert!(Instant::now() < deadline, "监听子进程未在期限内就绪");
            std::thread::sleep(Duration::from_millis(5));
        }

        let mut other_child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("启动对照子进程");

        let listener_pid = listener_child.id();
        let other_pid = other_child.id();
        assert_eq!(
            verify_socket_owner(&sock_path, listener_pid, deadline).expect("自有子进程核验"),
            SocketOwner::Owned
        );
        assert_eq!(
            verify_socket_owner(&sock_path, other_pid, deadline).expect("非自有监听者核验"),
            SocketOwner::Unowned { listener_pid }
        );

        let _ = other_child.kill();
        let _ = other_child.wait();
        std::fs::write(&stop_path, b"1").expect("停止监听子进程");
        let _ = listener_child.wait();
    }

    #[test]
    fn external_daemon_winning_after_precheck_is_rejected_without_stopping_it() {
        let dir = short_tempdir();
        let state_root = dir.path().join("state");
        let coord = dir.path().join("coord");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&state_root).expect("创建状态根");
        std::fs::create_dir_all(&coord).expect("创建协调目录");
        std::fs::create_dir_all(&project).expect("创建项目目录");

        let wrapper = dir.path().join("fake-lattice");
        let script = format!(
            "#!/bin/sh\nCOORD='{}'\nif [ \"${{1:-}}\" = '--json' ] && [ \"${{2:-}}\" = 'daemon' ] && [ \"${{3:-}}\" = 'status' ]; then\n  if [ -e \"$COORD/external-listening\" ]; then\n    : > \"$COORD/status-probed\"\n    printf '{{\"stopping\":false}}\\n'\n    exit 0\n  fi\n  printf '{{\"type\":\"error\",\"kind\":\"not_running\"}}\\n'\n  exit 1\nfi\nif [ \"${{1:-}}\" = '--json' ] && [ \"${{2:-}}\" = 'daemon' ] && [ \"${{3:-}}\" = 'stop' ]; then\n  : > \"$COORD/stop-invoked\"\n  printf '{{\"owners\":0}}\\n'\n  exit 0\nfi\nexit 2\n",
            coord.display(),
        );
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&wrapper, script).expect("写入假 CLI 脚本");
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))
                .expect("设置脚本可执行权限");
        }
        let wrapper = std::fs::canonicalize(&wrapper).expect("规范化假 CLI 路径");
        let daemon_binary = fake_daemon_binary();
        let exe = std::env::current_exe().expect("当前测试二进制");

        let daemon = ScenarioDaemon::new(state_root.clone());
        std::thread::scope(|scope| {
            let coord_bg = coord.clone();
            let state_bg = state_root.clone();
            let exe_bg = exe.clone();
            let external_join = scope.spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(5);
                while !coord_bg.join("serve-started").exists() {
                    assert!(Instant::now() < deadline, "自有子进程未启动");
                    std::thread::sleep(Duration::from_millis(5));
                }
                let mut external = Command::new(&exe_bg);
                external
                    .args([
                        "--exact",
                        "daemon::tests::socket_listener_helper",
                        "--ignored",
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .env(
                        SOCKET_HELPER_LOCK_ENV,
                        state_bg.join("lattice/local/owner.lock"),
                    )
                    .env(
                        SOCKET_HELPER_SOCK_ENV,
                        state_bg.join("lattice/local/api.sock"),
                    )
                    .env(SOCKET_HELPER_READY_ENV, coord_bg.join("external-listening"))
                    .env(SOCKET_HELPER_STOP_ENV, coord_bg.join("shutdown-external"))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                // SAFETY: `fork` 后仅调用异步信号安全的 `setsid`。
                unsafe {
                    external.pre_exec(|| {
                        if libc::setsid() < 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
                external.spawn().expect("启动外部获胜后台")
            });

            let mut env_snapshot = HashMap::new();
            if let Ok(path_env) = std::env::var("PATH") {
                env_snapshot.insert("PATH".into(), path_env);
            }
            env_snapshot.insert("FAKE_DAEMON_COORD".into(), coord.display().to_string());
            let start_res = daemon.start(
                &wrapper,
                daemon_binary,
                &project,
                env_snapshot,
                &DaemonOptions::default(),
                Instant::now() + Duration::from_secs(5),
            );
            let mut external_child = external_join.join().expect("等待外部启动线程");
            let external_pid = external_child.id();

            let err = match start_res {
                Err(err) => err,
                Ok(()) => {
                    let _ = std::fs::write(coord.join("shutdown-external"), b"1");
                    let _ = external_child.kill();
                    let _ = external_child.wait();
                    panic!("外部后台抢先获胜时 start() 必须拒绝就绪，实际返回了 Ok(())");
                }
            };

            assert!(
                daemon.pgid().is_none() && daemon.owners().expect("查询归属").is_empty(),
                "start() 拒绝非自有后台时不得接管归属: {err:?}"
            );
            assert!(
                !coord.join("stop-invoked").exists(),
                "不得向外部获胜后台发送 daemon stop"
            );
            assert!(
                external_child
                    .try_wait()
                    .expect("检查外部后台存活")
                    .is_none(),
                "外部获胜后台必须仍在运行: {err:?}"
            );
            drop(daemon);
            assert!(
                !coord.join("stop-invoked").exists(),
                "丢弃 ScenarioDaemon 时也不得停止外部后台"
            );
            assert!(
                external_child
                    .try_wait()
                    .expect("丢弃后检查外部后台存活")
                    .is_none(),
                "丢弃 ScenarioDaemon 后外部后台必须仍在运行"
            );

            std::fs::write(coord.join("shutdown-external"), b"1").expect("通知外部后台退出");
            let _ = external_child.wait();
            match &err {
                DaemonError::UnownedDaemonReady {
                    listener_pid,
                    exit_code,
                    stderr,
                    ..
                } => {
                    assert_eq!(*listener_pid, Some(external_pid));
                    assert_eq!(*exit_code, Some(3));
                    assert!(stderr.contains("状态根已被另一个后台占用"), "{stderr:?}");
                }
                other => panic!("应返回专门的非自有后台占用错误，实际为: {other:?}"),
            }
        });
    }

    #[test]
    fn has_exited_uses_pidfd_without_reaping_group_leader() {
        let dir = short_tempdir();
        let cli_binary = std::fs::canonicalize("/bin/false").expect("解析假 CLI 路径");
        let daemon_binary = fake_daemon_binary();
        let state_root = dir.path().join("state");
        std::fs::create_dir_all(&state_root).expect("创建状态根");
        let ready = dir.path().join("ready");
        let trigger = dir.path().join("trigger");

        let mut cmd = Command::new(daemon_binary);
        cmd.arg("--state-root")
            .arg(&state_root)
            .current_dir(dir.path())
            .env("FAKE_DAEMON_READY", &ready)
            .env("FAKE_DAEMON_TRIGGER", &trigger);
        let child = DaemonChild::spawn(cmd).expect("启动假后台");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() {
            assert!(Instant::now() < deadline, "假后台未就绪");
            std::thread::sleep(Duration::from_millis(5));
        }
        let pgid = child.pgid();
        let leader = OwnedProcess::open(child.pid()).expect("打开组首领 pidfd");
        let members = capture_members(pgid, &[pgid]).expect("捕获组首领成员");
        let daemon = ScenarioDaemon::new(state_root);
        *daemon.state.lock().expect("自有后台锁") = DaemonState::Unreaped(UnreapedDaemon {
            cli_binary,
            child,
            pgid,
            leader,
            members,
        });

        assert!(!daemon.has_exited().expect("退出前检查"));
        std::fs::write(&trigger, b"exit").expect("触发组首领退出");
        while !daemon.has_exited().expect("轮询退出状态") {
            assert!(Instant::now() < deadline, "组首领未在期限内退出");
            std::thread::sleep(Duration::from_millis(5));
        }

        // `has_exited()` 只读 `pidfd`，不得调用 `wait` 回收子进程：内核中仍应保留僵尸态子进程。
        // SAFETY: `info` 清零初始化，`WNOWAIT` 只观察子进程退出态而不消费它。
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pgid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        assert_eq!(
            rc,
            0,
            "has_exited() 不得提前回收组首领: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: `waitid` 返回 0 且找到目标僵尸子进程时 `si_pid` 字段有效。
        assert_eq!(unsafe { info.si_pid() } as u32, pgid);

        let outcome = daemon
            .wait(Some(Duration::from_secs(2)))
            .expect("正式回收组首领");
        assert_eq!(outcome.exit_code(), Some(0));
    }

    #[test]
    fn reaped_daemon_handle_never_signals_or_claims_reused_pgid() {
        // 情形一：子进程与同组成员全部退出并经 `wait` 正式回收（进入 `Reaped`），句柄仍被持有。
        let (fixture, daemon, pgid) = DirectFakeDaemon::spawn_into(false);
        fixture.trigger_leader_exit(pgid);
        for guard in &fixture.guard_members {
            let _ = guard.kill();
        }
        assert!(wait_group_empty(pgid).is_empty());
        let outcome = daemon
            .wait(Some(Duration::from_secs(2)))
            .expect("回收已退出组首领");
        assert_eq!(outcome.exit_code(), Some(0));
        assert!(daemon.has_exited().expect("回收后查询退出状态"));

        let mut victim_cmd = Command::new("sleep");
        victim_cmd
            .arg("30")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut victim = victim_cmd.spawn().expect("启动模拟复用组号的无关进程");
        let victim_pid = victim.id();
        let victim_guard = OwnedProcess::open(victim_pid).expect("打开无关进程兜底句柄");

        assert!(daemon.owners().expect("已回收后查询归属").is_empty());
        assert!(matches!(daemon.terminate(), Err(DaemonError::NotRunning)));
        assert!(matches!(daemon.interrupt(), Err(DaemonError::NotRunning)));
        assert!(matches!(
            daemon.kill_and_wait(),
            Err(DaemonError::NotRunning)
        ));

        let mut fake_reused_scan = |_| Ok(vec![victim_pid]);
        let cleanup_res = daemon.cleanup_with_inspection(Instant::now(), &mut fake_reused_scan);
        assert!(
            cleanup_res.is_ok(),
            "已回收后台面对假检查返回的复用组进程不得报错或误认领: {cleanup_res:?}"
        );
        drop(daemon);
        assert!(
            victim.try_wait().expect("检查无关进程存活").is_none(),
            "已回收句柄不得向数值进程组发信号或杀死复用组进程"
        );

        // 情形二：组首领已回收时曾进入 `ReapedResiduals`，随后原组成员也退出、数值组号被 `victim` 真实占用。
        let (old_fixture, old_daemon, old_pgid) = DirectFakeDaemon::spawn_into(false);
        let dead_member = OwnedProcess::open(old_pgid).expect("打开旧成员 pidfd");
        old_fixture.trigger_leader_exit(old_pgid);
        let _ = old_daemon.cleanup(Instant::now());
        assert!(wait_group_empty(old_pgid).is_empty());

        let dir = short_tempdir();
        let state_root = dir.path().join("state");
        std::fs::create_dir_all(&state_root).expect("创建状态根");
        let make_stale_residual = || {
            let d = ScenarioDaemon::new(state_root.clone());
            *d.state.lock().expect("自有后台锁") = DaemonState::ReapedResiduals(ResidualDaemon {
                pgid: victim_pid,
                members: vec![OwnedProcess {
                    pid: victim_pid,
                    fd: dead_member.fd.try_clone().expect("克隆过期成员 pidfd"),
                }],
                outcome: None,
                stderr: Vec::new(),
            });
            d
        };

        let d_term = make_stale_residual();
        assert!(
            d_term
                .owners()
                .expect("残留态过期见证下查询归属")
                .is_empty()
        );
        assert!(matches!(d_term.terminate(), Err(DaemonError::NotRunning)));

        let d_int = make_stale_residual();
        assert!(matches!(d_int.interrupt(), Err(DaemonError::NotRunning)));

        let d_kill = make_stale_residual();
        assert!(matches!(
            d_kill.kill_and_wait(),
            Err(DaemonError::NotRunning)
        ));

        let d_clean = make_stale_residual();
        let clean_errs = d_clean
            .cleanup_with_inspection(Instant::now(), &mut fake_reused_scan)
            .expect_err("残留态见证已退出且组号出现新进程时应拒绝认领并报告归属未确认");
        assert!(
            clean_errs.iter().any(|e| e.contains("归属未确认")),
            "{clean_errs:?}"
        );

        let d_drop = make_stale_residual();
        drop(d_drop);

        assert!(
            victim.try_wait().expect("检查无关进程存活").is_none(),
            "terminate/interrupt/kill_and_wait/cleanup/Drop 均不得向已回收首领的数值组号发信号"
        );

        let _ = victim_guard.kill();
        let _ = victim.wait();
    }

    #[test]
    fn persistent_inspection_failure_cleans_confirmed_members_across_reap_and_drop() {
        let dir = short_tempdir();
        let cli_binary = std::fs::canonicalize("/bin/false").expect("解析假 CLI 路径");
        let daemon_binary = fake_daemon_binary();
        let state_root = dir.path().join("state");
        std::fs::create_dir_all(&state_root).expect("创建状态根");
        let ready = dir.path().join("ready");
        let trigger = dir.path().join("trigger");

        let mut cmd = Command::new(daemon_binary);
        cmd.arg("--state-root")
            .arg(&state_root)
            .current_dir(dir.path())
            .env("FAKE_DAEMON_SPAWN_CHILD", "1")
            .env("FAKE_DAEMON_READY", &ready)
            .env("FAKE_DAEMON_TRIGGER", &trigger);
        let child = DaemonChild::spawn(cmd).expect("启动假后台");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() {
            assert!(Instant::now() < deadline, "假后台未就绪");
            std::thread::sleep(Duration::from_millis(5));
        }
        let pgid = child.pgid();
        let leader = OwnedProcess::open(child.pid()).expect("打开组首领 pidfd");
        let pids = processes_in_pgid(pgid).expect("初始读取假后台进程组");
        assert!(pids.len() >= 2, "假后台应包含组首领与同组子进程: {pids:?}");
        let members = capture_members(pgid, &pids).expect("捕获已确认成员 pidfd");
        // 只读观察句柄：本测试不安装任何 `Drop` 兜底 `kill`，完全依靠 `ScenarioDaemon` 自身完成清理。
        let observers = capture_members(pgid, &pids).expect("打开只读观察 pidfd");

        let daemon = ScenarioDaemon::new(state_root);
        *daemon.state.lock().expect("自有后台锁") = DaemonState::Unreaped(UnreapedDaemon {
            cli_binary,
            child,
            pgid,
            leader,
            members,
        });

        // 先让组首领退出并经正式 `wait` 回收，使状态进入“组首领已回收但组内仍有残留”。
        std::fs::write(&trigger, b"exit").expect("触发组首领退出");
        let outcome = daemon
            .wait(Some(Duration::from_secs(2)))
            .expect("回收组首领");
        assert_eq!(outcome.exit_code(), Some(0));
        assert!(
            !processes_in_pgid(pgid)
                .expect("验证同组孤儿仍存活")
                .is_empty(),
            "组首领回收后同组子进程应仍在运行，等待后续清理"
        );

        let mut always_fail_scan = |_| {
            Err(CleanupError {
                operation: CleanupOperation::InspectProcessGroup(
                    ProcessInspectionOperation::ReadProc,
                ),
                kind: std::io::ErrorKind::PermissionDenied,
                raw_os_error: Some(libc::EACCES),
                message: "持续模拟 /proc 检查失败".into(),
            })
        };

        let errors = daemon
            .cleanup_with_inspection(Instant::now(), &mut always_fail_scan)
            .expect_err("检查持续失败时必须照样报告错误");
        assert!(
            errors.iter().any(|e| e.contains("持续模拟 /proc 检查失败")),
            "错误报告应包含进程组检查失败原因: {errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("无法扫描确认其余同组成员")),
            "错误报告应写明无法确认其余成员的诊断: {errors:?}"
        );

        drop(daemon);

        assert!(
            wait_group_empty(pgid).is_empty(),
            "/proc 持续失败且组首领已回收时，经 pidfd 清理与 Drop 后不得留下自有进程残留"
        );
        for obs in &observers {
            assert!(
                obs.has_exited().expect("观察句柄检查退出"),
                "自有成员 pid={} 的内核进程实例必须已退出",
                obs.pid
            );
        }
    }

    #[test]
    fn verify_socket_owner_returns_within_deadline_when_listener_backlog_is_full() {
        let dir = short_tempdir();
        let sock_path = dir.path().join("full-backlog.sock");
        let (addr, addr_len) = unix_sockaddr(&sock_path).expect("构造测试套接字地址");

        // SAFETY: 创建本地流套接字并交由 `OwnedFd` 持有关闭责任。
        let raw_listener = unsafe {
            libc::socket(
                libc::AF_UNIX,
                libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                0,
            )
        };
        assert!(raw_listener >= 0, "创建监听套接字失败");
        // SAFETY: `raw_listener` 为独占的新建套接字描述符。
        let listener = unsafe { OwnedFd::from_raw_fd(raw_listener) };
        // SAFETY: `listener` 与 `addr` 均有效。
        assert_eq!(
            unsafe {
                libc::bind(
                    listener.as_raw_fd(),
                    (&addr as *const libc::sockaddr_un).cast(),
                    addr_len,
                )
            },
            0,
            "绑定测试监听套接字失败: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: 将监听队列设为最小长度 1，且本阶段绝不调用 `accept`。
        assert_eq!(
            unsafe { libc::listen(listener.as_raw_fd(), 1) },
            0,
            "启动监听失败: {}",
            std::io::Error::last_os_error()
        );

        let mut queued_clients = Vec::new();
        let mut backlog_saturated = false;
        for _ in 0..64 {
            // SAFETY: 创建非阻塞测试客户端套接字并由 `OwnedFd` 管理。
            let raw_client = unsafe {
                libc::socket(
                    libc::AF_UNIX,
                    libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                    0,
                )
            };
            assert!(raw_client >= 0, "创建测试客户端套接字失败");
            // SAFETY: `raw_client` 为独占的新建套接字描述符。
            let client = unsafe { OwnedFd::from_raw_fd(raw_client) };
            // SAFETY: `client` 与 `addr` 均有效。
            let rc = unsafe {
                libc::connect(
                    client.as_raw_fd(),
                    (&addr as *const libc::sockaddr_un).cast(),
                    addr_len,
                )
            };
            if rc == 0 {
                queued_clients.push(client);
                continue;
            }
            let err = std::io::Error::last_os_error();
            assert_eq!(
                err.kind(),
                std::io::ErrorKind::WouldBlock,
                "填满监听队列时应遇到 EAGAIN，实际为: {err}"
            );
            backlog_saturated = true;
            break;
        }
        assert!(backlog_saturated, "测试未能填满 Unix 域套接字监听队列");

        let budget = Duration::from_millis(80);
        let started = Instant::now();
        let owner = verify_socket_owner(&sock_path, std::process::id(), started + budget)
            .expect("队列已满时核验不应报致命错误");
        let elapsed = started.elapsed();
        assert_eq!(owner, SocketOwner::NotListening);
        assert!(
            elapsed >= Duration::from_millis(70) && elapsed < Duration::from_secs(1),
            "核验应在共享截止时间附近返回而非阻塞，实际耗时: {elapsed:?}"
        );

        // 截止时间内一旦监听端开始排空连接队列，重试应立即恢复并识别出正确监听者。
        std::thread::scope(|scope| {
            let listener_fd = listener.as_raw_fd();
            scope.spawn(move || {
                std::thread::sleep(Duration::from_millis(25));
                loop {
                    // SAFETY: 对有效监听套接字非阻塞或快速排空已排队连接，接受后立即关闭对端描述符。
                    let accepted = unsafe {
                        libc::accept4(
                            listener_fd,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                        )
                    };
                    if accepted < 0 {
                        break;
                    }
                    // SAFETY: `accepted` 为刚接受的有效描述符。
                    unsafe {
                        libc::close(accepted);
                    }
                }
            });
            let recovered = verify_socket_owner(
                &sock_path,
                std::process::id(),
                Instant::now() + Duration::from_secs(2),
            )
            .expect("队列排空后核验应成功");
            assert_eq!(recovered, SocketOwner::Owned);
        });
        drop(queued_clients);
    }

    #[test]
    fn kill_and_wait_surfaces_pidfd_errors_instead_of_masking_as_not_running() {
        let dir = short_tempdir();
        let state_root = dir.path().join("state");
        std::fs::create_dir_all(&state_root).expect("创建状态根");

        // 用真实但非 `pidfd` 的本地流套接字端构造 `OwnedProcess`：
        // 对端仍存活故 `poll(POLLIN, 0)` 返回未退出，而 `pidfd_send_signal` 确定返回 `EBADF`，且 `Drop` 关闭安全。
        let (non_pidfd_kill, _keep_peer_open) =
            std::os::unix::net::UnixStream::pair().expect("创建测试流对");
        let daemon_kill = ScenarioDaemon::new(state_root.clone());
        *daemon_kill.state.lock().expect("自有后台锁") =
            DaemonState::ReapedResiduals(ResidualDaemon {
                pgid: 999_991,
                members: vec![OwnedProcess {
                    pid: 999_991,
                    fd: OwnedFd::from(non_pidfd_kill),
                }],
                outcome: None,
                stderr: Vec::new(),
            });

        let mut empty_scan = |_| Ok(Vec::new());
        let err = daemon_kill
            .kill_and_wait_with_inspection(&mut empty_scan)
            .expect_err("pidfd 发送信号失败时不得伪装成 NotRunning");
        match err {
            DaemonError::Io(io_err) => {
                assert_eq!(io_err.raw_os_error(), Some(libc::EBADF), "{io_err:?}");
            }
            other => panic!("kill_and_wait 应如实返回 pidfd I/O 错误，实际为: {other:?}"),
        }

        let (non_pidfd_clean, _keep_clean_peer_open) =
            std::os::unix::net::UnixStream::pair().expect("创建测试流对");
        let daemon_clean = ScenarioDaemon::new(state_root);
        *daemon_clean.state.lock().expect("自有后台锁") =
            DaemonState::ReapedResiduals(ResidualDaemon {
                pgid: 999_992,
                members: vec![OwnedProcess {
                    pid: 999_992,
                    fd: OwnedFd::from(non_pidfd_clean),
                }],
                outcome: None,
                stderr: Vec::new(),
            });
        let clean_errs = daemon_clean
            .cleanup_with_inspection(Instant::now(), &mut empty_scan)
            .expect_err("cleanup 共享同一强制清理核心时也应汇总 pidfd 错误");
        assert!(
            clean_errs
                .iter()
                .any(|msg| msg.contains("经 pidfd 向自有后台成员发送 SIGKILL 失败")),
            "{clean_errs:?}"
        );
    }
}
