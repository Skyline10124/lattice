// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Test-only acknowledged crash barriers for the production Turn saga.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use lancet_audit::invocation_store::{InvocationRecord, PinnedLedgerView};
use lancet_protocol::InvocationId;
use lattice_session::PendingTurnTip;
use serde::Serialize;

use super::coordinator::{CoordinatorIoError, TurnCoordinator};

pub(crate) const BARRIER_ENV: &str = "LATTICE_TEST_TURN_BARRIER";
pub(crate) const ACK_ENV: &str = "LATTICE_TEST_TURN_BARRIER_ACK";

#[derive(Serialize)]
struct CrashBarrierFacts {
    barrier: String,
    session_id: String,
    turn_id: String,
    active_turn_oid: String,
    snapshot: lattice_session::pending::PendingSnapshot,
    draft_classification: Option<lattice_session::pending::DraftClassification>,
    ledger_oid: String,
    invocations: BTreeMap<InvocationId, InvocationRecord>,
}

pub(crate) async fn hit(
    coordinator: &TurnCoordinator,
    session_id: &str,
    barrier: &str,
) -> Result<(), CoordinatorIoError> {
    if std::env::var(BARRIER_ENV).ok().as_deref() != Some(barrier) {
        return Ok(());
    }
    let (tip, ledger, draft_classification) = coordinator
        .io
        .crash_barrier_snapshot(session_id.to_string())
        .await?;
    stop_at_barrier(session_id, barrier, tip, ledger, draft_classification);
}

pub(crate) async fn prepare_dispatch(
    coordinator: &TurnCoordinator,
    session_id: &str,
    invocation_id: InvocationId,
) -> Result<(), CoordinatorIoError> {
    if std::env::var(BARRIER_ENV).ok().as_deref() != Some("dispatch_started") {
        return Ok(());
    }
    let (tip, ledger, draft_classification) = coordinator
        .io
        .crash_barrier_snapshot(session_id.to_string())
        .await?;
    let turn_id = tip.snapshot.turn_id;
    let invocations = ledger
        .snapshot
        .invocations
        .into_iter()
        .filter(|(_, record)| record.binding.turn_id == turn_id)
        .collect();
    let facts = CrashBarrierFacts {
        barrier: "dispatch_started_context".to_string(),
        session_id: session_id.to_string(),
        turn_id: turn_id.to_string(),
        active_turn_oid: tip.oid.to_string(),
        snapshot: tip.snapshot,
        draft_classification,
        ledger_oid: ledger.commit_oid.to_string(),
        invocations,
    };
    let mut path = ack_path().into_os_string();
    path.push(".context");
    let path = PathBuf::from(path);
    let value = serde_json::json!({
        "invocation_id": invocation_id,
        "session": facts,
    });
    write_ack(&path, &value);
    Ok(())
}

pub(crate) fn record_tool_effect(invocation_id: InvocationId) {
    let Some(path) = std::env::var_os("LATTICE_TEST_TOOL_EFFECT_LOG") else {
        return;
    };
    let path = PathBuf::from(path);
    assert!(path.is_absolute(), "Tool effect log path must be absolute");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open Tool effect log");
    writeln!(file, "{invocation_id}").expect("append Tool effect log");
    file.sync_all().expect("fsync Tool effect log");
}

fn stop_at_barrier(
    session_id: &str,
    barrier: &str,
    tip: PendingTurnTip,
    ledger: PinnedLedgerView,
    draft_classification: Option<lattice_session::pending::DraftClassification>,
) -> ! {
    let turn_id = tip.snapshot.turn_id;
    let invocations = ledger
        .snapshot
        .invocations
        .into_iter()
        .filter(|(_, record)| record.binding.turn_id == turn_id)
        .collect();
    let facts = CrashBarrierFacts {
        barrier: barrier.to_string(),
        session_id: session_id.to_string(),
        turn_id: turn_id.to_string(),
        active_turn_oid: tip.oid.to_string(),
        snapshot: tip.snapshot,
        draft_classification,
        ledger_oid: ledger.commit_oid.to_string(),
        invocations,
    };
    let path = ack_path();
    write_ack(&path, &facts);
    // SAFETY: this test-only Linux feature stops only the current process. The
    // external harness validates the fsynced ack before SIGKILLing its process
    // group. Returning from SIGSTOP would mean the barrier protocol was broken.
    let result = unsafe { libc::raise(libc::SIGSTOP) };
    panic!("crash barrier resumed unexpectedly (raise returned {result})");
}

fn ack_path() -> PathBuf {
    let path =
        PathBuf::from(std::env::var_os(ACK_ENV).expect("crash barrier ack path is required"));
    assert!(
        path.is_absolute(),
        "crash barrier ack path must be absolute"
    );
    path
}

fn write_ack(path: &Path, facts: &impl Serialize) {
    let bytes = serde_json::to_vec(facts).expect("serialize crash barrier facts");
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .expect("create crash barrier ack");
    file.write_all(&bytes).expect("write crash barrier ack");
    file.write_all(b"\n").expect("terminate crash barrier ack");
    file.sync_all().expect("fsync crash barrier ack");
    let parent = std::fs::File::open(path.parent().expect("ack parent"))
        .expect("open crash barrier ack parent");
    parent.sync_all().expect("fsync crash barrier ack parent");
}
