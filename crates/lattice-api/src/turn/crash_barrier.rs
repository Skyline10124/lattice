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
    turn_id: TurnId,
    step: Option<u32>,
    part: Option<u32>,
    invocation_id: Option<InvocationId>,
    change_seq: ChangeSeq,
}
fn selected(config: &str, name: &str, slot: Option<(u32, u32)>) -> bool {
    config == name || slot.is_some_and(|(step, part)| config == format!("{name}:{step}:{part}"))
}
pub(crate) async fn hit(
    io: &CoordinatorStoreIo,
    turn_id: TurnId,
    barrier: &'static str,
    step: Option<u32>,
    binding: Option<&InvocationBinding>,
) -> Result<(), CoordinatorIoError> {
    let slot = binding.map(|binding| {
        (
            binding.identity.model_step_ordinal,
            binding.identity.part_ordinal,
        )
    });
    if !std::env::var("LATTICE_TEST_TURN_BARRIER")
        .ok()
        .is_some_and(|config| selected(&config, barrier, slot))
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
        turn_id,
        step,
        part: binding.map(|b| b.identity.part_ordinal),
        invocation_id: binding.map(|b| b.invocation_id),
        change_seq,
    };
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

#[cfg(test)]
mod tests {
    #[test]
    fn selector_can_pin_started_slot_without_stopping_earlier_calls() {
        assert!(super::selected(
            "invocation_started",
            "invocation_started",
            Some((1, 3))
        ));
        assert!(super::selected(
            "invocation_started:1:3",
            "invocation_started",
            Some((1, 3))
        ));
        assert!(!super::selected(
            "invocation_started:1:3",
            "invocation_started",
            Some((0, 3))
        ));
        assert!(!super::selected(
            "invocation_started:1:3",
            "invocation_started",
            Some((1, 2))
        ));
        assert!(!super::selected(
            "invocation_started:1:3",
            "calls_registered",
            Some((1, 3))
        ));
        assert!(!super::selected(
            "invocation_started:1:3",
            "invocation_started",
            None
        ));
    }
}
