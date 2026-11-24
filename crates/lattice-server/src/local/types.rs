// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

pub(crate) use lattice_conversation::{SessionId, TurnId};
pub(crate) use lattice_ipc::{
    Acceptance, AuthLoggedOut, AuthRemoval, ComponentState, ComponentStatus, ConfigureReply,
    ConfigureRequest, ConfigureSession, ConflictReason, DaemonStatus, EffortAdjustment,
    ImAuthRemoveReply, ImAuthSetReply, ImAuthStatus, ImCredentialStatus, ImMountResult,
    ImPlatformAuthStatus, LocalError, LocalErrorKind, LocalHistory, MAX_HISTORY_TURNS,
    ModelCandidateQuery, RecoveryCause, Request, Response, Result, SessionHead, SessionListRow,
    StoreDiagnostic, TextUpdate, TurnLookup, TurnRequest, TurnView, UserInputPart, ViewUpdate,
    WATCH_TURNS,
};
pub(crate) use lattice_session::store::Page;

#[cfg(test)]
pub(crate) use lattice_conversation::{Effort, ModelAttemptKey, ModelId, ModelRef, ProviderId};
#[cfg(test)]
pub(crate) use lattice_ipc::{
    AuthLoginOutcome, AuthStatus, Availability, CancelStatus, ConfigurationRequirement,
    ConfigureResult, ConnectionStatus, LocalUpdate, ProviderAuthStatus, RevocationStatus,
    SubmissionReceipt, SubmissionResult, SubmissionUpdate,
};
#[cfg(test)]
pub(crate) use lattice_ipc::{TextSegment, TurnRef};
#[cfg(test)]
pub(crate) use lattice_session::AgentName;
#[cfg(test)]
pub(crate) use lattice_session::store::{
    ConversationEntry, Node, NodeId, NodeRef, PublicTurnOutcome, StepDetail, StepEnd, StepKey,
    StepProgress, TurnDetail, TurnSummary, UsageSummary,
};
