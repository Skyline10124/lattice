// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 显式故障产物的提交后停点；marker 仅记录身份和事务序号，不复制正文。
use super::coordinator::{CoordinatorIoError, CoordinatorStoreIo};
use lancet_protocol::{InvocationBinding, InvocationId, SessionId, TurnId};
use lattice_session::store::ChangeSeq;
use serde::Serialize;
use std::{io::Write, path::PathBuf};

#[derive(Serialize)]
struct Marker {
    barrier: &'static str,
    session_id: SessionId,
    turn_id: Option<TurnId>,
    current: Option<lattice_session::store::NodeId>,
    step: Option<u32>,
    part: Option<u32>,
    invocation_id: Option<InvocationId>,
    change_seq: ChangeSeq,
}
fn selected(config: &str, name: &str, step: Option<u32>, part: Option<u32>) -> bool {
    config == name
        || step.is_some_and(|step| config == format!("{name}:{step}"))
        || step
            .zip(part)
            .is_some_and(|(step, part)| config == format!("{name}:{step}:{part}"))
}
pub(crate) async fn hit(
    io: &CoordinatorStoreIo,
    turn_id: TurnId,
    barrier: &'static str,
    step: Option<u32>,
    binding: Option<&InvocationBinding>,
) -> Result<(), CoordinatorIoError> {
    schedule(
        barrier,
        serde_json::json!({
            "session_id":io.session_id(),"turn_id":turn_id,"step":step,
            "invocation_id":binding.map(|b| b.invocation_id)
        }),
    )
    .await;
    if !std::env::var("LATTICE_TEST_TURN_BARRIER")
        .ok()
        .is_some_and(|config| {
            selected(
                &config,
                barrier,
                step,
                binding.map(|b| b.identity.part_ordinal),
            )
        })
    {
        return Ok(());
    }
    let session_id = io.session_id();
    let change_seq = io
        .read(move |read| {
            let project = read.session(session_id)?.project_store_key;
            Ok(read.operator_status(&project)?.latest_change)
        })
        .await?;
    let marker = Marker {
        barrier,
        session_id,
        turn_id: Some(turn_id),
        current: None,
        step,
        part: binding.map(|b| b.identity.part_ordinal),
        invocation_id: binding.map(|b| b.invocation_id),
        change_seq,
    };
    stop(marker).await
}

pub(crate) async fn rewind_hit(
    io: &CoordinatorStoreIo,
    session: &lattice_session::store::SessionRecord,
) -> Result<(), CoordinatorIoError> {
    if std::env::var("LATTICE_TEST_TURN_BARRIER").as_deref() != Ok("rewind_committed") {
        return Ok(());
    }
    let project = session.project_store_key;
    let change_seq = io
        .read(move |read| Ok(read.operator_status(&project)?.latest_change))
        .await?;
    stop(Marker {
        barrier: "rewind_committed",
        session_id: session.session_id,
        turn_id: None,
        current: Some(session.current),
        step: None,
        part: None,
        invocation_id: None,
        change_seq,
    })
    .await
}

async fn stop(marker: Marker) -> Result<(), CoordinatorIoError> {
    tokio::task::spawn_blocking(move || {
        let path = PathBuf::from(
            std::env::var_os("LATTICE_TEST_TURN_BARRIER_ACK").expect("barrier ack required"),
        );
        assert!(path.is_absolute(), "barrier ack must be absolute");
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .expect("create barrier ack");
        serde_json::to_writer(&mut file, &marker).expect("encode barrier ack");
        file.write_all(b"\n").expect("terminate barrier ack");
        file.sync_all().expect("sync barrier ack");
        std::fs::File::open(path.parent().expect("ack parent"))
            .expect("open parent")
            .sync_all()
            .expect("sync parent");
        // SAFETY: only the current explicitly feature-enabled process is stopped.
        // Harness must verify this marker and process ownership before SIGKILL.
        let result = unsafe { libc::raise(libc::SIGSTOP) };
        panic!("crash barrier resumed unexpectedly ({result})");
    })
    .await?;
    Ok(())
}

/// 外部场景的单次调度窗口；只在显式故障产物编译，不暂停整个后台。
pub(crate) async fn schedule(name: &str, marker: serde_json::Value) {
    use tokio::io::AsyncWriteExt;
    let Some(root) = std::env::var_os("LATTICE_TEST_SCHEDULE_DIR") else {
        return;
    };
    let root = PathBuf::from(root);
    assert!(root.is_absolute(), "schedule directory must be absolute");
    if !tokio::fs::try_exists(root.join(format!("{name}.arm")))
        .await
        .expect("read schedule arm")
    {
        return;
    }
    let mut ready = match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join(format!("{name}.ready")))
        .await
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return,
        Err(error) => panic!("claim schedule gate: {error}"),
    };
    ready
        .write_all(&serde_json::to_vec(&marker).expect("encode schedule marker"))
        .await
        .expect("write schedule marker");
    ready.sync_all().await.expect("sync schedule marker");
    let release = root.join(format!("{name}.release"));
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !tokio::fs::try_exists(&release)
            .await
            .expect("read schedule release")
        {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("schedule gate was not released");
}
