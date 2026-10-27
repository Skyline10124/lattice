// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! External system harness for the `lattice` binary.
//!
//! This crate must not depend on any `lattice-*`, `lancet-*`, or workspace product
//! crate. It only drives a pre-built binary from `LATTICE_BIN`.

pub mod artifacts;
mod assert;
pub mod cli;
mod daemon;
pub mod driver;
pub mod fake_llm;
pub mod fixture_http;
pub mod provider;
pub mod pty_tui;
pub mod scenario;
pub mod session_send;
pub mod tls;
pub mod tui_mock;
pub mod world;
pub use artifacts::{
    ArtifactScanError, EvidenceError, FailureReport, ScenarioEvidence, WorkspaceSnapshot,
    scan_artifacts_for_secrets, write_workspace_diff, write_workspace_listing, write_world_context,
};
pub use assert::{
    AssertError, assert_exit_code, assert_exit_failure, assert_exit_success, assert_no_secret,
    assert_stderr_contains, assert_stdout_contains, assert_stdout_contains_all, failure_dir,
    finalize_with_failure_artifacts,
};
pub use driver::processes_in_pgid;
pub use driver::{
    CleanupError, CleanupOperation, CommandOutcome, CommandOutcomeSummary, DriverError,
    HarnessIntervention, LatticeCommand, LatticeDriver, ProcessGroupCleanup,
    ProcessInspectionOperation, ProcessTermination, RunningCommand,
};
pub use fake_llm::{
    CapturedRequest, FAKE_API_KEY, FakeLlmServer, ResponseGate, ServeError, SseScript,
};
pub use fixture_http::{CapturedHttpRequest, FixtureHttpError, FixtureHttpServer, HttpFixture};
pub use provider::{ProviderFixture, ProviderProtocol, ProviderTarget};
pub use pty_tui::{PtyError, PtyTuiDriver};
pub use scenario::{ScenarioBudget, ScenarioError, ScenarioLayer, ScenarioSpec};
pub use session_send::{
    AttemptState, CancelRecord, CancelStatus, ConnectionStatus, Entry, EntryContent, Head,
    LocalError, MalformedJsonLine, Outcome, Receipt, ReceiptResult, RetryNotice, Segment,
    SegmentKind, SendExecution, SendExitCode, SendOutcome, SendRecord, SessionSendDriver, Slot,
    SlotOutcome, SlotState, StepEnd, StepEntry, StepProgress, SubmissionReceiptError, Submit,
    TextChunk, TurnSummary, ViewState, derive_send_exit_code, merge_segments, parse_entry,
    parse_record, parse_send_stdout, push_segment,
};
pub use tls::TlsIdentity;
pub use tui_mock::{TuiMockError, TuiMockScenario, run_tui_mock};
pub use world::{ScenarioWorld, WorldError};
