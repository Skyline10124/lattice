// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 全局同步会话库。事务调用由组合层置于阻塞执行边界。
//! 句柄独占文件；提交错误后停止所有写入，调用方必须释放读视图并关闭重开。

mod close;
mod codec;
mod local;
mod open;
mod read;
mod types;
mod validate;
mod write;
use crate::pending::hex_digest;
pub use close::{
    bound_provider_diagnostic, first_stopping_cause, plan_auto_close, prepare_terminal,
};
use codec::Records;
use lancet_protocol::{SessionId, TurnId};
use lattice_store::{FlockGuard, StateLayout};
pub use open::detect_legacy_stores;
#[cfg(feature = "test-utils")]
pub use open::with_test_creation_sync_probe;
use std::{path::PathBuf, sync::Mutex};
pub use types::*;

pub struct SessionStore {
    db: redb::Database,
    layout: StateLayout,
    path: PathBuf,
    write_disabled: Mutex<bool>,
}
/// 在观察锁中取得后即可释放观察锁；解码与传输在锁外进行。
pub struct SessionRead {
    tx: redb::ReadTransaction,
}
#[derive(Debug)]
pub struct SessionExecutionLock {
    guard: FlockGuard,
    session_id: SessionId,
}
impl SessionExecutionLock {
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }
}

impl SessionStore {
    /// 在提交健康锁内同步交接已经准备好的执行责任。
    /// 回调只能交给现有监督任务，不得 I/O、await 或等待其他锁。
    pub fn with_execution_start<T>(&self, start: impl FnOnce() -> T) -> Result<T> {
        let _health = self.execution_health()?;
        Ok(start())
    }

    fn execution_health(&self) -> Result<std::sync::MutexGuard<'_, bool>> {
        #[cfg(test)]
        if matches!(
            self.write_disabled.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        ) {
            write_probe(WriteStage::HealthLockContended);
        }
        let disabled = self
            .write_disabled
            .lock()
            .map_err(|_| StoreError::WriteDisabled)?;
        if *disabled {
            return Err(StoreError::WriteDisabled);
        }
        Ok(disabled)
    }
}

fn invalid(field: &'static str, detail: &str) -> StoreError {
    StoreError::InvalidInput {
        field,
        detail: detail.into(),
    }
}
fn corrupt(table: &str, key: &str, detail: &str) -> StoreError {
    StoreError::Corruption {
        table: table.into(),
        key: key.into(),
        detail: detail.into(),
    }
}
fn transition(turn_id: TurnId, detail: &str) -> StoreError {
    StoreError::InvalidTransition {
        turn_id,
        detail: detail.into(),
    }
}
fn missing(kind: &'static str, id: impl ToString) -> StoreError {
    StoreError::NotFound {
        kind,
        id: id.to_string(),
    }
}
fn next_sequence(value: u64, kind: &'static str) -> Result<u64> {
    value
        .checked_add(1)
        .ok_or(StoreError::SequenceExhausted { kind })
}
fn commit_error(source: redb::CommitError) -> StoreError {
    let kind = if matches!(source, redb::CommitError::TransactionPoisoned) {
        CommitFailureKind::TransactionPoisoned
    } else {
        CommitFailureKind::OutcomeUnknown
    };
    StoreError::CommitFailed { kind, source }
}

#[cfg(any(test, feature = "test-utils"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FaultPoint {
    #[cfg(test)]
    Probe,
    #[cfg(test)]
    CreateCommit,
    #[cfg(test)]
    BeforeRename,
    #[cfg(test)]
    AfterRename,
    #[cfg(test)]
    Cleanup,
    Poisoned,
    UnknownBefore,
    UnknownAfter,
}
/// 下游只在同步存储闭包中注入；不跨线程或 await 传播。
#[cfg(feature = "test-utils")]
#[derive(Clone, Copy)]
pub enum TestCommitFault {
    Poisoned,
    UnknownBefore,
    UnknownAfter,
}
#[cfg(feature = "test-utils")]
pub fn with_test_commit_fault<T>(fault: TestCommitFault, operation: impl FnOnce() -> T) -> T {
    with_fault(
        match fault {
            TestCommitFault::Poisoned => FaultPoint::Poisoned,
            TestCommitFault::UnknownBefore => FaultPoint::UnknownBefore,
            TestCommitFault::UnknownAfter => FaultPoint::UnknownAfter,
        },
        operation,
    )
}
#[cfg(any(test, feature = "test-utils"))]
thread_local! {static FAULT:std::cell::Cell<Option<FaultPoint>>=const {std::cell::Cell::new(None)};}
#[cfg(any(test, feature = "test-utils"))]
fn with_fault<T>(point: FaultPoint, f: impl FnOnce() -> T) -> T {
    struct Reset(Option<FaultPoint>);
    impl Drop for Reset {
        fn drop(&mut self) {
            FAULT.set(self.0);
        }
    }
    let _reset = Reset(FAULT.replace(Some(point)));
    f()
}
#[cfg(any(test, feature = "test-utils"))]
fn hit(point: FaultPoint) -> bool {
    FAULT.with(|f| {
        if f.get() == Some(point) {
            f.set(None);
            true
        } else {
            false
        }
    })
}
#[cfg(any(test, feature = "test-utils"))]
fn injected_io() -> std::io::Error {
    std::io::Error::other("injected failure")
}
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum WriteStage {
    HealthLockContended,
    BeforeCommit,
    CommitReturned,
}
#[cfg(test)]
type WriteProbe = Box<dyn Fn(WriteStage)>;
#[cfg(test)]
thread_local! { static WRITE_PROBE: std::cell::RefCell<Option<WriteProbe>> = const { std::cell::RefCell::new(None) }; }
#[cfg(test)]
fn write_probe(stage: WriteStage) {
    WRITE_PROBE.with(|probe| {
        if let Some(probe) = &*probe.borrow() {
            probe(stage);
        }
    });
}
#[cfg(test)]
fn with_write_probe<T>(probe: impl Fn(WriteStage) + 'static, f: impl FnOnce() -> T) -> T {
    struct Reset(Option<WriteProbe>);
    impl Drop for Reset {
        fn drop(&mut self) {
            WRITE_PROBE.with(|p| {
                p.replace(self.0.take());
            });
        }
    }
    let _reset = Reset(WRITE_PROBE.with(|p| p.replace(Some(Box::new(probe)))));
    f()
}
#[cfg(test)]
mod tests;
