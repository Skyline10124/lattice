// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::fmt;
use std::io::Read;
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
const TIMEOUT_POLL: Duration = Duration::from_millis(10);
const DRAIN_FINISH_GRACE: Duration = Duration::from_millis(250);

#[derive(Debug, Clone)]
pub struct LatticeDriver {
    binary: PathBuf,
}

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("set {LATTICE_BIN_ENV} to the path of a built lattice binary")]
    MissingLatticeBin,
    #[error("{LATTICE_BIN_ENV} points to a missing file: {0}")]
    BinaryNotFound(String),
    #[error("failed to run lattice: {0}")]
    Io(#[from] std::io::Error),
    #[error("command output drain thread panicked")]
    DrainThreadPanicked,
}

/// The direct child's observed kernel termination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessTermination {
    Exited(i32),
    Signaled(i32),
}

/// An action the harness requested, retained in request order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessIntervention {
    InterruptRequested,
    TimedOut(Duration),
    ForcedKill,
}

/// One stage of Linux `/proc` process-group inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessInspectionOperation {
    ReadProc,
    ReadProcEntry,
    ReadStat(u32),
    ParseStat(u32),
}

/// A fallible operation used to reconcile process-group cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupOperation {
    SignalProcessGroup(i32),
    KillChild,
    WaitChild,
    InspectProcessGroup(ProcessInspectionOperation),
}

/// One cleanup failure with its operation and complete I/O diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupError {
    pub operation: CleanupOperation,
    pub kind: std::io::ErrorKind,
    pub raw_os_error: Option<i32>,
    pub message: String,
}

impl CleanupError {
    fn new(operation: CleanupOperation, error: std::io::Error) -> Self {
        Self {
            operation,
            kind: error.kind(),
            raw_os_error: error.raw_os_error(),
            message: error.to_string(),
        }
    }
}

/// Forced-cleanup state, operation failures, and final live group members.
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

/// Captured command bytes with completeness, direct-child observation, intervention, and cleanup.
#[derive(Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_complete: bool,
    pub stderr_complete: bool,
    /// `None` only when cleanup could not wait for the direct child.
    pub termination: Option<ProcessTermination>,
    pub interventions: Vec<HarnessIntervention>,
    pub cleanup: ProcessGroupCleanup,
}

/// Metadata-only command diagnostic. It never renders stdout or stderr bytes.
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
    /// Per-invocation wall-clock timeout. `None` waits indefinitely.
    pub timeout: Option<Duration>,
}

/// A spawned command whose output pipes are drained concurrently.
///
/// The child is always its process-group leader on Linux. Timeout, interrupt,
/// and drop cleanup therefore reach descendants as well as the direct child.
///
/// `pgid` is read back from the kernel with `getpgid(child_pid)` rather than
/// assumed equal to the child pid: a group signal aimed at the wrong group
/// would hit the test process itself, killing the whole `cargo test` run
/// (#105). Every group signal additionally refuses to fire when the target
/// group is this process's own group.
struct DrainResult {
    bytes: Vec<u8>,
    reached_eof: bool,
}

struct DrainHandle {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<std::io::Result<DrainResult>>,
}

pub struct RunningCommand {
    child: Option<Child>,
    pgid: u32,
    stdout: Option<DrainHandle>,
    stderr: Option<DrainHandle>,
    interventions: Vec<HarnessIntervention>,
}

impl RunningCommand {
    pub fn spawn(mut command: Command) -> std::io::Result<Self> {
        command.stdin(Stdio::null());
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
        })
    }

    pub fn pgid(&self) -> u32 {
        self.pgid
    }

    /// Deliver a controlled interrupt to the whole process group.
    pub fn interrupt(&mut self) -> std::io::Result<()> {
        interrupt_process_group(self.pgid)?;
        self.interventions
            .push(HarnessIntervention::InterruptRequested);
        Ok(())
    }

    /// SIGKILL the whole process group and retain bytes drained before death.
    pub fn kill_and_wait(mut self) -> Result<CommandOutcome, DriverError> {
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
        let child = self.child.as_mut().expect("running child");
        record_cleanup_result(&mut cleanup, CleanupOperation::KillChild, child.kill());
        let status = record_cleanup_result(&mut cleanup, CleanupOperation::WaitChild, child.wait());
        self.finish(status, cleanup)
    }

    /// Wait for completion while retaining all bytes drained before cleanup.
    /// Timeout and interrupt are outcomes, not I/O errors.
    pub fn wait(mut self, timeout: Option<Duration>) -> Result<CommandOutcome, DriverError> {
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.as_mut().expect("running child").try_wait()? {
                return self.finish(Some(status), ProcessGroupCleanup::default());
            }
            if let Some(limit) = timeout
                && start.elapsed() >= limit
            {
                self.interventions
                    .push(HarnessIntervention::TimedOut(limit));
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
                let child = self.child.as_mut().expect("running child");
                record_cleanup_result(&mut cleanup, CleanupOperation::KillChild, child.kill());
                let status =
                    record_cleanup_result(&mut cleanup, CleanupOperation::WaitChild, child.wait());
                return self.finish(status, cleanup);
            }
            std::thread::sleep(TIMEOUT_POLL);
        }
    }

    fn finish(
        mut self,
        status: Option<ExitStatus>,
        mut cleanup: ProcessGroupCleanup,
    ) -> Result<CommandOutcome, DriverError> {
        self.child.take();
        if !cleanup.forced
            && process_group_requires_cleanup(&mut cleanup, processes_in_pgid(self.pgid))
        {
            self.interventions.push(HarnessIntervention::ForcedKill);
            cleanup.forced = true;
            record_process_group_signal_result(
                &mut cleanup,
                CleanupOperation::SignalProcessGroup(libc::SIGKILL),
                signal_process_group(self.pgid, libc::SIGKILL),
                || processes_in_pgid(self.pgid),
            );
        }
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
        if let Some(child) = self.child.as_mut() {
            kill_process_group(self.pgid);
            let _ = child.kill();
            let _ = child.wait();
        }
        self.child.take();
        let _ = finish_drain(self.stdout.take());
        let _ = finish_drain(self.stderr.take());
    }
}

impl LatticeDriver {
    pub fn from_env() -> Result<Self, DriverError> {
        let raw = std::env::var(LATTICE_BIN_ENV).map_err(|_| DriverError::MissingLatticeBin)?;
        let path = Path::new(&raw);
        if !path.is_file() {
            return Err(DriverError::BinaryNotFound(raw));
        }
        let binary = std::fs::canonicalize(path)
            .map_err(|_| DriverError::BinaryNotFound(path.display().to_string()))?;
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
        Ok(RunningCommand::spawn(cmd)?)
    }

    pub fn run(
        &self,
        world: &ScenarioWorld,
        command: &LatticeCommand,
    ) -> Result<CommandOutcome, DriverError> {
        self.start(world, command)?.wait(command.timeout)
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

fn set_nonblocking(stream: &impl AsRawFd) -> std::io::Result<()> {
    let fd = stream.as_raw_fd();
    // SAFETY: `fd` is a live child-pipe descriptor. `fcntl` reads and updates
    // only that descriptor's status flags and reports failure through errno.
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
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let Some(mut stream) = maybe else {
            return Ok(DrainResult {
                bytes,
                reached_eof: true,
            });
        };
        let mut chunk = [0_u8; 8192];
        let reached_eof = loop {
            if thread_stop.load(Ordering::Acquire) {
                break false;
            }
            match stream.read(&mut chunk) {
                Ok(0) => break true,
                Ok(read) => bytes.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(TIMEOUT_POLL);
                }
                Err(error) => return Err(error),
            }
        };
        Ok(DrainResult { bytes, reached_eof })
    });
    DrainHandle { stop, thread }
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
    let result = handle
        .thread
        .join()
        .map_err(|_| DriverError::DrainThreadPanicked)?
        .map_err(DriverError::Io)?;
    Ok((result.bytes, result.reached_eof))
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

/// Read the child's actual process group from the kernel.
///
/// `process_group(0)` makes the child its own group leader, so this normally
/// equals the child pid. Reading it back keeps a failed or unsupported
/// `setpgid` from silently leaving `pgid` pointing at the harness's own group.
fn resolve_child_pgid(child: &Child) -> std::io::Result<u32> {
    let pid = child.id();
    // SAFETY: `getpgid` only reads scheduler metadata for `pid`; it has no
    // memory-safety preconditions and reports failure through `errno`.
    let pgid = unsafe { libc::getpgid(pid as libc::pid_t) };
    if pgid < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let pgid = pgid as u32;
    reject_own_process_group(pgid)?;
    Ok(pgid)
}

/// Refuse to treat this process's own group as a signal target.
///
/// Broadcasting into the harness's group interrupts the test binary itself:
/// `cargo test` then dies with exit 130 while reporting zero failures (#105).
/// Failing closed turns that silent, whole-run abort into a local error.
fn reject_own_process_group(pgid: u32) -> std::io::Result<()> {
    // SAFETY: `getpgrp` takes no arguments, reads only the caller's own
    // process group, and cannot fail.
    let own = unsafe { libc::getpgrp() } as u32;
    if pgid == own {
        return Err(std::io::Error::other(format!(
            "refusing to signal process group {pgid}: it is this process's own group"
        )));
    }
    Ok(())
}

fn signal_process_group(pgid: u32, signal: libc::c_int) -> std::io::Result<()> {
    reject_own_process_group(pgid)?;
    // SAFETY: `killpg` only delivers `signal` to the process group; it has no
    // memory-safety preconditions and reports failure through `errno`.
    if unsafe { libc::killpg(pgid as libc::pid_t, signal) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn interrupt_process_group(pgid: u32) -> std::io::Result<()> {
    signal_process_group(pgid, libc::SIGINT)
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
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // The process exited between listing `/proc` and reading its
                // stat file, so it cannot be a final survivor.
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
        if parse_process_stat(pid, &stat)? == Some(pgid) {
            out.push(pid);
        }
    }
    out.sort_unstable();
    Ok(out)
}

fn parse_process_stat(pid: u32, stat: &str) -> Result<Option<u32>, CleanupError> {
    let invalid = |message: &str| {
        CleanupError::new(
            CleanupOperation::InspectProcessGroup(ProcessInspectionOperation::ParseStat(pid)),
            std::io::Error::new(std::io::ErrorKind::InvalidData, message),
        )
    };
    let (_, rest) = stat
        .rsplit_once(')')
        .ok_or_else(|| invalid("process stat is missing the command terminator"))?;
    let mut fields = rest.split_whitespace();
    let state = fields
        .next()
        .ok_or_else(|| invalid("process stat is missing state"))?;
    let _ppid = fields
        .next()
        .ok_or_else(|| invalid("process stat is missing parent pid"))?;
    let member_pgid = fields
        .next()
        .ok_or_else(|| invalid("process stat is missing process group"))?
        .parse::<u32>()
        .map_err(|_| invalid("process stat has an invalid process group"))?;
    Ok((state != "Z").then_some(member_pgid))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process_group_members(pgid: u32) -> Vec<u32> {
        processes_in_pgid(pgid).expect("inspect process group")
    }

    #[test]
    fn exit_records_code_without_intervention_or_forced_cleanup() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "exit 7"]);
        let outcome = RunningCommand::spawn(cmd)
            .expect("spawn")
            .wait(Some(Duration::from_secs(2)))
            .expect("exit outcome");

        assert_eq!(outcome.termination, Some(ProcessTermination::Exited(7)));
        assert!(outcome.interventions.is_empty());
        assert_eq!(outcome.cleanup, ProcessGroupCleanup::default());
        assert_eq!(outcome.exit_code(), Some(7));
        assert_eq!(outcome.signal(), None);
    }

    #[test]
    fn signal_records_concrete_signal_without_magic_exit_code() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "kill -TERM $$"]);
        let outcome = RunningCommand::spawn(cmd)
            .expect("spawn")
            .wait(Some(Duration::from_secs(2)))
            .expect("signal outcome");

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
    fn timeout_retains_partial_output_and_cleans_process_group() {
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            "printf partial-out; printf partial-err >&2; sleep 30 & wait",
        ]);
        let running = RunningCommand::spawn(cmd).expect("spawn");
        let pgid = running.pgid();
        let outcome = running
            .wait(Some(Duration::from_millis(100)))
            .expect("timeout outcome");
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
            .expect("spawn")
            .wait(Some(Duration::from_millis(100)))
            .expect("descendant cleanup outcome");

        assert!(
            started.elapsed() < Duration::from_secs(1),
            "pipe drain exceeded external bound: {:?}",
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
            std::os::unix::net::UnixStream::pair().expect("create local pipe");
        reader
            .set_nonblocking(true)
            .expect("set reader nonblocking");
        let drain = drain_to_thread(Some(reader));
        std::io::Write::write_all(&mut writer, b"prefix").expect("write prefix");

        let (stdout, stdout_complete) = finish_drain(Some(drain)).expect("finish drain");
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
            "an interrupted output drain must contaminate exact exit"
        );
    }

    #[test]
    fn interrupt_timeout_preserves_both_interventions_and_forced_kill() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ready = dir.path().join("ready");
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(format!(
            "trap '' INT; touch '{}'; sleep 30",
            ready.display()
        ));
        let mut running = RunningCommand::spawn(cmd).expect("spawn");
        let deadline = Instant::now() + Duration::from_secs(2);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(ready.exists(), "child never reached interrupt gate");

        running.interrupt().expect("interrupt group");
        let outcome = running
            .wait(Some(Duration::from_millis(100)))
            .expect("interrupt timeout outcome");

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
    fn kill_and_wait_records_forced_kill() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30"]);
        let outcome = RunningCommand::spawn(cmd)
            .expect("spawn")
            .kill_and_wait()
            .expect("kill outcome");

        assert_eq!(
            outcome.termination,
            Some(ProcessTermination::Signaled(libc::SIGKILL))
        );
        assert_eq!(outcome.interventions, vec![HarnessIntervention::ForcedKill]);
        assert!(outcome.cleanup.forced);
        assert!(outcome.cleanup.succeeded(), "{:?}", outcome.cleanup);
        assert!(outcome.cleanup.surviving_pids.is_empty());
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
        let running = RunningCommand::spawn(cmd).expect("spawn");
        let child_pid = running.child.as_ref().expect("running child").id();
        let mut status = 0;
        // SAFETY: `child_pid` belongs to this test and `status` points to a
        // valid integer for `waitpid` to write.
        assert_eq!(
            unsafe { libc::waitpid(child_pid as libc::pid_t, &mut status, 0) },
            child_pid as libc::pid_t
        );

        let outcome = running.kill_and_wait().expect("cleanup outcome");

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
            .expect("temp parent")
            .path()
            .join("missing-proc");
        let root_error =
            processes_in_pgid_at(&missing_root, 7).expect_err("missing proc root must fail");
        assert_eq!(
            root_error.operation,
            CleanupOperation::InspectProcessGroup(ProcessInspectionOperation::ReadProc)
        );

        let proc_root = tempfile::tempdir().expect("temp proc");
        let pid_dir = proc_root.path().join("4242");
        std::fs::create_dir(&pid_dir).expect("pid dir");
        std::fs::create_dir(pid_dir.join("stat")).expect("unreadable stat entry");

        let error = processes_in_pgid_at(proc_root.path(), 7).expect_err("stat read must fail");
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
        for stat in ["4242 malformed", "4242 (test) S 1 nope"] {
            let error = parse_process_stat(4242, stat).expect_err("malformed stat must fail");
            assert_eq!(
                error.operation,
                CleanupOperation::InspectProcessGroup(ProcessInspectionOperation::ParseStat(4242))
            );
            assert_eq!(error.kind, std::io::ErrorKind::InvalidData);
        }
    }

    /// #105: a group signal aimed at the harness's own group would interrupt
    /// the test binary itself. The guard must reject it instead of firing.
    #[test]
    fn signalling_own_process_group_is_refused() {
        // SAFETY: `getpgrp` reads only the caller's own process group.
        let own = unsafe { libc::getpgrp() } as u32;
        let err = signal_process_group(own, libc::SIGINT).expect_err("own group must be refused");
        assert!(
            err.to_string().contains("own group"),
            "unexpected error: {err}"
        );
        // The SIGKILL wrapper swallows errors; it must stay a no-op here
        // rather than tearing down the test run.
        kill_process_group(own);
    }

    /// The spawned child must land in a group that is not the harness's own,
    /// so `interrupt`/`kill` can never reach back into `cargo test`.
    #[test]
    fn spawned_child_pgid_is_read_back_and_distinct() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30"]);
        let running = RunningCommand::spawn(cmd).expect("spawn");
        // SAFETY: `getpgrp` reads only the caller's own process group.
        let own = unsafe { libc::getpgrp() } as u32;
        assert_ne!(
            running.pgid(),
            own,
            "child shares the harness process group"
        );
        let child_pid = running.child.as_ref().expect("running child").id();
        assert_eq!(
            running.pgid(),
            child_pid,
            "child should lead its own process group"
        );
    }

    #[test]
    fn interrupt_reaches_and_reaps_the_process_group() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ready = dir.path().join("ready");
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(format!(
            "trap 'exit 0' INT; sh -c \"trap '' INT; sleep 30\" & \
             touch '{}'; while :; do sleep 1; done",
            ready.display()
        ));
        let mut running = RunningCommand::spawn(cmd).expect("spawn");
        let pgid = running.pgid();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(ready.exists(), "child never reached interrupt gate");
        running.interrupt().expect("interrupt group");
        let outcome = running
            .wait(Some(Duration::from_secs(2)))
            .expect("interrupt outcome");
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
        let deadline = Instant::now() + Duration::from_secs(2);
        while !process_group_members(pgid).is_empty() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(process_group_members(pgid).is_empty());
    }
}
