// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 操作员查询复用调用方持有的读事务，不打开库或组合执行器。

use lancet_protocol::{InvocationId, SessionId, TurnId};
use lattice_session::{
    ProjectStoreKey,
    store::{self, SessionRead},
};
pub use store::{
    AuditFilter, AuditPage, AuditReplay, AuditTailPage, ChangeSeq, InvocationChange,
    OperatorInvocation, OperatorInvocationState,
};

pub fn query_audit(
    read: &SessionRead,
    filter: &AuditFilter,
    after: Option<InvocationId>,
    limit: usize,
) -> store::Result<AuditPage> {
    read.query_audit(filter, after, limit)
}

pub fn tail_audit(
    read: &SessionRead,
    filter: &AuditFilter,
    after: ChangeSeq,
    limit: usize,
) -> store::Result<AuditTailPage> {
    read.tail_audit(filter, after, limit)
}

pub fn replay_audit(
    read: &SessionRead,
    project: &ProjectStoreKey,
    session: SessionId,
    turn: TurnId,
) -> store::Result<AuditReplay> {
    read.replay_audit(project, session, turn)
}
