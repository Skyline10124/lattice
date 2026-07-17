// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! External system harness for the `lattice` binary.
//!
//! This crate must not depend on any `lattice-*`, `lancet-*`, or workspace product
//! crate. It only drives a pre-built binary from `LATTICE_BIN`.

pub mod artifacts;
mod assert;
pub mod driver;
pub mod fake_llm;
#[cfg(unix)]
pub mod pty_tui;
pub mod scenario;
pub mod session_send;
pub mod world;
pub use artifacts::{
    WorkspaceSnapshot, scan_artifacts_for_secrets, write_workspace_diff, write_workspace_listing,
    write_world_context,
};
pub use assert::{
    AssertError, assert_exit_code, assert_exit_failure, assert_exit_success, assert_no_secret,
    assert_stderr_contains, assert_stdout_contains, assert_stdout_contains_all,
    assert_with_failure_artifacts,
};
#[cfg(all(unix, target_os = "linux"))]
pub use driver::processes_in_pgid;
pub use driver::{DriverError, LatticeCommand, LatticeDriver};
pub use fake_llm::{CapturedRequest, FAKE_API_KEY, FakeLlmServer, ServeError, SseScript};
#[cfg(unix)]
pub use pty_tui::{PtyError, PtyTuiDriver};
pub use scenario::{ScenarioBudget, ScenarioError, ScenarioLayer, ScenarioSpec};
pub use session_send::{
    SendExitCode, SendOutcome, SessionSendDriver, SessionSendError, TurnEventLine,
    assert_event_subsequence, classify_exit, create_session, derive_send_exit_code,
    parse_event_line, parse_send_stdout, send_prompt,
};
pub use world::{ScenarioWorld, WorldError};
