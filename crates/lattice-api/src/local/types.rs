// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

pub(crate) use crate::models::ModelCandidateQuery;
pub(crate) use lattice_conversation::{SessionId, TurnId};
pub(crate) use lattice_ipc::{
    Acceptance, AuthLoggedOut, AuthRemoval, ConfigureReply, ConfigureRequest, ConfigureSession,
    ConflictReason, DaemonStatus, EffortAdjustment, LocalError, LocalErrorKind, LocalHistory,
    MAX_HISTORY_TURNS, RecoveryCause, Request, Response, Result, SessionHead, SessionListRow,
    StoreDiagnostic, TextUpdate, TurnLookup, TurnRequest, TurnView, ViewUpdate, WATCH_TURNS,
};
pub(crate) use lattice_session::store::Page;

#[cfg(test)]
pub(crate) use crate::turn::{TextSegment, TurnRef};
#[cfg(test)]
pub(crate) use crate::{AgentName, ModelId, ModelRef, ProviderId};
#[cfg(test)]
pub(crate) use lattice_conversation::{Effort, MessageRole, ModelAttemptKey, NativeMessage};
#[cfg(test)]
pub(crate) use lattice_ipc::{
    AuthLoginOutcome, AuthStatus, Availability, CancelStatus, ConfigurationRequirement,
    ConfigureResult, ConnectionStatus, LocalUpdate, ProviderAuthStatus, RevocationStatus,
    SubmissionReceipt, SubmissionResult, SubmissionUpdate,
};
#[cfg(test)]
pub(crate) use lattice_session::store::{
    ConversationEntry, Node, NodeId, NodeRef, PublicTurnOutcome, StepDetail, StepEnd, StepKey,
    StepProgress, TurnDetail, TurnSummary, UsageSummary,
};
