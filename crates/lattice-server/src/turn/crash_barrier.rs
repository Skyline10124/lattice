// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 显式故障产物的提交后停点；marker 仅记录会话、Step 与调用槽位自身标识，不复制正文。
use super::coordinator::{CoordinatorIoError, CoordinatorStoreIo};
use lattice_conversation::{InvocationId, SessionId, TurnId};
use lattice_session::store::InvocationBinding;
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
    let marker = Marker {
        barrier,
        session_id: io.session_id(),
        turn_id: Some(turn_id),
        current: None,
        step,
        part: binding.map(|b| b.identity.part_ordinal),
        invocation_id: binding.map(|b| b.invocation_id),
    };
    stop(marker).await
}

pub(crate) async fn rewind_hit(
    session: &lattice_session::store::SessionRecord,
) -> Result<(), CoordinatorIoError> {
    if std::env::var("LATTICE_TEST_TURN_BARRIER").as_deref() != Ok("rewind_committed") {
        return Ok(());
    }
    stop(Marker {
        barrier: "rewind_committed",
        session_id: session.session_id,
        turn_id: None,
        current: Some(session.current),
        step: None,
        part: None,
        invocation_id: None,
    })
    .await
}

pub(crate) async fn configure_hit(
    session: &lattice_session::store::SessionRecord,
) -> Result<(), CoordinatorIoError> {
    if std::env::var("LATTICE_TEST_TURN_BARRIER").as_deref() != Ok("configure_committed") {
        return Ok(());
    }
    stop(Marker {
        barrier: "configure_committed",
        session_id: session.session_id,
        turn_id: None,
        current: Some(session.current),
        step: None,
        part: None,
        invocation_id: None,
    })
    .await
}

async fn stop(marker: Marker) -> Result<(), CoordinatorIoError> {
    tokio::task::spawn_blocking(move || {
        let path = PathBuf::from(
            std::env::var_os("LATTICE_TEST_TURN_BARRIER_ACK").expect("缺少屏障确认文件路径"),
        );
        assert!(path.is_absolute(), "屏障确认文件路径必须是绝对路径");
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .expect("创建屏障确认文件失败");
        serde_json::to_writer(&mut file, &marker).expect("编码屏障确认失败");
        file.write_all(b"\n").expect("写入屏障确认结尾换行失败");
        file.sync_all().expect("同步屏障确认文件失败");
        std::fs::File::open(path.parent().expect("屏障确认文件缺少父目录"))
            .expect("打开屏障确认文件的父目录失败")
            .sync_all()
            .expect("同步屏障确认文件的父目录失败");
        // 向调用线程自身发送 SIGSTOP：信号挂在本线程上，系统调用返回前整个进程即停住，
        // 本线程不会再执行后面的语句。发给整个进程的信号可能先由别的线程处理，本线程会在停住前继续运行。
        // harness 在发送 SIGKILL 前必须核对 marker 与进程归属。
        // SAFETY: `raise` 只向调用线程发送信号，不访问内存；只在显式启用测试屏障的产物中编译。
        let result = unsafe { libc::raise(libc::SIGSTOP) };
        panic!("崩溃屏障停住后意外恢复执行（raise 返回 {result}）");
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
    assert!(root.is_absolute(), "调度目录必须是绝对路径");
    if !tokio::fs::try_exists(root.join(format!("{name}.arm")))
        .await
        .expect("读取调度开关文件失败")
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
        Err(error) => panic!("占用调度窗口失败：{error}"),
    };
    ready
        .write_all(&serde_json::to_vec(&marker).expect("编码调度标记失败"))
        .await
        .expect("写入调度标记失败");
    ready.sync_all().await.expect("同步调度标记失败");
    let release = root.join(format!("{name}.release"));
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !tokio::fs::try_exists(&release)
            .await
            .expect("读取调度放行文件失败")
        {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("调度窗口在 30 秒内没有放行");
}
