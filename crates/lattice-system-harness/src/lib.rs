// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! External system harness for the `lattice` binary.
//!
//! This crate must not depend on any `lattice-*`, `lancet-*`, or workspace product
//! crate. It only drives a pre-built binary from `LATTICE_BIN`.

pub mod artifacts;
mod assert;
pub mod cli;
pub mod driver;
pub mod fake_llm;
pub mod fixture_http;
pub mod pty_tui;
pub mod scenario;
pub mod session_send;
pub mod tui_mock;
pub mod world;
pub use artifacts::{
    ArtifactScanError, EvidenceError, ScenarioEvidence, WorkspaceSnapshot,
    scan_artifacts_for_secrets, write_workspace_diff, write_workspace_listing, write_world_context,
};
pub use assert::{
    AssertError, assert_exit_code, assert_exit_failure, assert_exit_success, assert_no_secret,
    assert_stderr_contains, assert_stdout_contains, assert_stdout_contains_all,
    assert_with_failure_artifacts, failure_dir,
};
pub use driver::processes_in_pgid;
pub use driver::{
    CleanupError, CleanupOperation, CommandOutcome, DriverError, HarnessIntervention,
    LatticeCommand, LatticeDriver, ProcessGroupCleanup, ProcessTermination, RunningCommand,
};
pub use fake_llm::{
    CapturedRequest, FAKE_API_KEY, FakeLlmServer, ResponseGate, ServeError, SseScript,
};
pub use fixture_http::{CapturedHttpRequest, FixtureHttpError, FixtureHttpServer, HttpFixture};
pub use pty_tui::{PtyError, PtyTuiDriver};
pub use scenario::{ScenarioBudget, ScenarioError, ScenarioLayer, ScenarioSpec};
pub use session_send::{
    MalformedJsonLine, SendExecution, SendExitCode, SendOutcome, SessionSendDriver,
    SessionSendError, TurnEventLine, assert_event_subsequence, classify_exit,
    derive_send_exit_code, parse_event_line, parse_send_stdout,
};
pub use tui_mock::{TuiMockError, TuiMockScenario, run_tui_mock};
pub use world::{ScenarioWorld, WorldError};
