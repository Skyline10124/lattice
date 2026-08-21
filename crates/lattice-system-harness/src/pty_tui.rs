// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `PtyTuiDriver`: drive the prototype TUI through a PTY for smoke scenarios.
//!
//! RFC #41 (Codeberg #19) decision three: PTY is only for one or two smoke
//! scenarios that prove the TUI entrypoint still wires into the same
//! `stream_turn` chain. It does **not** assert on colors, prompt shape, or
//! layout — those are prototype presentation details that must not become
//! long-term contract. Ready recognition (alternate-screen enter) and ANSI
//! cleanup live **inside this driver** only; scenario-level assertions never
//! touch display details.
//!
//! Linux-only. The driver is built on `portable-pty`, a general test library
//! that satisfies the harness boundary (no product crate deps).

use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use thiserror::Error;

use crate::driver::{LatticeDriver, kill_process_group};
use crate::world::ScenarioWorld;

/// Max bytes drained from the PTY before we stop reading. A full-screen TUI
/// redraws whole frames, so byte count is no longer proportional to visible
/// content. 4 MiB bounds a runaway painter without treating a normal session
/// as a display bug.
const READ_BUDGET: usize = 4 * 1024 * 1024;

/// CSI enter-alternate-screen (`ESC [ ? 1049 h`). Terminal protocol, not a
/// presentation detail — this is how the driver knows the full-screen TUI
/// has taken over the PTY and is ready for input.
const ENTER_ALTERNATE_SCREEN: &[u8] = b"\x1b[?1049h";

#[derive(Debug, Error)]
pub enum PtyError {
    #[error("pty io: {0}")]
    Io(#[from] std::io::Error),
    #[error("pty: {0}")]
    Pty(String),
    /// Timed out waiting for output. Carries a bounded, ANSI-stripped tail of
    /// what the TUI *did* emit: without it a quiet TUI is indistinguishable
    /// from a dead one, which turns a one-line bug into a blind CI timeout.
    #[error("tui did not produce matching output within {0:?}; last output: {1:?}")]
    Timeout(Duration, String),
}

/// A live PTY-backed TUI session. Drop kills and reaps the child so no TUI
/// process survives the scenario (PRD user story 20: process-group cleanup).
pub struct PtyTuiDriver {
    /// Kept alive so the PTY stays open; the reader thread owns a clone.
    #[allow(dead_code)]
    master: Box<dyn MasterPty + Send>,
    master_writer: Box<dyn Write + Send>,
    /// Chunks drained by a background reader thread. The thread owns the only
    /// blocking read, so `drain_until` can honour its deadline instead of
    /// parking forever on a quiet PTY.
    rx: Receiver<Vec<u8>>,
    /// Used to kill the child directly (best-effort) on drop/timeout.
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    /// The child itself, kept so `kill_and_reap` can `try_wait` to reap the
    /// zombie after SIGKILL. Without this, the killed child becomes a zombie
    /// whose PID still appears in `/proc/<pid>/stat` with the same pgrp,
    /// causing `processes_in_pgid` to report it as a "live member" even
    /// though it is dead-but-unreaped.
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
    /// The process-group leader pid reported by the master, as a `u32` to
    /// match `driver::kill_process_group` and `driver::processes_in_pgid`
    /// (which both key off `std::process::Child::id()` → `u32`). Used to
    /// kill the whole group (TUI -> lattice -> fake-LLM sockets) on
    /// timeout. `None` when the PTY did not report a valid positive leader.
    pgid: Option<u32>,
    /// The full drained PTY output so far (raw, ANSI preserved).
    output: Vec<u8>,
}

impl PtyTuiDriver {
    /// Spawn `lattice tui --agent <agent> --project-branch <branch>` in a
    /// PTY inside the scenario world. The fake LLM env must already be wired
    /// into the world. The child runs in a new session/process group under
    /// the PTY; `portable-pty` sets it as the controlling terminal, so
    /// `process_group_leader()` returns its pgid and `kill -KILL -<pgid>`
    /// cleans up the whole tree.
    pub fn spawn(
        driver: &LatticeDriver,
        world: &ScenarioWorld,
        agent: &str,
        project_branch: &str,
    ) -> Result<Self, PtyError> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| PtyError::Pty(e.to_string()))?;

        let mut cmd = CommandBuilder::new(driver.binary_path());
        cmd.args(["tui", "--agent", agent, "--project-branch", project_branch]);
        cmd.cwd(world.project_dir());
        cmd.env_clear();
        for (k, v) in world.env_summary() {
            cmd.env(k, v);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| PtyError::Pty(e.to_string()))?;
        let killer = child.clone_killer();
        // `process_group_leader` reports an `i32` (pid_t); a real leader is
        // positive, so non-positive values collapse to `None`. Cast to `u32`
        // to unify with `driver::kill_process_group` / `processes_in_pgid`
        // (which key off `Child::id()` → `u32`), removing the `as u32` casts
        // that previously lived at every call site.
        let pgid = pair
            .master
            .process_group_leader()
            .filter(|p| *p > 0)
            .map(|p| p as u32);

        let writer = pair
            .master
            .take_writer()
            .map_err(|e| PtyError::Pty(e.to_string()))?;

        // Drop the slave so EOF propagates to the child when the master closes.
        drop(pair.slave);

        // One blocking read lives in this thread; the driver only ever waits on
        // the channel. A full-screen TUI is silent whenever nothing changes, so
        // a deadline that is only checked *after* a read returns is no deadline
        // at all — it parks until the process dies.
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| PtyError::Pty(e.to_string()))?;
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        Ok(Self {
            master: pair.master,
            master_writer: writer,
            rx,
            killer,
            child: Some(child),
            pgid,
            output: Vec::new(),
        })
    }

    /// Send a line to the TUI's prompt input, terminated by `\r` (carriage
    /// return) — what a real terminal sends when the user presses Enter.
    ///
    /// `\r`, not `\n`, is load-bearing. The full-screen TUI runs in raw mode,
    /// and crossterm only maps `\n` to `KeyCode::Enter` when raw mode is
    /// *disabled* (crossterm issue #371: with the line discipline off, `\n`
    /// is Ctrl+J, not Enter). A line terminated by `\n` therefore lands in the
    /// input buffer and is never submitted — the turn silently never starts.
    /// The inline REPL tolerated `\n` because readline ran in cooked mode.
    pub fn send_line(&mut self, line: &str) -> Result<(), PtyError> {
        self.master_writer
            .write_all(line.as_bytes())
            .and_then(|_| self.master_writer.write_all(b"\r"))?;
        self.master_writer.flush()?;
        Ok(())
    }

    /// Drain PTY output until `predicate` returns `true` on the accumulated
    /// output (with ANSI escapes stripped and consecutive whitespace collapsed
    /// for matching only), or until `timeout` elapses. Returns the raw
    /// accumulated bytes on success.
    ///
    /// The predicate matches against ANSI-stripped, whitespace-collapsed text
    /// so a smoke scenario can wait for a semantic marker (e.g. a token, a
    /// status word) without depending on colors, cursor moves, or 24×80 wrap
    /// — keeping presentation details out of the long-term contract (RFC #41
    /// decision three / five).
    pub fn wait_for(
        &mut self,
        timeout: Duration,
        predicate: impl Fn(&str) -> bool,
    ) -> Result<Vec<u8>, PtyError> {
        self.drain_until(timeout, |raw| {
            let stripped = strip_ansi(&String::from_utf8_lossy(raw));
            predicate(&collapse_ws(&stripped))
        })
    }

    /// Drain PTY output until `predicate` returns `true` on the accumulated
    /// **raw** bytes (ANSI preserved). Used for terminal-protocol facts such
    /// as alternate-screen enter, which `strip_ansi` would remove.
    pub fn wait_for_raw(
        &mut self,
        timeout: Duration,
        predicate: impl Fn(&[u8]) -> bool,
    ) -> Result<Vec<u8>, PtyError> {
        self.drain_until(timeout, predicate)
    }

    /// Wait until the TUI has entered the alternate screen. This is the
    /// **ready-recognition seam**: the sequence `\x1b[?1049h` lives inside
    /// the driver, never in a scenario (RFC #41 decision three — "prompt
    /// recognition and ANSI cleanup only exist inside the driver; scenarios
    /// assert no display details"). A scenario that needs to know the TUI is
    /// ready for input calls this, not `wait_for(..., |s| s.contains('>'))`.
    ///
    /// Alternate-screen enter is a terminal protocol fact, not a presentation
    /// detail, and is stricter than matching a prompt character the
    /// full-screen shell never prints.
    pub fn wait_for_ready(&mut self, timeout: Duration) -> Result<Vec<u8>, PtyError> {
        self.wait_for_raw(timeout, |bytes| {
            bytes
                .windows(ENTER_ALTERNATE_SCREEN.len())
                .any(|window| window == ENTER_ALTERNATE_SCREEN)
        })
    }

    fn drain_until(
        &mut self,
        timeout: Duration,
        mut predicate: impl FnMut(&[u8]) -> bool,
    ) -> Result<Vec<u8>, PtyError> {
        let start = Instant::now();
        // The predicate may already hold (bytes drained by an earlier wait).
        if predicate(&self.output) {
            return Ok(self.output.clone());
        }
        loop {
            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Err(PtyError::Timeout(timeout, self.tail_for_diagnostics()));
            }
            match self
                .rx
                .recv_timeout(remaining.min(Duration::from_millis(200)))
            {
                Ok(chunk) => {
                    self.output.extend_from_slice(&chunk);
                    if self.output.len() > READ_BUDGET {
                        return Err(PtyError::Pty(format!(
                            "PTY output exceeded {READ_BUDGET} byte budget"
                        )));
                    }
                    if predicate(&self.output) {
                        return Ok(self.output.clone());
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                // Reader thread ended: the child closed the PTY. Nothing more
                // can arrive, so fail now rather than burning the deadline.
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(PtyError::Timeout(
                        start.elapsed(),
                        format!("pty closed; {}", self.tail_for_diagnostics()),
                    ));
                }
            }
        }
    }

    /// Bounded, ANSI-stripped tail of the drained output, for error messages.
    fn tail_for_diagnostics(&self) -> String {
        let text = collapse_ws(&strip_ansi(&String::from_utf8_lossy(&self.output)));
        let tail: String = text
            .chars()
            .rev()
            .take(300)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        tail
    }

    /// The accumulated raw PTY output so far.
    pub fn output(&self) -> &[u8] {
        &self.output
    }

    /// The child's process-group leader pid (if the PTY reports one), as a
    /// `u32` matching `driver::kill_process_group` / `processes_in_pgid`.
    pub fn pgid(&self) -> Option<u32> {
        self.pgid
    }

    /// Kill the whole process group and reap the child. Best-effort: errors
    /// are swallowed because the priority is "no orphaned TUI / sandbox
    /// children", not precise error reporting. Delegates to the shared
    /// `driver::kill_process_group` so the `kill -KILL -<pgid>` shape exists
    /// in exactly one place. Reaps the direct child with a short `try_wait`
    /// poll so it does not linger as a zombie — a zombie's PID is still in
    /// `/proc/<pid>/stat` with the same pgrp, which would make
    /// `processes_in_pgid` report it as a "live member".
    pub fn kill_and_reap(&mut self) {
        let _ = self.killer.kill();
        if let Some(pgid) = self.pgid {
            kill_process_group(pgid);
        }
        // Reap the direct child: poll try_wait for up to ~2s. The child was
        // just SIGKILL'd (via killer + killpg), so it should exit promptly;
        // the poll is a bound, not a blocking wait, so a wedged child does
        // not hang Drop indefinitely.
        if let Some(mut child) = self.child.take() {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                    Err(_) => break, // not actionable; don't hang Drop
                }
            }
            // If still not reaped by the deadline, drop `child` —
            // portable-pty's Child drop on Linux forgets (no kill/wait), so
            // the zombie may linger, but we have done our best within a
            // bounded window.
        }
    }
}

impl Drop for PtyTuiDriver {
    fn drop(&mut self) {
        self.kill_and_reap();
    }
}

/// Strip ANSI escape sequences (CSI, OSC, simple escapes) from `text`. Used
/// only for predicate matching — the raw output is preserved separately so
/// no display detail ever enters a long-term contract.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        // ESC consumed; what follows determines the escape kind.
        match chars.next() {
            Some('[') => {
                // CSI: skip until a 0x40..0x7e terminator.
                #[allow(clippy::while_let_on_iterator)]
                while let Some(t) = chars.next() {
                    if ('@'..='~').contains(&t) {
                        break;
                    }
                }
            }
            Some(']') => {
                // OSC: skip until BEL (\x07) or ST (ESC \).
                #[allow(clippy::while_let_on_iterator)]
                while let Some(t) = chars.next() {
                    if t == '\x07' {
                        break;
                    }
                    if t == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            Some(other) => {
                // Simple two-char escape (e.g. ESC c reset).
                let _ = other;
            }
            None => break,
        }
    }
    out
}

/// Collapse consecutive whitespace (including wrap-induced newlines) to a
/// single space so scenario predicates match semantic text, not 24×80 layout.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_color_and_cursor_codes() {
        let raw = "\x1b[32m> \x1b[0mhello\x1b[1mworld\x1b[0m\x1b[?25h";
        assert_eq!(strip_ansi(raw), "> helloworld");
    }

    #[test]
    fn strip_ansi_handles_osc_title_sequence() {
        let raw = "\x1b]0;some title\x07prompt: ";
        assert_eq!(strip_ansi(raw), "prompt: ");
    }

    #[test]
    fn strip_ansi_handles_st_terminated_osc() {
        let raw = "\x1b]2;window title\x1b\\done";
        assert_eq!(strip_ansi(raw), "done");
    }

    #[test]
    fn strip_ansi_passes_plain_text_through() {
        assert_eq!(strip_ansi("plain text"), "plain text");
    }

    #[test]
    fn collapse_ws_flattens_wrapped_frames() {
        assert_eq!(collapse_ws("hello\n  world"), "hello world");
        assert_eq!(collapse_ws("pong"), "pong");
    }

    #[test]
    fn enter_alternate_screen_is_csi_1049h() {
        assert_eq!(
            ENTER_ALTERNATE_SCREEN,
            &[0x1b, 0x5b, 0x3f, 0x31, 0x30, 0x34, 0x39, 0x68]
        );
    }

    /// Tracer bullet: spawn `cat` in a PTY, send a line, wait for it to be
    /// echoed back. Proves the PTY round-trip works end-to-end without
    /// touching the product binary. `cat` copies stdin to stdout via
    /// the PTY, so the line we write should come back through the master
    /// reader.
    #[test]
    fn pty_round_trip_with_cat() {
        let world = ScenarioWorld::new("lattice-harness-pty-cat").expect("world");
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut cmd = CommandBuilder::new("cat");
        cmd.cwd(world.project_dir());
        cmd.env_clear();
        for (k, v) in world.env_summary() {
            cmd.env(k, v);
        }
        let _child = pair.slave.spawn_command(cmd).expect("spawn cat");
        let mut writer = pair.master.take_writer().expect("writer");
        drop(pair.slave);

        writer.write_all(b"hello-pty\n").expect("write");
        writer.flush().expect("flush");

        let start = Instant::now();
        let mut reader = pair.master.try_clone_reader().expect("reader");
        let mut got = String::new();
        let mut buf = [0u8; 1024];
        while start.elapsed() < Duration::from_secs(5) {
            if let Ok(n) = reader.read(&mut buf)
                && n > 0
            {
                got.push_str(&String::from_utf8_lossy(&buf[..n]));
                if got.contains("hello-pty") {
                    break;
                }
            }
        }
        assert!(
            strip_ansi(&got).contains("hello-pty"),
            "expected echo of hello-pty in PTY output, got: {got:?}"
        );
    }
}
