// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::scenario::ScenarioSpec;
use crate::world::ScenarioWorld;

const LATTICE_BIN_ENV: &str = "LATTICE_BIN";
const TIMEOUT_POLL: Duration = Duration::from_millis(10);

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Termination {
    Exited(Option<i32>),
    TimedOut(Duration),
    Interrupted,
}

#[derive(Debug)]
pub struct CommandOutcome {
    pub output: Output,
    pub termination: Termination,
}

impl std::ops::Deref for CommandOutcome {
    type Target = Output;

    fn deref(&self) -> &Self::Target {
        &self.output
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
/// The child is always its process-group leader on Unix. Timeout, interrupt,
/// and drop cleanup therefore reach descendants as well as the direct child.
pub struct RunningCommand {
    child: Option<Child>,
    pgid: u32,
    stdout: Option<JoinHandle<std::io::Result<Vec<u8>>>>,
    stderr: Option<JoinHandle<std::io::Result<Vec<u8>>>>,
    interrupted: bool,
}

impl RunningCommand {
    pub fn spawn(mut command: Command) -> std::io::Result<Self> {
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        set_new_process_group(&mut command);
        let mut child = command.spawn()?;
        let pgid = child.id();
        let stdout = Some(drain_to_thread(child.stdout.take()));
        let stderr = Some(drain_to_thread(child.stderr.take()));
        Ok(Self {
            child: Some(child),
            pgid,
            stdout,
            stderr,
            interrupted: false,
        })
    }

    pub fn pgid(&self) -> u32 {
        self.pgid
    }

    /// Deliver a controlled interrupt to the whole process group.
    pub fn interrupt(&mut self) -> std::io::Result<()> {
        signal_process_group(self.pgid, "-INT")?;
        self.interrupted = true;
        Ok(())
    }

    /// Wait for completion while retaining all bytes drained before cleanup.
    /// Timeout and interrupt are outcomes, not I/O errors.
    pub fn wait(mut self, timeout: Option<Duration>) -> Result<CommandOutcome, DriverError> {
        let start = Instant::now();
        let child = self.child.as_mut().expect("running child");
        let (status, termination) = loop {
            if let Some(status) = child.try_wait()? {
                let termination = if self.interrupted {
                    Termination::Interrupted
                } else {
                    Termination::Exited(status.code())
                };
                break (status, termination);
            }
            if let Some(limit) = timeout
                && start.elapsed() >= limit
            {
                kill_process_group(self.pgid);
                let _ = child.kill();
                let status = child.wait()?;
                let termination = if self.interrupted {
                    Termination::Interrupted
                } else {
                    Termination::TimedOut(limit)
                };
                break (status, termination);
            }
            std::thread::sleep(TIMEOUT_POLL);
        };
        if self.interrupted {
            // The direct child may exit on SIGINT while a descendant ignores it.
            // Force-close the entire group before joining pipe drains so no
            // descendant can retain stdout/stderr and hang this wait forever.
            kill_process_group(self.pgid);
        }
        self.child.take();
        let stdout = join_drain(self.stdout.take())?;
        let stderr = join_drain(self.stderr.take())?;
        Ok(CommandOutcome {
            output: Output {
                status,
                stdout,
                stderr,
            },
            termination,
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
        if let Some(handle) = self.stdout.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.stderr.take() {
            let _ = handle.join();
        }
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

fn drain_to_thread<R: Read + Send + 'static>(
    maybe: Option<R>,
) -> JoinHandle<std::io::Result<Vec<u8>>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut stream) = maybe {
            stream.read_to_end(&mut buf)?;
        }
        Ok(buf)
    })
}

fn join_drain(
    handle: Option<JoinHandle<std::io::Result<Vec<u8>>>>,
) -> Result<Vec<u8>, DriverError> {
    match handle {
        None => Ok(Vec::new()),
        Some(handle) => handle
            .join()
            .map_err(|_| DriverError::DrainThreadPanicked)?
            .map_err(DriverError::Io),
    }
}

#[cfg(unix)]
pub(crate) fn set_new_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(not(unix))]
pub(crate) fn set_new_process_group(_cmd: &mut Command) {}

#[cfg(unix)]
fn signal_process_group(pgid: u32, signal: &str) -> std::io::Result<()> {
    let status = Command::new("kill")
        .arg(signal)
        .arg(format!("-{pgid}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "kill {signal} -{pgid} exited {status}"
        )))
    }
}

#[cfg(not(unix))]
fn signal_process_group(_pgid: u32, _signal: &str) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "process-group signals require Unix",
    ))
}

pub(crate) fn kill_process_group(pgid: u32) {
    let _ = signal_process_group(pgid, "-KILL");
}

#[cfg(all(unix, target_os = "linux"))]
pub fn processes_in_pgid(pgid: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some((_, rest)) = stat.rsplit_once(')') else {
            continue;
        };
        let mut fields = rest.split_whitespace();
        let state = fields.next();
        let _ppid = fields.next();
        let member_pgid = fields.next().and_then(|s| s.parse::<u32>().ok());
        if state != Some("Z") && member_pgid == Some(pgid) {
            out.push(pid);
        }
    }
    out
}

#[cfg(all(unix, not(target_os = "linux")))]
pub fn processes_in_pgid(_pgid: u32) -> Vec<u32> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
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
            Termination::TimedOut(Duration::from_millis(100))
        );
        assert_eq!(outcome.stdout, b"partial-out");
        assert_eq!(outcome.stderr, b"partial-err");
        let deadline = Instant::now() + Duration::from_secs(2);
        while !processes_in_pgid(pgid).is_empty() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(processes_in_pgid(pgid).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn interrupt_reaches_and_reaps_the_process_group() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ready = dir.path().join("ready");
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(format!(
            "touch '{}'; trap 'exit 0' INT; \
             sh -c \"trap '' INT; sleep 30\" & wait",
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
        assert_eq!(outcome.termination, Termination::Interrupted);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !processes_in_pgid(pgid).is_empty() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(processes_in_pgid(pgid).is_empty());
    }
}
