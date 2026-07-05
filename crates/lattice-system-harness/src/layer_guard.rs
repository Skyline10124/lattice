// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Layer gating for Microsandbox / PTY-smoke scenarios.
//!
//! RFC #19 decision seven + PRD user story 17 are a hard, non-reversible
//! boundary: **explicitly running the full Microsandbox suite must FAIL when
//! the environment is unavailable, never skip.** "Skipped tests must not be
//! used to pretend future behavior is already covered" (RFC #19 irreversible
//! boundary 7).
//!
//! The mechanism:
//!
//! - `#[ignore]` keeps MSB/PTY-smoke scenarios out of ordinary
//!   `cargo test` (which is the fast-binary lane — RFC #19 decision seven).
//! - The owner opts in via `LATTICE_HARNESS_MSB=1` (set by the local full
//!   gate / release gate). When the flag is set, the test body runs. The
//!   first thing the body does is call [`require_msb_env`]: if the flag is
//!   absent the test would not have run anyway (it is `#[ignore]`), but if
//!   the flag is set and the MSB daemon is not contactable, the helper
//!   **panics with a clear message** — that is the "must fail, not skip"
//!   contract. There is no path where the flag is set, MSB is broken, and
//!   the suite reports green.
//!
//! This keeps the fast lane clean (no MSB dependency, no accidental skips)
//! and makes the full gate honest (a broken MSB environment is a real
//! failure the owner must fix, not a silent pass).

use std::process::Command;
use std::time::Duration;

use crate::driver::{kill_process_group, set_new_process_group};

/// Env var the owner sets to opt into the Microsandbox / PTY-smoke suite.
pub const MSB_ENV: &str = "LATTICE_HARNESS_MSB";

/// Returns `true` iff `LATTICE_HARNESS_MSB=1`. Used by `#[ignore]`-gated
/// tests to decide whether to run their body at all.
pub fn msb_requested() -> bool {
    matches!(std::env::var(MSB_ENV).as_deref(), Ok("1"))
}

/// Called at the top of an MSB/PTY-smoke scenario body. When the suite has
/// been opted in (`LATTICE_HARNESS_MSB=1`), this probes the Microsandbox
/// daemon and **panics** if it is not reachable — implementing the
/// "must fail, not skip" contract. When the suite has not been opted in,
/// the `#[ignore]` attribute already prevented the body from running, so
/// reaching this point without the flag set is itself a bug and also
/// panics.
///
/// The probe is intentionally minimal: `lattice lancet status` is the
/// product's own self-check and exercises the same MSB stack a real turn
/// would. A 10s timeout bounds the probe. We do not call the `msb` CLI
/// directly because it may not be on PATH; the product binary is the
/// canonical probe.
pub fn require_msb_env(driver: &crate::driver::LatticeDriver) {
    if !msb_requested() {
        panic!(
            "{} was not set but an MSB-gated scenario body ran; \
             this is a harness bug (the test should be marked #[ignore])",
            MSB_ENV
        );
    }
    // Probe the MSB stack via the product's own self-check. A non-zero or
    // timed-out probe is a real "environment unavailable" failure — we
    // surface it as a panic with a clear message rather than letting the
    // scenario proceed and fail with a confusing MSB error mid-turn.
    let mut cmd = Command::new(driver.binary_path());
    cmd.args(["lancet", "status"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Spawn the probe in its own process group, exactly as the driver does
    // for scenario children. Without this, `child.id()` is the child PID but
    // the child is NOT a group leader, so a timeout `kill -KILL -<pid>` would
    // target group `<pid>` (ESRCH, or an unrelated group that happens to have
    // that id) instead of the probe's tree. `set_new_process_group` makes the
    // child the leader of a fresh group so `kill_process_group(child.id())`
    // reaps the whole probe.
    set_new_process_group(&mut cmd);
    let probe = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => panic!(
            "{MSB_ENV}=1 but `lattice lancet status` could not spawn: {e}; \
             Microsandbox environment is unavailable — fix the daemon or \
             do not set {MSB_ENV}"
        ),
    };
    let mut child = probe;
    let pgid = child.id();
    let start = std::time::Instant::now();
    let status = loop {
        if let Ok(Some(s)) = child.try_wait() {
            break Some(s);
        }
        if start.elapsed() >= Duration::from_secs(10) {
            // Best-effort group kill, mirroring the driver's timeout path.
            kill_process_group(pgid);
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    match status {
        Some(s) if s.success() => {}
        other => panic!(
            "{MSB_ENV}=1 but `lattice lancet status` reported the MSB stack \
             is unavailable (exit={other:?}). Microsandbox environment is \
             not ready — the full gate must fail rather than skip."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msb_requested_is_false_when_unset() {
        // SAFETY: `remove_var` is process-global but we run it before a
        // read of the same var in the same test; no other code in this test
        // process reads MSB_ENV concurrently. Rust 2024 marks env mutation
        // unsafe to flag the data-race risk, not because the call itself is
        // unsound.
        unsafe {
            std::env::remove_var(MSB_ENV);
        }
        assert!(!msb_requested());
    }

    #[test]
    fn msb_requested_is_true_when_one() {
        // SAFETY: same single-threaded reasoning as above; we restore at the
        // end so other tests see the unset state.
        unsafe {
            std::env::set_var(MSB_ENV, "1");
        }
        let v = msb_requested();
        unsafe {
            std::env::remove_var(MSB_ENV);
        }
        assert!(v);
    }
}
