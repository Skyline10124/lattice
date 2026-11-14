// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::fmt;
use std::io::{Read, Seek, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::scenario::ScenarioSpec;
use crate::world::ScenarioWorld;

const LATTICE_BIN_ENV: &str = "LATTICE_BIN";
const LATTICE_DAEMON_BIN_ENV: &str = "LATTICE_DAEMON_BIN";
const TIMEOUT_POLL: Duration = Duration::from_millis(10);
const DRAIN_FINISH_GRACE: Duration = Duration::from_millis(250);

#[derive(Debug, Clone)]
pub struct LatticeDriver {
    binary: PathBuf,
}

#[derive(Debug, Clone)]
pub struct LatticeDaemonDriver {
    binary: PathBuf,
}

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("请将环境变量 {env} 设为已构建 {binary} 二进制的路径")]
    MissingBinaryEnv {
        env: &'static str,
        binary: &'static str,
    },
    #[error("{env} 指向的文件不存在: {path}")]
    BinaryNotFound { env: &'static str, path: String },
    #[error("{env} 指向的路径无法规范化: {path}（{reason}）")]
    BinaryCanonicalizeFailed {
        env: &'static str,
        path: String,
        reason: String,
    },
    #[error("{env} 指向的文件不可执行: {path}")]
    BinaryNotExecutable { env: &'static str, path: String },
    #[error("运行被测二进制失败: {0}")]
    Io(#[from] std::io::Error),
    #[error("命令输出读取线程发生 panic")]
    DrainThreadPanicked,
}

/// 直接子进程被观察到的内核终止结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessTermination {
    Exited(i32),
    Signaled(i32),
}

/// 测试套件主动发起的受控干预，按请求顺序保留。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessIntervention {
    InterruptRequested,
    TerminateRequested,
    TimedOut(Duration),
    ForcedKill,
}

/// Linux `/proc` 进程组检查中的单个阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessInspectionOperation {
    ReadProc,
    ReadProcEntry,
    ReadStat(u32),
    ParseStat(u32),
}

/// 收敛进程组清理时执行的可失败操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupOperation {
    SignalProcessGroup(i32),
    KillChild,
    WaitChild,
    InspectProcessGroup(ProcessInspectionOperation),
}

/// 单次清理操作失败及其完整的输入输出诊断信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupError {
    pub operation: CleanupOperation,
    pub kind: std::io::ErrorKind,
    pub raw_os_error: Option<i32>,
    pub message: String,
}

impl CleanupError {
    fn new(operation: CleanupOperation, error: std::io::Error) -> Self {
        Self::from_io(operation, &error)
    }

    pub(crate) fn from_io(operation: CleanupOperation, error: &std::io::Error) -> Self {
        Self {
            operation,
            kind: error.kind(),
            raw_os_error: error.raw_os_error(),
            message: error.to_string(),
        }
    }
}

/// 进程组的强制清理状态、操作失败记录以及最终残留的存活成员。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessGroupCleanup {
    pub forced: bool,
    pub errors: Vec<CleanupError>,
    pub surviving_pids: Vec<u32>,
}

impl ProcessGroupCleanup {
    pub fn succeeded(&self) -> bool {
        self.errors.is_empty() && self.surviving_pids.is_empty()
    }
}

/// 一条命令的完整结果：输出字节及其完整性、直接子进程的终止方式、测试套件的干预与进程组清理。
///
/// [`CommandOutcome::clean_exit_code`] 只在干净退出时给出退出码：两条输出流都读到 EOF，没有
/// 干预与强制清理，清理成功且没有存活进程。信号终止、缺失的终止状态或单纯的退出码 `0` 都不算干净退出。
/// [`CommandOutcome::summary`] 与 `Debug` 只输出元数据；产品是否发生 panic 不从信号推断。
#[derive(Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_complete: bool,
    pub stderr_complete: bool,
    /// 仅当清理阶段无法回收直接子进程时为 `None`。
    pub termination: Option<ProcessTermination>,
    pub interventions: Vec<HarnessIntervention>,
    pub cleanup: ProcessGroupCleanup,
}

/// 仅包含元数据的命令诊断视图，绝不输出 `stdout` 或 `stderr` 原始字节。
#[derive(Clone, Copy)]
pub struct CommandOutcomeSummary<'a> {
    outcome: &'a CommandOutcome,
}

impl fmt::Display for CommandOutcomeSummary<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CommandOutcome")
            .field("termination", &self.outcome.termination)
            .field("interventions", &self.outcome.interventions)
            .field("cleanup", &self.outcome.cleanup)
            .field("stdout_bytes", &self.outcome.stdout.len())
            .field("stdout_complete", &self.outcome.stdout_complete)
            .field("stderr_bytes", &self.outcome.stderr.len())
            .field("stderr_complete", &self.outcome.stderr_complete)
            .finish()
    }
}

impl fmt::Debug for CommandOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.summary(), formatter)
    }
}

impl CommandOutcome {
    pub fn summary(&self) -> CommandOutcomeSummary<'_> {
        CommandOutcomeSummary { outcome: self }
    }

    pub fn exit_code(&self) -> Option<i32> {
        match self.termination {
            Some(ProcessTermination::Exited(code)) => Some(code),
            _ => None,
        }
    }

    pub fn signal(&self) -> Option<i32> {
        match self.termination {
            Some(ProcessTermination::Signaled(signal)) => Some(signal),
            _ => None,
        }
    }

    pub fn clean_exit_code(&self) -> Option<i32> {
        if self.stdout_complete
            && self.stderr_complete
            && self.interventions.is_empty()
            && !self.cleanup.forced
            && self.cleanup.succeeded()
        {
            self.exit_code()
        } else {
            None
        }
    }

    pub fn is_clean_exit(&self, expected: i32) -> bool {
        self.clean_exit_code() == Some(expected)
    }
}

#[derive(Debug, Clone)]
pub struct LatticeCommand {
    pub args: Vec<String>,
    /// 单次调用的墙钟超时预算；为 `None` 时不设单命令上限。
    pub timeout: Option<Duration>,
}

/// 后台并发排空输出管道的运行中子进程句柄。
///
/// 在 Linux 上，直接子进程始终作为自身独立进程组的组首领运行，因此超时、中断与 `Drop` 清理
/// 都能同时覆盖直接子进程及其派生后代。
///
/// `pgid` 通过 `getpgid(child_pid)` 从内核回读，而不直接假定等于子进程号：若向错误的进程组广播信号，
/// 可能命中测试进程自身并杀掉整轮 `cargo test`（#105）。每次发送进程组信号前还会再次核验目标组不是本进程自身所在的进程组。
struct DrainHandle {
    bytes: Arc<std::sync::Mutex<Vec<u8>>>,
    stop: Arc<AtomicBool>,
    thread: JoinHandle<std::io::Result<bool>>,
}

pub struct RunningCommand {
    child: Option<Child>,
    pgid: u32,
    stdout: Option<DrainHandle>,
    stderr: Option<DrainHandle>,
    interventions: Vec<HarnessIntervention>,
    started_at: Instant,
    timeout: Option<Duration>,
}

impl RunningCommand {
    pub fn spawn(command: Command) -> std::io::Result<Self> {
        Self::spawn_with_timeout(command, None)
    }

    /// 正常收尾继承绝对期限；`spawn` 与后续 `wait` 都不能重新获得一份旧时长。
    pub(crate) fn spawn_until(command: Command, deadline: Instant) -> std::io::Result<Self> {
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "命令截止时间在启动前已耗尽",
            ));
        }
        let mut running = Self::spawn(command)?;
        running.timeout = Some(deadline.saturating_duration_since(running.started_at));
        Ok(running)
    }

    fn spawn_with_timeout(command: Command, timeout: Option<Duration>) -> std::io::Result<Self> {
        Self::spawn_with_input(command, timeout, Stdio::null())
    }

    fn spawn_with_input(
        mut command: Command,
        timeout: Option<Duration>,
        stdin: Stdio,
    ) -> std::io::Result<Self> {
        let started_at = Instant::now();
        command.stdin(stdin);
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        set_new_process_group(&mut command);
        let mut child = command.spawn()?;
        let pgid = resolve_child_pgid(&child)?;
        let stdout_stream = child.stdout.take();
        let stderr_stream = child.stderr.take();
        if let Err(error) = stdout_stream
            .as_ref()
            .map(set_nonblocking)
            .transpose()
            .and_then(|_| stderr_stream.as_ref().map(set_nonblocking).transpose())
        {
            kill_process_group(pgid);
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let stdout = Some(drain_to_thread(stdout_stream));
        let stderr = Some(drain_to_thread(stderr_stream));
        Ok(Self {
            child: Some(child),
            pgid,
            stdout,
            stderr,
            interventions: Vec::new(),
            started_at,
            timeout,
        })
    }

    pub fn pgid(&self) -> u32 {
        self.pgid
    }

    /// 读取当前已收集到的标准输出副本，不中断后台读取线程。
    pub fn stdout_snapshot(&self) -> Vec<u8> {
        self.stdout
            .as_ref()
            .map(|handle| handle.bytes.lock().expect("标准输出锁").clone())
            .unwrap_or_default()
    }

    /// 读取当前已收集到的标准错误副本，不中断后台读取线程。
    pub fn stderr_snapshot(&self) -> Vec<u8> {
        self.stderr
            .as_ref()
            .map(|handle| handle.bytes.lock().expect("标准错误锁").clone())
            .unwrap_or_default()
    }

    /// 当前命令剩余可用时间，按单次中间握手的上限截断。
    pub fn remaining_timeout(&self, cap: Duration) -> Duration {
        self.timeout
            .map(|limit| limit.saturating_sub(self.started_at.elapsed()).min(cap))
            .unwrap_or(cap)
    }

    /// 非阻塞观察直接子进程是否已退出，不调用 `wait` 提前回收子进程。
    /// 未被回收的组首领会继续占住数值 `pgid`，直到 `wait`、`kill_and_wait` 或 `Drop` 正式回收。
    pub fn has_exited(&mut self) -> std::io::Result<bool> {
        let pid = self.child.as_ref().expect("运行中的子进程句柄缺失").id();
        loop {
            // SAFETY: `info` 清零初始化；`WEXITED | WNOHANG | WNOWAIT` 只观察退出状态而不消费它。
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let rc = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if rc == 0 {
                // SAFETY: `waitid` 带 `WNOHANG` 返回 0 时，目标尚未退出则 `si_pid` 保持为 0，已进入可回收态则写入子进程号。
                return Ok(unsafe { info.si_pid() } != 0);
            }
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
    }

    /// 向整个进程组发送受控的 `SIGINT` 中断。
    pub fn interrupt(&mut self) -> std::io::Result<()> {
        interrupt_process_group(self.pgid)?;
        self.interventions
            .push(HarnessIntervention::InterruptRequested);
        Ok(())
    }

    /// 向整个进程组发送受控的 `SIGTERM` 终止信号。
    pub fn terminate(&mut self) -> std::io::Result<()> {
        terminate_process_group(self.pgid)?;
        self.interventions
            .push(HarnessIntervention::TerminateRequested);
        Ok(())
    }

    /// 向整个进程组发送 `SIGKILL` 并保留进程终止前已收集到的输出字节。
    pub fn kill_and_wait(self) -> Result<CommandOutcome, DriverError> {
        self.force_kill_and_finish()
    }

    /// 等待命令完成并保留清理前已收集到的全部输出字节；超时与中断作为结果字段返回，不视为输入输出错误。
    pub fn wait(mut self, timeout: Option<Duration>) -> Result<CommandOutcome, DriverError> {
        let limit = self.timeout.or(timeout);
        loop {
            if self.has_exited()? {
                return self.reap_and_finish();
            }
            if let Some(limit) = limit
                && self.started_at.elapsed() >= limit
            {
                self.interventions
                    .push(HarnessIntervention::TimedOut(limit));
                return self.force_kill_and_finish();
            }
            std::thread::sleep(TIMEOUT_POLL);
        }
    }

    fn reap_and_finish(mut self) -> Result<CommandOutcome, DriverError> {
        let mut cleanup = ProcessGroupCleanup::default();
        // 先趁组首领尚未被 `wait` 回收、数值 `pgid` 仍被内核锁定时清理同组残留后代，再正式回收组首领。
        if process_group_requires_cleanup(&mut cleanup, processes_in_pgid(self.pgid)) {
            self.interventions.push(HarnessIntervention::ForcedKill);
            cleanup.forced = true;
            record_process_group_signal_result(
                &mut cleanup,
                CleanupOperation::SignalProcessGroup(libc::SIGKILL),
                signal_process_group(self.pgid, libc::SIGKILL),
                || processes_in_pgid(self.pgid),
            );
        }
        let mut child = self.child.take().expect("运行中的子进程句柄缺失");
        let status = record_cleanup_result(&mut cleanup, CleanupOperation::WaitChild, child.wait());
        self.finish(status, cleanup)
    }

    fn force_kill_and_finish(mut self) -> Result<CommandOutcome, DriverError> {
        self.interventions.push(HarnessIntervention::ForcedKill);
        let mut cleanup = ProcessGroupCleanup {
            forced: true,
            ..ProcessGroupCleanup::default()
        };
        record_process_group_signal_result(
            &mut cleanup,
            CleanupOperation::SignalProcessGroup(libc::SIGKILL),
            signal_process_group(self.pgid, libc::SIGKILL),
            || processes_in_pgid(self.pgid),
        );
        let mut child = self.child.take().expect("运行中的子进程句柄缺失");
        record_cleanup_result(&mut cleanup, CleanupOperation::KillChild, child.kill());
        let status = record_cleanup_result(&mut cleanup, CleanupOperation::WaitChild, child.wait());
        self.finish(status, cleanup)
    }

    fn finish(
        mut self,
        status: Option<ExitStatus>,
        mut cleanup: ProcessGroupCleanup,
    ) -> Result<CommandOutcome, DriverError> {
        if cleanup.forced
            && let Some(survivors) =
                record_inspection_result(&mut cleanup, wait_for_process_group_exit(self.pgid))
        {
            cleanup.surviving_pids = survivors;
        }
        let (stdout, stdout_complete) = finish_drain(self.stdout.take())?;
        let (stderr, stderr_complete) = finish_drain(self.stderr.take())?;
        Ok(CommandOutcome {
            stdout,
            stderr,
            stdout_complete,
            stderr_complete,
            termination: status.as_ref().and_then(process_termination),
            interventions: std::mem::take(&mut self.interventions),
            cleanup,
        })
    }
}

impl Drop for RunningCommand {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            kill_process_group(self.pgid);
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = finish_drain(self.stdout.take());
        let _ = finish_drain(self.stderr.take());
    }
}

/// 供 `ScenarioDaemon` 独占持有的未回收直接子进程句柄。
///
/// 只要该句柄尚未被回收入口按值消费，底层 `Child` 就从未调用过 `wait` 或 `try_wait`，
/// 内核保证其数值 `pgid` 不会被其他新进程组复用；退出探测由调用方经 `pidfd` 完成。
/// 一旦消费回收，立即产出 [`CommandOutcome`]，不再残留任何会在 `Drop` 时向数值进程组发信号的句柄。
pub(crate) struct DaemonChild {
    child: Option<Child>,
    pid: u32,
    pgid: u32,
    stdout: Option<DrainHandle>,
    stderr: Option<DrainHandle>,
    interventions: Vec<HarnessIntervention>,
}

impl DaemonChild {
    pub(crate) fn spawn(mut command: Command) -> std::io::Result<Self> {
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        set_new_process_group(&mut command);
        let mut child = command.spawn()?;
        let pid = child.id();
        let pgid = match resolve_child_pgid(&child) {
            Ok(pgid) => pgid,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let stdout_stream = child.stdout.take();
        let stderr_stream = child.stderr.take();
        if let Err(error) = stdout_stream
            .as_ref()
            .map(set_nonblocking)
            .transpose()
            .and_then(|_| stderr_stream.as_ref().map(set_nonblocking).transpose())
        {
            kill_process_group(pgid);
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let stdout = Some(drain_to_thread(stdout_stream));
        let stderr = Some(drain_to_thread(stderr_stream));
        Ok(Self {
            child: Some(child),
            pid,
            pgid,
            stdout,
            stderr,
            interventions: Vec::new(),
        })
    }

    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    pub(crate) fn pgid(&self) -> u32 {
        self.pgid
    }

    pub(crate) fn stderr_snapshot(&self) -> Vec<u8> {
        self.stderr
            .as_ref()
            .map(|handle| handle.bytes.lock().expect("标准错误锁").clone())
            .unwrap_or_default()
    }

    pub(crate) fn record_intervention(&mut self, intervention: HarnessIntervention) {
        self.interventions.push(intervention);
    }

    pub(crate) fn reap_exited(
        mut self,
        mut cleanup: ProcessGroupCleanup,
    ) -> Result<CommandOutcome, DriverError> {
        let mut child = self.child.take().expect("未回收的后台直接子进程");
        let status = record_cleanup_result(&mut cleanup, CleanupOperation::WaitChild, child.wait());
        self.finish_outcome(status, cleanup)
    }

    pub(crate) fn force_reap(
        mut self,
        mut cleanup: ProcessGroupCleanup,
    ) -> Result<CommandOutcome, DriverError> {
        let mut child = self.child.take().expect("未回收的后台直接子进程");
        record_cleanup_result(&mut cleanup, CleanupOperation::KillChild, child.kill());
        let status = record_cleanup_result(&mut cleanup, CleanupOperation::WaitChild, child.wait());
        self.finish_outcome(status, cleanup)
    }

    fn finish_outcome(
        &mut self,
        status: Option<ExitStatus>,
        cleanup: ProcessGroupCleanup,
    ) -> Result<CommandOutcome, DriverError> {
        let (stdout, stdout_complete) = finish_drain(self.stdout.take())?;
        let (stderr, stderr_complete) = finish_drain(self.stderr.take())?;
        Ok(CommandOutcome {
            stdout,
            stderr,
            stdout_complete,
            stderr_complete,
            termination: status.as_ref().and_then(process_termination),
            interventions: std::mem::take(&mut self.interventions),
            cleanup,
        })
    }
}

impl Drop for DaemonChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = finish_drain(self.stdout.take());
        let _ = finish_drain(self.stderr.take());
    }
}

fn resolve_binary_value(
    raw: Option<&std::ffi::OsStr>,
    env: &'static str,
    binary_name: &'static str,
) -> Result<PathBuf, DriverError> {
    use std::os::unix::fs::PermissionsExt as _;
    let Some(raw) = raw else {
        return Err(DriverError::MissingBinaryEnv {
            env,
            binary: binary_name,
        });
    };
    if raw.is_empty() {
        return Err(DriverError::BinaryNotFound {
            env,
            path: String::new(),
        });
    }
    let path = Path::new(raw);
    let display = path.display().to_string();
    let link_meta = std::fs::symlink_metadata(path).map_err(|_| DriverError::BinaryNotFound {
        env,
        path: display.clone(),
    })?;
    if link_meta.file_type().is_dir() {
        return Err(DriverError::BinaryNotFound { env, path: display });
    }
    let binary =
        std::fs::canonicalize(path).map_err(|error| DriverError::BinaryCanonicalizeFailed {
            env,
            path: display.clone(),
            reason: error.to_string(),
        })?;
    let meta = std::fs::metadata(&binary).map_err(|_| DriverError::BinaryNotFound {
        env,
        path: binary.display().to_string(),
    })?;
    if !meta.is_file() {
        return Err(DriverError::BinaryNotFound {
            env,
            path: binary.display().to_string(),
        });
    }
    if meta.permissions().mode() & 0o111 == 0 {
        return Err(DriverError::BinaryNotExecutable {
            env,
            path: binary.display().to_string(),
        });
    }
    Ok(binary)
}

impl LatticeDriver {
    pub fn from_env() -> Result<Self, DriverError> {
        let raw = std::env::var_os(LATTICE_BIN_ENV);
        let binary = resolve_binary_value(raw.as_deref(), LATTICE_BIN_ENV, "lattice")?;
        Ok(Self { binary })
    }

    pub fn binary_path(&self) -> &Path {
        &self.binary
    }

    pub fn start(
        &self,
        world: &ScenarioWorld,
        command: &LatticeCommand,
    ) -> Result<RunningCommand, DriverError> {
        let mut cmd = world.command(&self.binary);
        cmd.args(&command.args);
        Ok(RunningCommand::spawn_with_timeout(cmd, command.timeout)?)
    }

    pub fn run(
        &self,
        world: &ScenarioWorld,
        command: &LatticeCommand,
    ) -> Result<CommandOutcome, DriverError> {
        self.start(world, command)?.wait(None)
    }

    /// 用匿名临时文件提供标准输入，使秘密不进入命令行参数、`Debug` 输出或场景证据。
    pub fn run_with_stdin(
        &self,
        world: &ScenarioWorld,
        command: &LatticeCommand,
        input: &[u8],
    ) -> Result<CommandOutcome, DriverError> {
        let mut stdin = tempfile::tempfile()?;
        stdin.write_all(input)?;
        stdin.rewind()?;
        let mut cmd = world.command(&self.binary);
        cmd.args(&command.args);
        RunningCommand::spawn_with_input(cmd, command.timeout, Stdio::from(stdin))?.wait(None)
    }

    pub fn help(
        &self,
        world: &ScenarioWorld,
        spec: &ScenarioSpec,
    ) -> Result<CommandOutcome, DriverError> {
        self.run(
            world,
            &LatticeCommand {
                args: vec!["--help".into()],
                timeout: Some(spec.step_timeout()),
            },
        )
    }

    pub fn bare_invoke(
        &self,
        world: &ScenarioWorld,
        spec: &ScenarioSpec,
    ) -> Result<CommandOutcome, DriverError> {
        self.run(
            world,
            &LatticeCommand {
                args: Vec::new(),
                timeout: Some(spec.step_timeout()),
            },
        )
    }
}

impl LatticeDaemonDriver {
    pub fn from_env() -> Result<Self, DriverError> {
        let raw = std::env::var_os(LATTICE_DAEMON_BIN_ENV);
        let binary =
            resolve_binary_value(raw.as_deref(), LATTICE_DAEMON_BIN_ENV, "lattice-daemon")?;
        Ok(Self { binary })
    }

    pub fn binary_path(&self) -> &Path {
        &self.binary
    }

    pub fn start(
        &self,
        world: &ScenarioWorld,
        command: &LatticeCommand,
    ) -> Result<RunningCommand, DriverError> {
        let mut cmd = world.command(&self.binary);
        cmd.args(&command.args);
        Ok(RunningCommand::spawn_with_timeout(cmd, command.timeout)?)
    }

    pub fn run(
        &self,
        world: &ScenarioWorld,
        command: &LatticeCommand,
    ) -> Result<CommandOutcome, DriverError> {
        self.start(world, command)?.wait(None)
    }
}

fn set_nonblocking(stream: &impl AsRawFd) -> std::io::Result<()> {
    let fd = stream.as_raw_fd();
    // SAFETY: `fd` 是存活的子进程管道描述符；`fcntl` 仅读写该描述符的状态标志并通过 `errno` 报告失败。
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn drain_to_thread<R: Read + Send + 'static>(maybe: Option<R>) -> DrainHandle {
    let bytes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let thread_bytes = Arc::clone(&bytes);
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread = std::thread::spawn(move || {
        let Some(mut stream) = maybe else {
            return Ok(true);
        };
        let mut chunk = [0_u8; 8192];
        let reached_eof = loop {
            if thread_stop.load(Ordering::Acquire) {
                break false;
            }
            match stream.read(&mut chunk) {
                Ok(0) => break true,
                Ok(read) => {
                    thread_bytes
                        .lock()
                        .expect("输出收集锁")
                        .extend_from_slice(&chunk[..read]);
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(TIMEOUT_POLL);
                }
                Err(error) => return Err(error),
            }
        };
        Ok(reached_eof)
    });
    DrainHandle {
        bytes,
        stop,
        thread,
    }
}

fn finish_drain(handle: Option<DrainHandle>) -> Result<(Vec<u8>, bool), DriverError> {
    let Some(handle) = handle else {
        return Ok((Vec::new(), true));
    };
    let deadline = Instant::now() + DRAIN_FINISH_GRACE;
    while !handle.thread.is_finished() && Instant::now() < deadline {
        std::thread::sleep(TIMEOUT_POLL);
    }
    handle.stop.store(true, Ordering::Release);
    let reached_eof = handle
        .thread
        .join()
        .map_err(|_| DriverError::DrainThreadPanicked)?
        .map_err(DriverError::Io)?;
    let bytes = std::mem::take(&mut *handle.bytes.lock().expect("输出收集锁"));
    Ok((bytes, reached_eof))
}

fn record_cleanup_result<T>(
    cleanup: &mut ProcessGroupCleanup,
    operation: CleanupOperation,
    result: std::io::Result<T>,
) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            cleanup.errors.push(CleanupError::new(operation, error));
            None
        }
    }
}

fn record_process_group_signal_result<F>(
    cleanup: &mut ProcessGroupCleanup,
    operation: CleanupOperation,
    result: std::io::Result<()>,
    recheck: F,
) where
    F: FnOnce() -> Result<Vec<u32>, CleanupError>,
{
    match result {
        Ok(()) => {}
        // 清理信号遇到 `ESRCH` 时立即复核进程组：组已为空才算无害；主动请求发出的 `SIGINT` 走另一条路径。
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
            let signal_error = CleanupError::new(operation, error);
            match recheck() {
                Ok(pids) if pids.is_empty() => {}
                Ok(_) => cleanup.errors.push(signal_error),
                Err(inspection_error) => {
                    cleanup.errors.push(signal_error);
                    cleanup.errors.push(inspection_error);
                }
            }
        }
        Err(error) => cleanup.errors.push(CleanupError::new(operation, error)),
    }
}

fn process_group_requires_cleanup(
    cleanup: &mut ProcessGroupCleanup,
    result: Result<Vec<u32>, CleanupError>,
) -> bool {
    record_inspection_result(cleanup, result).is_none_or(|pids| !pids.is_empty())
}

fn record_inspection_result(
    cleanup: &mut ProcessGroupCleanup,
    result: Result<Vec<u32>, CleanupError>,
) -> Option<Vec<u32>> {
    match result {
        Ok(pids) => Some(pids),
        Err(error) => {
            cleanup.errors.push(error);
            None
        }
    }
}

fn process_termination(status: &ExitStatus) -> Option<ProcessTermination> {
    status
        .code()
        .map(ProcessTermination::Exited)
        .or_else(|| status.signal().map(ProcessTermination::Signaled))
}

fn wait_for_process_group_exit(pgid: u32) -> Result<Vec<u32>, CleanupError> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let survivors = processes_in_pgid(pgid)?;
        if survivors.is_empty() || Instant::now() >= deadline {
            return Ok(survivors);
        }
        std::thread::sleep(TIMEOUT_POLL);
    }
}

pub(crate) fn set_new_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

/// 从内核回读子进程实际所属的进程组号。
///
/// `process_group(0)` 会让子进程成为自身进程组的组首领，因此正常情况下该值等于子进程号；
/// 显式回读可防止 `setpgid` 失败或不受支持时静默将 `pgid` 留在测试套件自身的进程组上。
fn resolve_child_pgid(child: &Child) -> std::io::Result<u32> {
    let pid = child.id();
    // SAFETY: `getpgid` 只读取 `pid` 的内核调度元数据，无内存安全前置条件，失败经 `errno` 报告。
    let pgid = unsafe { libc::getpgid(pid as libc::pid_t) };
    if pgid < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let pgid = pgid as u32;
    reject_own_process_group(pgid)?;
    Ok(pgid)
}

/// 拒绝把当前测试进程自身的进程组当作信号发送目标。
///
/// 向测试套件自身所在的进程组广播信号会中断测试二进制本身，导致 `cargo test` 以退出码 `130` 终止却报告零失败（#105）；
/// 封闭失败能把这种静默中止整轮测试的故障转化为局部错误。
pub(crate) fn reject_own_process_group(pgid: u32) -> std::io::Result<()> {
    // SAFETY: `getpgrp` 无参数，仅读取调用者自身的进程组号，不会失败。
    let own = unsafe { libc::getpgrp() } as u32;
    if pgid == own {
        return Err(std::io::Error::other(format!(
            "拒绝向进程组 {pgid} 发送信号：它是当前测试进程自身的进程组"
        )));
    }
    Ok(())
}

pub(crate) fn signal_process_group(pgid: u32, signal: libc::c_int) -> std::io::Result<()> {
    reject_own_process_group(pgid)?;
    // SAFETY: `killpg` 仅向目标进程组投递 `signal`，无内存安全前置条件，失败经 `errno` 报告。
    if unsafe { libc::killpg(pgid as libc::pid_t, signal) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn interrupt_process_group(pgid: u32) -> std::io::Result<()> {
    signal_process_group(pgid, libc::SIGINT)
}

fn terminate_process_group(pgid: u32) -> std::io::Result<()> {
    signal_process_group(pgid, libc::SIGTERM)
}

pub(crate) fn kill_process_group(pgid: u32) {
    let _ = signal_process_group(pgid, libc::SIGKILL);
}

pub fn processes_in_pgid(pgid: u32) -> Result<Vec<u32>, CleanupError> {
    processes_in_pgid_at(Path::new("/proc"), pgid)
}

fn processes_in_pgid_at(proc_root: &Path, pgid: u32) -> Result<Vec<u32>, CleanupError> {
    let entries = std::fs::read_dir(proc_root).map_err(|error| {
        CleanupError::new(
            CleanupOperation::InspectProcessGroup(ProcessInspectionOperation::ReadProc),
            error,
        )
    })?;
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            CleanupError::new(
                CleanupOperation::InspectProcessGroup(ProcessInspectionOperation::ReadProcEntry),
                error,
            )
        })?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let stat_path = entry.path().join("stat");
        let stat = match std::fs::read_to_string(&stat_path) {
            Ok(stat) => stat,
            Err(error) if process_exited(&error) => {
                // 进程在枚举 `/proc` 与读取 `stat` 之间已退出，不可能是最终残留的存活成员。
                continue;
            }
            Err(error) => {
                return Err(CleanupError::new(
                    CleanupOperation::InspectProcessGroup(ProcessInspectionOperation::ReadStat(
                        pid,
                    )),
                    error,
                ));
            }
        };
        if parse_process_stat(pid, &stat)?.is_some_and(|member| member.pgid == pgid) {
            out.push(pid);
        }
    }
    out.sort_unstable();
    Ok(out)
}

/// 清理所需的进程组与会话字段；已退出或僵尸进程返回 `None`。
pub(crate) fn process_stat(pid: u32) -> Result<Option<ProcessStat>, CleanupError> {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) if process_exited(&error) => return Ok(None),
        Err(error) => {
            return Err(CleanupError::new(
                CleanupOperation::InspectProcessGroup(ProcessInspectionOperation::ReadStat(pid)),
                error,
            ));
        }
    };
    parse_process_stat(pid, &stat)
}

/// 进程在列出 `/proc` 与读取 `stat` 之间退出：目录已消失时为 `ENOENT`，
/// 目录仍在但进程已回收时读取返回 `ESRCH`。两者都说明它不再是存活成员。
pub(crate) fn process_exited(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ESRCH)
}

/// `/proc/<pid>/stat` 中清理需要的字段：进程组与会话。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcessStat {
    pub(crate) pgid: u32,
    pub(crate) session: u32,
}

fn parse_process_stat(pid: u32, stat: &str) -> Result<Option<ProcessStat>, CleanupError> {
    let invalid = |message: &str| {
        CleanupError::new(
            CleanupOperation::InspectProcessGroup(ProcessInspectionOperation::ParseStat(pid)),
            std::io::Error::new(std::io::ErrorKind::InvalidData, message),
        )
    };
    let (_, rest) = stat
        .rsplit_once(')')
        .ok_or_else(|| invalid("进程 stat 缺少命令名右括号"))?;
    let mut fields = rest.split_whitespace();
    let state = fields
        .next()
        .ok_or_else(|| invalid("进程 stat 缺少状态字段"))?;
    // `Z`（僵尸）与 `X`（正在释放）均已退出，不属于存活成员。
    if matches!(state, "Z" | "X") {
        return Ok(None);
    }
    let _ppid = fields
        .next()
        .ok_or_else(|| invalid("进程 stat 缺少父进程号字段"))?;
    let pgid = fields
        .next()
        .ok_or_else(|| invalid("进程 stat 缺少进程组字段"))?;
    // 退出后期 `sighand` 已释放时，内核无论状态字符为何都会把 `pgrp`/`session` 报告为 `-1`
    // （`fs/proc/array.c` 的初始值）。这类进程已经退出，不属于存活成员。
    if pgid == "-1" {
        return Ok(None);
    }
    let pgid = pgid
        .parse::<u32>()
        .map_err(|_| invalid("进程 stat 的进程组字段无效"))?;
    let session = fields
        .next()
        .ok_or_else(|| invalid("进程 stat 缺少会话号字段"))?
        .parse::<u32>()
        .map_err(|_| invalid("进程 stat 的会话号字段无效"))?;
    Ok(Some(ProcessStat { pgid, session }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_resolution_rejects_missing_nonexistent_uncanonicalizable_and_non_executable_paths() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        for (env_name, label) in [
            (LATTICE_BIN_ENV, "lattice"),
            (LATTICE_DAEMON_BIN_ENV, "lattice-daemon"),
        ] {
            let missing = resolve_binary_value(None, env_name, label).unwrap_err();
            assert!(
                matches!(missing, DriverError::MissingBinaryEnv { env, .. } if env == env_name),
                "未设置 {env_name} 应报错，实际：{missing}"
            );

            let empty =
                resolve_binary_value(Some(std::ffi::OsStr::new("")), env_name, label).unwrap_err();
            assert!(
                matches!(empty, DriverError::BinaryNotFound { env, .. } if env == env_name),
                "空路径应报错，实际：{empty}"
            );

            let dir = tempfile::tempdir().expect("创建临时目录失败");
            let absent = dir.path().join("absent-bin");
            let not_found =
                resolve_binary_value(Some(absent.as_os_str()), env_name, label).unwrap_err();
            assert!(
                matches!(not_found, DriverError::BinaryNotFound { env, .. } if env == env_name),
                "不存在的文件应报错，实际：{not_found}"
            );

            let dir_err =
                resolve_binary_value(Some(dir.path().as_os_str()), env_name, label).unwrap_err();
            assert!(
                matches!(dir_err, DriverError::BinaryNotFound { env, .. } if env == env_name),
                "目录路径不应被当作二进制，实际：{dir_err}"
            );

            let dangling = dir.path().join("dangling-link");
            symlink(dir.path().join("missing-target"), &dangling).expect("创建悬空符号链接失败");
            let canon_err =
                resolve_binary_value(Some(dangling.as_os_str()), env_name, label).unwrap_err();
            assert!(
                matches!(
                    canon_err,
                    DriverError::BinaryCanonicalizeFailed { env, .. } if env == env_name
                ),
                "无法规范化的悬空符号链接应报错，实际：{canon_err}"
            );

            let non_exec = dir.path().join("non-exec-bin");
            std::fs::write(&non_exec, b"#!/bin/sh\nexit 0\n").expect("写入不可执行文件失败");
            std::fs::set_permissions(&non_exec, std::fs::Permissions::from_mode(0o644))
                .expect("设置 0644 权限失败");
            let exec_err =
                resolve_binary_value(Some(non_exec.as_os_str()), env_name, label).unwrap_err();
            assert!(
                matches!(exec_err, DriverError::BinaryNotExecutable { env, .. } if env == env_name),
                "不可执行文件应报错，实际：{exec_err}"
            );

            let valid = dir.path().join(label);
            std::fs::write(&valid, b"#!/bin/sh\nexit 0\n").expect("写入可执行文件失败");
            std::fs::set_permissions(&valid, std::fs::Permissions::from_mode(0o755))
                .expect("设置 0755 权限失败");
            let resolved = resolve_binary_value(Some(valid.as_os_str()), env_name, label)
                .expect("合法可执行文件应通过");
            assert_eq!(
                resolved,
                std::fs::canonicalize(&valid).expect("规范化期望路径")
            );
        }
    }

    #[test]
    fn human_output_with_a_sending_record_is_a_clean_exit() {
        let outcome = CommandOutcome {
            stdout: b"Example request:\n{\"type\":\"submit\",\"stage\":\"sending\",\"turn_id\":\"t\",\"session_id\":\"s\",\"new\":true}\n".to_vec(),
            stderr: Vec::new(),
            stdout_complete: true,
            stderr_complete: true,
            termination: Some(ProcessTermination::Exited(0)),
            interventions: Vec::new(),
            cleanup: ProcessGroupCleanup::default(),
        };
        assert!(outcome.is_clean_exit(0), "{}", outcome.summary());
    }

    fn process_group_members(pgid: u32) -> Vec<u32> {
        processes_in_pgid(pgid).expect("检查进程组成员失败")
    }

    #[test]
    fn signal_records_concrete_signal_without_magic_exit_code() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "kill -TERM $$"]);
        let outcome = RunningCommand::spawn(cmd)
            .expect("启动子进程失败")
            .wait(Some(Duration::from_secs(2)))
            .expect("等待信号终止结果失败");

        assert_eq!(
            outcome.termination,
            Some(ProcessTermination::Signaled(libc::SIGTERM))
        );
        assert_eq!(outcome.exit_code(), None);
        assert_eq!(outcome.signal(), Some(libc::SIGTERM));
        assert!(outcome.interventions.is_empty());
        assert_eq!(outcome.cleanup, ProcessGroupCleanup::default());
    }

    #[test]
    fn timeout_includes_time_before_wait() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let ready = dir.path().join("ready");
        let trigger = dir.path().join("trigger");
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(format!(
            "touch '{}'; while [ ! -e '{}' ]; do sleep 0.01; done; printf after-deadline; sleep 30",
            ready.display(),
            trigger.display()
        ));
        let running = RunningCommand::spawn_with_timeout(cmd, Some(Duration::from_millis(500)))
            .expect("启动带超时子进程失败");
        let ready_deadline = Instant::now() + Duration::from_secs(2);
        while !ready.exists() && Instant::now() < ready_deadline {
            std::thread::yield_now();
        }
        assert!(ready.exists(), "子进程未进入就绪状态");
        std::thread::sleep(Duration::from_millis(550));

        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            std::fs::write(trigger, b"release").expect("写入释放触发文件失败");
        });
        let outcome = running.wait(None).expect("等待超时结果失败");
        release.join().expect("回收释放线程失败");

        assert_eq!(
            outcome.interventions,
            vec![
                HarnessIntervention::TimedOut(Duration::from_millis(500)),
                HarnessIntervention::ForcedKill,
            ]
        );
        assert_eq!(
            outcome.stdout, b"",
            "超时从 wait() 重新起算，放过了启动截止时间之后的输出"
        );
    }

    #[test]
    fn timeout_retains_partial_output_and_cleans_process_group() {
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            "printf partial-out; printf partial-err >&2; sleep 30 & wait",
        ]);
        let running = RunningCommand::spawn(cmd).expect("启动子进程失败");
        let pgid = running.pgid();
        let outcome = running
            .wait(Some(Duration::from_millis(100)))
            .expect("等待超时结果失败");
        assert_eq!(
            outcome.termination,
            Some(ProcessTermination::Signaled(libc::SIGKILL))
        );
        assert_eq!(
            outcome.interventions,
            vec![
                HarnessIntervention::TimedOut(Duration::from_millis(100)),
                HarnessIntervention::ForcedKill,
            ]
        );
        assert!(outcome.cleanup.forced);
        assert!(outcome.cleanup.succeeded(), "{:?}", outcome.cleanup);
        assert!(outcome.cleanup.surviving_pids.is_empty());
        assert_eq!(outcome.stdout, b"partial-out");
        assert_eq!(outcome.stderr, b"partial-err");
        let deadline = Instant::now() + Duration::from_secs(2);
        while !process_group_members(pgid).is_empty() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(process_group_members(pgid).is_empty());
    }

    #[test]
    fn descendant_inheriting_output_pipe_is_reconciled_before_drain() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf direct-out; sleep 2 & exit 0"]);
        let started = Instant::now();

        let outcome = RunningCommand::spawn(cmd)
            .expect("启动子进程失败")
            .wait(Some(Duration::from_millis(100)))
            .expect("等待后代清理结果失败");

        assert!(
            started.elapsed() < Duration::from_secs(1),
            "管道排空耗时超出外部上限: {:?}",
            started.elapsed()
        );
        assert_eq!(outcome.termination, Some(ProcessTermination::Exited(0)));
        assert_eq!(outcome.interventions, vec![HarnessIntervention::ForcedKill]);
        assert!(outcome.cleanup.forced);
        assert!(outcome.cleanup.succeeded(), "{:?}", outcome.cleanup);
        assert!(outcome.cleanup.surviving_pids.is_empty());
        assert_eq!(outcome.stdout, b"direct-out");
    }

    #[test]
    fn incomplete_output_drain_rejects_clean_exit() {
        let (reader, mut writer) =
            std::os::unix::net::UnixStream::pair().expect("创建本地流管道失败");
        reader.set_nonblocking(true).expect("设置读取端非阻塞失败");
        let drain = drain_to_thread(Some(reader));
        std::io::Write::write_all(&mut writer, b"prefix").expect("写入前缀失败");

        let (stdout, stdout_complete) = finish_drain(Some(drain)).expect("结束输出排空失败");
        drop(writer);
        let outcome = CommandOutcome {
            stdout,
            stderr: Vec::new(),
            stdout_complete,
            stderr_complete: true,
            termination: Some(ProcessTermination::Exited(0)),
            interventions: Vec::new(),
            cleanup: ProcessGroupCleanup::default(),
        };

        assert_eq!(outcome.stdout, b"prefix");
        assert!(!outcome.stdout_complete);
        assert!(
            !outcome.is_clean_exit(0),
            "未读完的输出排空必须使结果不再视为干净退出"
        );
    }

    #[test]
    fn interrupt_timeout_preserves_both_interventions_and_forced_kill() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let ready = dir.path().join("ready");
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(format!(
            "trap '' INT; touch '{}'; sleep 30",
            ready.display()
        ));
        let mut running = RunningCommand::spawn(cmd).expect("启动子进程失败");
        let deadline = Instant::now() + Duration::from_secs(2);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(ready.exists(), "子进程未进入中断等待点");

        running.interrupt().expect("中断进程组失败");
        let outcome = running
            .wait(Some(Duration::from_millis(100)))
            .expect("等待中断超时结果失败");

        assert_eq!(
            outcome.termination,
            Some(ProcessTermination::Signaled(libc::SIGKILL))
        );
        assert_eq!(
            outcome.interventions,
            vec![
                HarnessIntervention::InterruptRequested,
                HarnessIntervention::TimedOut(Duration::from_millis(100)),
                HarnessIntervention::ForcedKill,
            ]
        );
        assert!(outcome.cleanup.forced);
        assert!(outcome.cleanup.succeeded(), "{:?}", outcome.cleanup);
    }

    #[test]
    fn terminate_delivers_sigterm_to_process_group_and_records_intervention() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let ready = dir.path().join("ready");
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(format!(
            "trap 'exit 42' TERM; touch '{}'; while :; do sleep 1; done",
            ready.display()
        ));
        let mut running = RunningCommand::spawn(cmd).expect("启动子进程失败");
        let deadline = Instant::now() + Duration::from_secs(2);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(ready.exists(), "子进程未进入 SIGTERM 等待点");

        running.terminate().expect("终止进程组失败");
        let outcome = running
            .wait(Some(Duration::from_secs(2)))
            .expect("终止后等待子进程失败");

        assert_eq!(
            outcome.exit_code(),
            Some(42),
            "子进程应收到 SIGTERM 并以 42 退出"
        );
        assert_eq!(
            outcome.interventions,
            vec![HarnessIntervention::TerminateRequested],
            "干预记录应包含 TerminateRequested"
        );
        assert!(!outcome.cleanup.forced);
        assert!(outcome.cleanup.succeeded(), "{:?}", outcome.cleanup);
    }

    #[test]
    fn cleanup_sigkill_esrch_is_benign_only_when_group_is_empty() {
        let operation = CleanupOperation::SignalProcessGroup(libc::SIGKILL);
        let mut empty = ProcessGroupCleanup::default();
        record_process_group_signal_result(
            &mut empty,
            operation,
            Err(std::io::Error::from_raw_os_error(libc::ESRCH)),
            || Ok(Vec::new()),
        );
        assert!(empty.errors.is_empty());

        let mut occupied = ProcessGroupCleanup::default();
        record_process_group_signal_result(
            &mut occupied,
            operation,
            Err(std::io::Error::from_raw_os_error(libc::ESRCH)),
            || Ok(vec![42]),
        );
        assert_eq!(occupied.errors.len(), 1);
        assert_eq!(occupied.errors[0].raw_os_error, Some(libc::ESRCH));
    }

    #[test]
    fn cleanup_failures_are_retained_without_an_invented_process_status() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "exit 0"]);
        let running = RunningCommand::spawn(cmd).expect("启动子进程失败");
        let child_pid = running.child.as_ref().expect("运行中的子进程句柄缺失").id();
        let mut status = 0;
        // SAFETY: `child_pid` 属于本测试刚派生的子进程，`status` 指向供 `waitpid` 写入的有效栈上整数。
        assert_eq!(
            unsafe { libc::waitpid(child_pid as libc::pid_t, &mut status, 0) },
            child_pid as libc::pid_t
        );

        let outcome = running.kill_and_wait().expect("获取清理结果失败");

        assert_eq!(outcome.termination, None);
        assert_eq!(outcome.interventions, vec![HarnessIntervention::ForcedKill]);
        assert!(outcome.cleanup.forced);
        assert_eq!(
            outcome
                .cleanup
                .errors
                .iter()
                .map(|error| error.operation)
                .collect::<Vec<_>>(),
            vec![CleanupOperation::KillChild, CleanupOperation::WaitChild]
        );
        assert!(outcome.cleanup.surviving_pids.is_empty());
    }

    #[test]
    fn process_group_inspection_errors_are_typed_and_fail_cleanup_closed() {
        let missing_root = tempfile::tempdir()
            .expect("创建临时父目录失败")
            .path()
            .join("missing-proc");
        let root_error =
            processes_in_pgid_at(&missing_root, 7).expect_err("缺失的 proc 根目录必须报错");
        assert_eq!(
            root_error.operation,
            CleanupOperation::InspectProcessGroup(ProcessInspectionOperation::ReadProc)
        );

        let proc_root = tempfile::tempdir().expect("创建临时 proc 目录失败");
        let pid_dir = proc_root.path().join("4242");
        std::fs::create_dir(&pid_dir).expect("创建进程号目录失败");
        std::fs::create_dir(pid_dir.join("stat")).expect("构造不可读的目录型 stat 条目失败");

        let error =
            processes_in_pgid_at(proc_root.path(), 7).expect_err("读取目录型 stat 必须报错");
        assert_eq!(
            error.operation,
            CleanupOperation::InspectProcessGroup(ProcessInspectionOperation::ReadStat(4242))
        );
        let mut cleanup = ProcessGroupCleanup::default();
        assert!(process_group_requires_cleanup(
            &mut cleanup,
            Err(error.clone())
        ));
        assert_eq!(cleanup.errors, vec![error]);
        assert!(!cleanup.succeeded());
    }

    #[test]
    fn process_stat_parser_rejects_incomplete_and_invalid_group_fields() {
        // 组号缺失、组号非数字、会话号缺失、会话号非数字都必须报 `InvalidData`，不得部分解析。
        for stat in [
            "4242 malformed",
            "4242 (test) S 1 nope",
            "4242 (test) S 1 4242",
            "4242 (te st) S 1 4242 sid",
        ] {
            let error = parse_process_stat(4242, stat).expect_err("畸形 stat 必须解析失败");
            assert_eq!(
                error.operation,
                CleanupOperation::InspectProcessGroup(ProcessInspectionOperation::ParseStat(4242))
            );
            assert_eq!(error.kind, std::io::ErrorKind::InvalidData);
        }
        // 内核对正在释放的进程报告 `pgrp`/`session` 为 `-1`；它已退出，不是存活成员。
        // 状态字符不一定是 `X`：退出后期 `sighand` 已释放时，状态可能仍是 `R`/`D`/`S`，
        // 但 `pgrp`/`session` 同样是 `-1`（压力下偶发，曾让中断清理测试失败）。
        for dead in [
            "4242 (sleep) X 0 -1 -1 0 -1 4227084",
            "4242 (sleep) R 0 -1 -1 0 -1 4227084",
            "4242 (sleep) D 0 -1 -1 0 -1 4227084",
        ] {
            assert_eq!(
                parse_process_stat(4242, dead).expect("解析已退出进程 stat 失败"),
                None,
                "{dead}"
            );
        }
        // 只有 `-1` 表示已退出：同样的状态字符带正常组号与会话号时仍是存活成员，
        // 不能因为上面的放宽而被漏掉。
        for (live, pgid, session) in [
            ("4242 (sleep) R 1 4300 4100 0 -1 4227084", 4300, 4100),
            ("4242 (sleep) D 1 4300 4100 0 -1 4227084", 4300, 4100),
            ("4242 (sleep) S 1 4300 4100 0 -1 4227084", 4300, 4100),
        ] {
            assert_eq!(
                parse_process_stat(4242, live).expect("解析存活进程 stat 失败"),
                Some(ProcessStat { pgid, session }),
                "{live}"
            );
        }
    }

    /// #105：瞄准测试套件自身进程组的信号会中断测试二进制本身，守卫必须直接拒绝而不实际发送。
    #[test]
    fn signalling_own_process_group_is_refused() {
        // SAFETY: `getpgrp` 仅读取调用者自身的进程组号。
        let own = unsafe { libc::getpgrp() } as u32;
        let err = signal_process_group(own, libc::SIGINT).expect_err("必须拒绝向自身进程组发信号");
        assert!(
            err.to_string().contains("自身的进程组"),
            "错误文本不符合预期: {err}"
        );
        // `SIGKILL` 包装函数会吞掉错误；此处必须保持空操作而不得杀掉测试进程自身。
        kill_process_group(own);
    }

    /// 派生的子进程必须落入独立于测试套件自身的进程组，使 `interrupt`/`kill` 绝不会反向命中 `cargo test`。
    #[test]
    fn spawned_child_pgid_is_read_back_and_distinct() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30"]);
        let running = RunningCommand::spawn(cmd).expect("启动子进程失败");
        // SAFETY: `getpgrp` 仅读取调用者自身的进程组号。
        let own = unsafe { libc::getpgrp() } as u32;
        assert_ne!(running.pgid(), own, "子进程与测试套件共享了同一个进程组");
        let child_pid = running.child.as_ref().expect("运行中的子进程句柄缺失").id();
        assert_eq!(
            running.pgid(),
            child_pid,
            "子进程应当成为自身进程组的组首领"
        );
    }

    /// 供中断测试重新执行本测试二进制时运行的进程组成员环境变量。
    const INT_RECORDER_DIR: &str = "LATTICE_HARNESS_INT_RECORDER_DIR";

    /// 后台 `&` 启动的进程继承被忽略的 `SIGINT`，`sh` 的 `trap` 不能恢复它；
    /// 这里自行安装信号处理器，收到 `SIGINT` 时在当前目录创建 `got-int` 后退出。
    #[test]
    #[ignore = "供中断测试自执行复用的辅助入口"]
    fn int_recorder_helper() {
        let Some(dir) = std::env::var_os(INT_RECORDER_DIR) else {
            return;
        };
        extern "C" fn record_int(_: libc::c_int) {
            // SAFETY: `open` 与 `_exit` 均为异步信号安全函数，路径是静态 C 字符串。
            unsafe {
                libc::open(
                    c"got-int".as_ptr(),
                    libc::O_CREAT | libc::O_WRONLY | libc::O_CLOEXEC,
                    0o600,
                );
                libc::_exit(0);
            }
        }
        std::env::set_current_dir(&dir).expect("切换至信号记录目录失败");
        let handler: extern "C" fn(libc::c_int) = record_int;
        // SAFETY: 安装的处理器只调用异步信号安全函数。
        let previous = unsafe { libc::signal(libc::SIGINT, handler as libc::sighandler_t) };
        assert_ne!(previous, libc::SIG_ERR, "安装 SIGINT 信号处理器失败");
        std::fs::write("recorder-ready", b"").expect("写入记录器就绪标记失败");
        std::thread::sleep(Duration::from_secs(30));
    }

    #[test]
    fn interrupt_reaches_and_reaps_the_process_group() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let ready = dir.path().join("ready");
        let recorder_ready = dir.path().join("recorder-ready");
        let got_int = dir.path().join("got-int");
        // 就绪文件由后台子进程在设置忽略 `SIGINT` 之后创建。若由外层在 `&` 之后创建，
        // 中断可能赶在子进程设置忽略之前到达，子进程随之退出，进程组里没有幸存者，
        // 也就不会出现预期的强制清理（压力下约 2%）。外层的 `trap` 在启动子进程之前
        // 已经设置，所以子进程就绪时外层也已就绪。
        // 外层用 `sleep 1 & wait`：`trap` 只在前台命令结束后运行，`wait` 被信号打断后
        // 立即运行 `trap`，不必等当前的 `sleep` 结束。
        // 第三个成员是自执行的本测试二进制，自行安装 `SIGINT` 处理器并记录收到信号，
        // 用来区分信号是发给了整个进程组还是只发给了组首领。
        let exe = std::env::current_exe().expect("获取当前测试二进制路径失败");
        let mut cmd = Command::new("sh");
        cmd.env(INT_RECORDER_DIR, dir.path());
        cmd.arg("-c").arg(format!(
            "trap 'exit 0' INT; sh -c \"trap '' INT; touch '{}'; sleep 30\" & \
             '{}' --exact driver::tests::int_recorder_helper --ignored --nocapture --test-threads=1 & \
             while :; do sleep 1 & wait $!; done",
            ready.display(),
            exe.display()
        ));
        let mut running = RunningCommand::spawn(cmd).expect("启动子进程失败");
        let pgid = running.pgid();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !(ready.exists() && recorder_ready.exists()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(ready.exists(), "子进程未进入中断等待点");
        assert!(recorder_ready.exists(), "SIGINT 记录器未进入就绪状态");
        running.interrupt().expect("中断进程组失败");
        // 先等记录者落盘再收尾，避免强制清理抢在处理器运行之前。
        let deadline = Instant::now() + Duration::from_secs(5);
        while !got_int.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(got_int.exists(), "SIGINT 未送达非组首领的同组成员");
        // 时限从启动算起，含等待就绪的时间；放宽到 20 秒，负载下也不会误判超时。
        let outcome = running
            .wait(Some(Duration::from_secs(20)))
            .expect("等待中断结果失败");
        assert_eq!(outcome.termination, Some(ProcessTermination::Exited(0)));
        assert_eq!(
            outcome.interventions,
            vec![
                HarnessIntervention::InterruptRequested,
                HarnessIntervention::ForcedKill,
            ]
        );
        assert!(outcome.cleanup.forced);
        assert!(outcome.cleanup.succeeded(), "{:?}", outcome.cleanup);
        assert!(outcome.cleanup.surviving_pids.is_empty());
        // `wait` 返回时进程组必须已经清空，不再额外等待。
        assert_eq!(process_group_members(pgid), Vec::<u32>::new());
    }

    fn assert_zombie_still_unreaped(pid: u32) {
        // SAFETY: `info` 清零初始化；`WNOWAIT` 只观察子进程僵尸态而不消费退出状态。
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        assert_eq!(
            rc,
            0,
            "has_exited() 返回 true 后子进程不应已被提前回收: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: `waitid` 找到目标僵尸子进程时 `si_pid` 字段有效。
        assert_eq!(unsafe { info.si_pid() } as u32, pid);
    }

    fn assert_child_reaped(pid: u32) {
        // SAFETY: `info` 清零初始化；已被回收的子进程应返回 `ECHILD`。
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        assert_eq!(rc, -1, "回收后 waitid 应失败，实际仍观察到子进程 pid={pid}");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[test]
    fn has_exited_observes_without_reaping_until_wait_or_kill_and_wait() {
        let deadline = Instant::now() + Duration::from_secs(5);

        // 路径一：`has_exited()` 返回 `true` 后子进程仍未回收，随后的 `wait` 正常回收。
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf waited-out; exit 7"]);
        let mut running = RunningCommand::spawn(cmd).expect("启动待 wait 子进程失败");
        let pid = running.pgid();
        while !running.has_exited().expect("轮询退出状态") {
            assert!(Instant::now() < deadline, "子进程未在期限内退出");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_zombie_still_unreaped(pid);
        let outcome = running
            .wait(Some(Duration::from_secs(2)))
            .expect("wait 回收已退出子进程");
        assert!(outcome.is_clean_exit(7), "{}", outcome.summary());
        assert_eq!(outcome.stdout, b"waited-out");
        assert_child_reaped(pid);

        // 路径二：`has_exited()` 返回 `true` 后子进程仍占住 `pgid`，随后的 `kill_and_wait` 安全发信号并回收。
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf loser-err >&2; exit 1"]);
        let mut running = RunningCommand::spawn(cmd).expect("启动待 kill_and_wait 子进程失败");
        let pid = running.pgid();
        while !running.has_exited().expect("轮询退出状态") {
            assert!(Instant::now() < deadline, "子进程未在期限内退出");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_zombie_still_unreaped(pid);
        let outcome = running
            .kill_and_wait()
            .expect("kill_and_wait 回收已退出子进程");
        assert_eq!(outcome.exit_code(), Some(1));
        assert_eq!(outcome.stderr, b"loser-err");
        assert!(outcome.cleanup.forced);
        assert!(outcome.cleanup.succeeded(), "{:?}", outcome.cleanup);
        assert_child_reaped(pid);

        // 路径三：`has_exited()` 返回 `true` 后直接 `Drop`，同样在回收前安全向原组发信号并完成回收。
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "exit 0"]);
        let mut running = RunningCommand::spawn(cmd).expect("启动待 Drop 子进程失败");
        let pid = running.pgid();
        while !running.has_exited().expect("轮询退出状态") {
            assert!(Instant::now() < deadline, "子进程未在期限内退出");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_zombie_still_unreaped(pid);
        drop(running);
        assert_child_reaped(pid);
    }
}
