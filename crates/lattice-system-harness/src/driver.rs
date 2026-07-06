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
        // Spawn in its own process group so a timeout can kill the whole tree
        // (TUI -> microsandbox -> fake LLM sockets), not only the direct
        // child (RFC #19 decision eight, PRD user story 20). `process_group(0)`
        // makes the child the leader of a new session/process group.
        set_new_process_group(&mut cmd);
        let mut child = cmd.spawn()?;
        let pgid = child.id();
        // Drain stdout/stderr concurrently so a large output cannot fill the
        // pipe buffer and deadlock the child while we wait for the timeout.
        let stdout_handle = drain_to_thread(child.stdout.take());
        let stderr_handle = drain_to_thread(child.stderr.take());

        let status = wait_with_timeout(&mut child, command.timeout, pgid)?;
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

/// Put a `Command`'s child into a new process group where it is the leader.
/// Unix-only; on non-Unix targets this is a no-op (the repo does not commit
/// to Windows, per #26 Out-of-Scope).
///
/// `pub(crate)` so the layer-gate probe (`layer_guard::require_msb_env`) and
/// the PTY driver reuse the same setup instead of each re-deriving it — the
/// probe in particular must spawn its `lattice lancet status` child in its
/// own group so a timeout `kill_process_group(child.id())` hits the right
/// group (the child is the leader), not an unrelated group or ESRCH.
#[cfg(unix)]
pub(crate) fn set_new_process_group(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(not(unix))]
pub(crate) fn set_new_process_group(_cmd: &mut std::process::Command) {}

/// Kill an entire process group. Best-effort: errors are swallowed because
/// the caller is already on the timeout/failure path and the priority is
/// "no orphaned TUI / sandbox / fake-server children", not precise error
/// reporting. Uses the external `kill` utility to avoid a `libc` dependency
/// (the harness boundary allows general test libraries, but keeping the
/// dependency surface minimal is the documented discipline).
///
/// `pub(crate)` so `layer_guard` and `pty_tui` share the one implementation
/// rather than each inlining `kill -KILL -<pgid>` (the duplicated shape had
/// drifted: `layer_guard` forgot `set_new_process_group`, and `pty_tui` used
/// `i32` pgids while this helper used `u32`).
pub(crate) fn kill_process_group(pgid: u32) {
    let pgid_str = format!("-{pgid}");
    let _ = std::process::Command::new("kill")
        .arg("-KILL")
        .arg(&pgid_str)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn wait_with_timeout(
    child: &mut Child,
    timeout: Option<Duration>,
    pgid: u32,
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
            // Kill the process group first so grandchildren (TUI, microsandbox,
            // fake-LLM sockets) die with the child, then reap the direct child
            // to avoid a zombie. Surface the timeout regardless.
            kill_process_group(pgid);
            let _ = child.kill();
            let _ = child.wait();
            return Err(DriverError::Timeout(limit));
        }
        std::thread::sleep(TIMEOUT_POLL);
    }
}

/// Read `/proc/<pid>/stat` for every pid and return the pids whose process
/// group id == `pgid` and that are **not zombies**. Linux-only; returns
/// empty on non-Linux Unix. Used by the process-group-kill test and the PTY
/// smoke scenario to verify no orphan survives a `killpg`.
///
/// Zombies (state `Z`) are excluded: a killed child that has not yet been
/// reaped still has its pgrp in `/proc/<pid>/stat`, but it is not a "live
/// member" — it cannot run, hold sockets, or spawn children. The direct
/// child is reaped by `PtyTuiDriver::kill_and_reap`; grandchildren are reaped
/// by init after the child dies. Excluding zombies here avoids a false
/// positive during the brief window between SIGKILL and reap.
#[cfg(all(unix, target_os = "linux"))]
pub fn processes_in_pgid(pgid: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir("/proc") {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = match name.to_str() {
            Some(s) => s,
            None => continue,
        };
        let pid: u32 = match name.parse() {
            Ok(n) => n,
            Err(_) => continue,
        };
        // /proc/<pid>/stat: pid (comm) state ppid pgrp ...
        // pgrp is field 5. The comm field is parenthesized and may contain
        // spaces/parens, so split on the closing paren first.
        let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let after_comm = match stat.rsplit_once(')') {
            Some((_, rest)) => rest.trim_start(),
            None => continue,
        };
        let mut fields = after_comm.split_whitespace();
        let state = fields.next();
        let _ppid = fields.next();
        let pgrp = fields.next();
        // Exclude zombies (state 'Z'): they are dead-but-unreaped, not live.
        if state == Some("Z") {
            continue;
        }
        if let Some(pgrp) = pgrp.and_then(|s| s.parse::<u32>().ok())
            && pgrp == pgid
        {
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

    /// Tracer bullet: a child that finishes on its own returns its real
    /// exit status, and the process-group setup does not break the happy
    /// path. Uses `true` so it exits 0 immediately.
    #[cfg(unix)]
    #[test]
    fn run_child_in_own_process_group_returns_exit_status() {
        let world = ScenarioWorld::new("lattice-harness-pgrp-ok").expect("world");
        let mut cmd = world.command("true");
        set_new_process_group(&mut cmd);
        let mut child = cmd.spawn().expect("spawn true");
        let pgid = child.id();
        let status =
            wait_with_timeout(&mut child, Some(Duration::from_secs(5)), pgid).expect("wait");
        assert!(status.success());
    }

    /// The core #21 acceptance point: a timed-out child must be killed, and
    /// the timeout must surface as `DriverError::Timeout`. Spawns `sleep 30`
    /// in its own process group with a 1s timeout; on success the wait
    /// returns `Timeout` quickly and the child is reaped (no zombie).
    #[cfg(unix)]
    #[test]
    fn timed_out_child_is_killed_and_timeout_surfaces() {
        let world = ScenarioWorld::new("lattice-harness-pgrp-timeout").expect("world");
        let mut cmd = world.command("sleep");
        cmd.arg("30");
        set_new_process_group(&mut cmd);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn sleep 30");
        let pgid = child.id();
        let start = Instant::now();
        let err = wait_with_timeout(&mut child, Some(Duration::from_secs(1)), pgid)
            .expect_err("should time out");
        assert!(matches!(err, DriverError::Timeout(_)), "got {err:?}");
        assert!(start.elapsed() < Duration::from_secs(10));
        // try_wait after a timeout returns Some(exited): the kill+wait inside
        // wait_with_timeout reaps the child (no zombie).
        let reaped = child.try_wait().expect("try_wait");
        assert!(
            reaped.is_some(),
            "child should be reaped, not still running"
        );
    }

    /// A timed-out child's *grandchild* must also die when the process group
    /// is killed — this is the actual #21 / PRD user story 20 requirement.
    /// Spawns a shell that itself spawns `sleep 30` (the grandchild), then
    /// waits. A 1s timeout must kill both; we verify by scanning /proc for
    /// any survivor in the process group. This is the contract single-child
    /// `kill()` does NOT satisfy and `killpg` does.
    #[cfg(unix)]
    #[test]
    fn timed_out_process_group_kills_grandchild() {
        let world = ScenarioWorld::new("lattice-harness-pgrp-grandchild").expect("world");
        // `sh -c 'sleep 30 & wait'`: both share the new process group.
        let mut cmd = world.command("sh");
        cmd.args(["-c", "sleep 30 & wait"]);
        set_new_process_group(&mut cmd);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn sh");
        let pgid = child.id();
        std::thread::sleep(Duration::from_millis(300));
        let err = wait_with_timeout(&mut child, Some(Duration::from_secs(1)), pgid)
            .expect_err("should time out");
        assert!(matches!(err, DriverError::Timeout(_)));

        std::thread::sleep(Duration::from_millis(400));
        let reaped = child.try_wait().expect("try_wait");
        assert!(reaped.is_some(), "group leader should be reaped");

        // No process in the group should survive the killpg.
        let survivors = processes_in_pgid(pgid);
        assert!(
            survivors.is_empty(),
            "process group {pgid} still has live members after killpg: {survivors:?}"
        );
    }

    /// `processes_in_pgid` must exclude zombies (state `Z`): a killed child
    /// that has not been reaped still has its pgrp in `/proc/<pid>/stat`, but
    /// it is not a live member — it cannot run, hold sockets, or spawn
    /// children. This is the `continue` branch at the `state == Some("Z")`
    /// check. We spawn a shell that starts a background `sleep 0.1` and then
    /// blocks in `sleep 3`; the background job exits and becomes a zombie the
    /// shell never reaps. We then (a) independently confirm a zombie is
    /// actually present in the group — so the `continue` branch is exercised,
    /// not silently skipped — and (b) assert `processes_in_pgid` omits it.
    #[cfg(target_os = "linux")]
    #[test]
    fn processes_in_pgid_excludes_zombies() {
        let world = ScenarioWorld::new("lattice-harness-pgrp-zombie").expect("world");
        let mut cmd = world.command("sh");
        // Start a short-lived background `sleep` and then `exec sleep N` so
        // the shell process is *replaced* by the long `sleep`. A non-interactive
        // shell (the default under `-c`) auto-reaps background children whenever
        // it regains control, so if the shell kept running it would reap the
        // background job before we sample /proc. `exec sleep 10` overwrites the
        // shell image with a `sleep` that never calls `wait`, so the exited
        // background `sleep 0.2` lingers as a zombie in the same process group
        // (the shell did not create a new pgrp for it). We sample /proc while
        // the exec'd `sleep` is still alive.
        cmd.args(["-c", "sleep 0.2 & exec sleep 10"]);
        set_new_process_group(&mut cmd);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn sh");
        let pgid = child.id();

        // Let the background `sleep 0.2` exit and become a zombie the exec'd
        // `sleep` never reaps.
        std::thread::sleep(Duration::from_millis(700));

        // Independently scan /proc for the group, splitting by state. This
        // mirrors `processes_in_pgid`'s parsing but is a separate read, so we
        // can prove the `continue` branch is actually hit (a zombie exists).
        let mut zombie_pids = Vec::new();
        let mut all_members = Vec::new();
        if let Ok(entries) = std::fs::read_dir("/proc") {
            for entry in entries.flatten() {
                let name = match entry.file_name().to_str() {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                let pid = match name.parse::<u32>() {
                    Ok(n) => n,
                    Err(_) => continue,
                };
                let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let Some((_, rest)) = stat.rsplit_once(')') else {
                    continue;
                };
                let mut fields = rest.split_whitespace();
                let state = fields.next();
                let _ppid = fields.next();
                let pgrp = fields.next();
                if let Some(pgrp) = pgrp.and_then(|s| s.parse::<u32>().ok())
                    && pgrp == pgid
                {
                    all_members.push((pid, state.map(str::to_string)));
                    if state == Some("Z") {
                        zombie_pids.push(pid);
                    }
                }
            }
        }
        assert!(
            !zombie_pids.is_empty(),
            "precondition failed: no zombie in process group {pgid}; the \
             `continue` branch would not be exercised. members: {all_members:?}"
        );

        let live = processes_in_pgid(pgid);
        assert!(
            !live.is_empty(),
            "expected at least one live (non-zombie) member in group {pgid}"
        );
        for zpid in &zombie_pids {
            assert!(
                !live.contains(zpid),
                "zombie pid {zpid} must not be reported as live; got {live:?}"
            );
        }
        // Belt-and-suspenders: no reported pid may actually be a zombie.
        for &pid in &live {
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                continue;
            };
            let Some((_, rest)) = stat.rsplit_once(')') else {
                continue;
            };
            let state = rest.split_whitespace().next();
            assert_ne!(
                state,
                Some("Z"),
                "pid {pid} is a zombie but was reported as live"
            );
        }

        // Cleanup: kill the whole group and reap the direct child so the
        // test leaves no survivors (the zombie is reaped by init once the
        // shell dies).
        kill_process_group(pgid);
        let _ = child.wait();
    }
}
