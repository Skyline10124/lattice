// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 同一读事务内的近期调用投影；排序与20条去重边界归会话库。
use lattice_session::{
    ProjectStoreKey,
    store::{self, SessionRead},
};
pub use store::OperatorStatus;

pub fn collect_status(
    read: &SessionRead,
    project: &ProjectStoreKey,
) -> store::Result<OperatorStatus> {
    read.operator_status(project)
}
