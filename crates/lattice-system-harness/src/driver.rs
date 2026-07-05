// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::scenario::ScenarioSpec;
use crate::world::ScenarioWorld;

const LATTICE_BIN_ENV: &str = "LATTICE_BIN";

/// Poll interval used when enforcing a per-command timeout.
const TIMEOUT_POLL: Duration = Duration::from_millis(10);

/// Resolved path to the external `lattice` binary under test.
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
    #[error("lattice command timed out after {0:?}")]
    Timeout(Duration),
}

/// One invocation of the product binary inside a scenario world.
#[derive(Debug, Clone)]
pub struct LatticeCommand {
    pub args: Vec<String>,
    /// Per-invocation wall-clock timeout. `None` waits indefinitely.
    pub timeout: Option<Duration>,
}

impl LatticeDriver {
    /// Resolve the binary only from `LATTICE_BIN`.
    ///
    /// The path is checked and canonicalized **in the test process**, i.e.
    /// relative to the test-process cwd, before any scenario child is spawned.
    /// `canonicalize` stores an absolute path so the later child `current_dir`
    /// change to the per-scenario project dir cannot break resolution. A
    /// relative `LATTICE_BIN` therefore works as long as it is a real file from
    /// the test-process cwd; an absolute path is just the cwd-independent way
    /// to guarantee that.
    pub fn from_env() -> Result<Self, DriverError> {
        let raw = std::env::var(LATTICE_BIN_ENV).map_err(|_| DriverError::MissingLatticeBin)?;
        let path = Path::new(&raw);
        if !path.is_file() {
            return Err(DriverError::BinaryNotFound(raw.clone()));
        }
        let binary =
            std::fs::canonicalize(path).map_err(|_| DriverError::BinaryNotFound(raw.clone()))?;
        Ok(Self { binary })
    }

    pub fn binary_path(&self) -> &Path {
        &self.binary
    }

    pub fn run(
        &self,
        world: &ScenarioWorld,
        command: &LatticeCommand,
    ) -> Result<Output, DriverError> {
        let mut cmd = world.command(&self.binary);
        cmd.args(&command.args);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        // Drain stdout/stderr concurrently so a large output cannot fill the
        // pipe buffer and deadlock the child while we wait for the timeout.
        let stdout_handle = drain_to_thread(child.stdout.take());
        let stderr_handle = drain_to_thread(child.stderr.take());

        let status = wait_with_timeout(&mut child, command.timeout)?;
        let stdout = join_drain(stdout_handle);
        let stderr = join_drain(stderr_handle);
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }

    pub fn help(&self, world: &ScenarioWorld, spec: &ScenarioSpec) -> Result<Output, DriverError> {
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
    ) -> Result<Output, DriverError> {
        self.run(
            world,
            &LatticeCommand {
                args: Vec::new(),
                timeout: Some(spec.step_timeout()),
            },
        )
    }
}

fn drain_to_thread<R: Read + Send + 'static>(maybe: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut stream) = maybe {
            // A read error here is not actionable from the caller's perspective;
            // an empty buffer is preferable to a panic that masks the real
            // exit status / timeout outcome.
            let _ = stream.read_to_end(&mut buf);
        }
        buf
    })
}

fn join_drain(handle: std::thread::JoinHandle<Vec<u8>>) -> Vec<u8> {
    handle.join().unwrap_or_default()
}

fn wait_with_timeout(
    child: &mut Child,
    timeout: Option<Duration>,
) -> Result<std::process::ExitStatus, DriverError> {
    let Some(limit) = timeout else {
        return child.wait().map_err(DriverError::Io);
    };
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(DriverError::Io)? {
            return Ok(status);
        }
        if start.elapsed() >= limit {
            // Best-effort kill; reap to avoid leaving a zombie, then surface the
            // timeout regardless of the kill/wait outcome.
            let _ = child.kill();
            let _ = child.wait();
            return Err(DriverError::Timeout(limit));
        }
        std::thread::sleep(TIMEOUT_POLL);
    }
}
