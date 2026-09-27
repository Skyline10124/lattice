// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::types::{LocalError, LocalErrorKind, Result};
use std::{
    fs::{self, File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            net::UnixListener,
        },
    },
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"LTLOCAL3";
pub(super) const MAX_FRAME: usize = 16 * 1024 * 1024;
pub(super) const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

fn permission() -> LocalError {
    LocalError::new(
        LocalErrorKind::Permission,
        "local endpoint ownership or permissions rejected",
    )
}
fn protocol() -> LocalError {
    LocalError::new(
        LocalErrorKind::Protocol,
        "unsupported or malformed local protocol frame",
    )
}

pub(super) fn uid() -> u32 {
    // Linux geteuid 无参数、无内存访问；通信两端必须匹配实际有效 UID。
    unsafe { libc::geteuid() }
}

fn check_dir(path: &Path, private: bool) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir()
        || meta.uid() != uid()
        || meta.mode() & if private { 0o077 } else { 0o022 } != 0
    {
        return Err(permission());
    }
    Ok(())
}

/// 不创建运行资源的客户端寻址；使用规范路径，避免同一 STATE_ROOT 的锁别名。
pub(super) fn endpoint(layout: &crate::StateLayout) -> Result<PathBuf> {
    if !layout.lattice_root().is_absolute() {
        return Err(permission());
    }
    let root = fs::canonicalize(layout.lattice_root())?;
    check_dir(&root, false)?;
    let directory = root.join("local");
    check_dir(&directory, true)?;
    Ok(directory.join("api.sock"))
}

pub(super) struct Instance {
    _lock: File,
    socket: PathBuf,
    identity: (u64, u64),
}

impl Instance {
    pub(super) fn bind(layout: &crate::StateLayout) -> Result<(Self, UnixListener)> {
        if !layout.lattice_root().is_absolute() {
            return Err(permission());
        }
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(layout.lattice_root())?;
        let root = fs::canonicalize(layout.lattice_root())?;
        check_dir(&root, false)?;
        let directory = root.join("local");
        match fs::DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        check_dir(&directory, true)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(directory.join("owner.lock"))?;
        let meta = lock.metadata()?;
        if meta.uid() != uid() {
            return Err(permission());
        }
        // 持有文件描述符直至 serve 排空结束；不按 PID、mtime 或客户端连接数释放或抢占。
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = std::io::Error::last_os_error();
            return Err(if error.kind() == std::io::ErrorKind::WouldBlock {
                LocalError::new(
                    LocalErrorKind::AlreadyRunning,
                    "local backend already owns STATE_ROOT",
                )
            } else {
                error.into()
            });
        }
        let socket = directory.join("api.sock");
        remove_stale_socket(&socket)?;
        let staging = directory.join("bind.sock");
        remove_stale_socket(&staging)?;
        let listener = UnixListener::bind(&staging)?;
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        // 发布前设好权限，首次自启客户端不会把 chmod 窗口误判为不安全后台。
        fs::rename(&staging, &socket)?;
        let meta = fs::symlink_metadata(&socket)?;
        Ok((
            Self {
                _lock: lock,
                socket,
                identity: (meta.dev(), meta.ino()),
            },
            listener,
        ))
    }

    pub(super) fn cleanup(self) -> Result<()> {
        let meta = fs::symlink_metadata(&self.socket)?;
        if meta.uid() != uid() {
            return Err(permission());
        }
        if (meta.dev(), meta.ino()) != self.identity {
            return Err(permission());
        }
        fs::remove_file(&self.socket)?;
        Ok(())
    }
}

// 仅在持有实例锁时调用；失效名称仍须是本用户的 socket，不能按文件名删除任意对象。
fn remove_stale_socket(socket: &Path) -> Result<()> {
    let meta = match fs::symlink_metadata(socket) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !meta.file_type().is_socket() || meta.uid() != uid() {
        return Err(permission());
    }
    match std::os::unix::net::UnixStream::connect(socket) {
        Ok(_) => {
            return Err(LocalError::new(
                LocalErrorKind::Conflict,
                "live socket exists without matching owner lock",
            ));
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) => {}
        Err(error) => return Err(error.into()),
    }
    let current = fs::symlink_metadata(socket)?;
    if (current.dev(), current.ino()) != (meta.dev(), meta.ino()) {
        return Err(permission());
    }
    fs::remove_file(socket)?;
    Ok(())
}

fn check_socket(meta: &fs::Metadata) -> Result<()> {
    if meta.uid() != uid() {
        return Err(permission());
    }
    Ok(())
}

pub(super) fn check_peer(stream: &UnixStream) -> Result<()> {
    if stream.peer_cred()?.uid() != uid() {
        return Err(permission());
    }
    Ok(())
}

pub(super) async fn connect(layout: crate::StateLayout) -> Result<UnixStream> {
    let path = tokio::task::spawn_blocking(move || {
        let path = endpoint(&layout)?;
        check_socket(&fs::symlink_metadata(&path)?)?;
        Ok::<_, LocalError>(path)
    })
    .await
    .map_err(|_| LocalError::new(LocalErrorKind::Internal, "endpoint worker failed"))??;
    let stream = UnixStream::connect(path).await?;
    check_peer(&stream)?;
    Ok(stream)
}

pub(super) async fn read<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> Result<T> {
    let mut magic = [0; 8];
    stream.read_exact(&mut magic).await?;
    if &magic != MAGIC {
        return Err(protocol());
    }
    let len = stream.read_u32().await? as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(protocol());
    }
    let mut bytes = Zeroizing::new(vec![0; len]);
    stream.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(|_| protocol())
}

struct Frame(Zeroizing<Vec<u8>>);
impl std::io::Write for Frame {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_FRAME {
            return Err(std::io::Error::other("frame exceeds limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) async fn write<T: serde::Serialize>(stream: &mut UnixStream, value: &T) -> Result<()> {
    let mut frame = Frame(Zeroizing::new(Vec::new()));
    serde_json::to_writer(&mut frame, value).map_err(|_| protocol())?;
    tokio::time::timeout(WRITE_TIMEOUT, async {
        stream.write_all(MAGIC).await?;
        stream.write_u32(frame.0.len() as u32).await?;
        stream.write_all(&frame.0).await
    })
    .await
    .map_err(|_| {
        LocalError::new(
            LocalErrorKind::Transport,
            "local write timed out; acknowledgement may be unknown",
        )
    })??;
    Ok(())
}

/// 每个连接独立编号；片段的 offset 按逻辑 JSON 的 UTF-8 字节计量。
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseFrame {
    message_id: u64,
    offset: u64,
    json_fragment: String,
    complete: bool,
}

#[derive(Default)]
pub(super) struct ResponseWriter {
    next_message: u64,
}

impl ResponseWriter {
    pub(super) async fn write<T: serde::Serialize>(
        &mut self,
        stream: &mut UnixStream,
        response: &T,
    ) -> Result<()> {
        let next_message = self.next_message.checked_add(1).ok_or_else(protocol)?;
        // ponytail: 一次物化逻辑响应；实际出现内存压力时再改增量编码，不拒绝合法大记录。
        let json = serde_json::to_string(response).map_err(|_| protocol())?;
        let mut offset = 0;
        while offset < json.len() {
            let mut end = (offset + 65_536).min(json.len());
            while !json.is_char_boundary(end) {
                end -= 1;
            }
            let frame = ResponseFrame {
                message_id: self.next_message,
                offset: offset as u64,
                json_fragment: json[offset..end].to_owned(),
                complete: end == json.len(),
            };
            // write 对外层 JSON 实际编码计量，包含字符串二次转义。
            write(stream, &frame).await?;
            offset = end;
        }
        self.next_message = next_message;
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct ResponseReader {
    next_message: u64,
}

impl ResponseReader {
    /// 由连接的持续读取任务持有；调用方不能取消后在同一 socket 重启半条响应。
    pub(super) async fn read<T: serde::de::DeserializeOwned>(
        &mut self,
        stream: &mut UnixStream,
    ) -> Result<T> {
        self.read_with_progress(stream, || {}).await
    }

    /// 完整合法分片即为读取进度；不必等整个大对象解码完才刷新连接 idle。
    pub(super) async fn read_with_progress<T: serde::de::DeserializeOwned>(
        &mut self,
        stream: &mut UnixStream,
        mut progress: impl FnMut(),
    ) -> Result<T> {
        let next_message = self.next_message.checked_add(1).ok_or_else(protocol)?;
        let mut json = String::new();
        loop {
            let frame: ResponseFrame = read(stream).await?;
            if frame.message_id != self.next_message
                || frame.offset != json.len() as u64
                || frame.json_fragment.is_empty()
            {
                return Err(protocol());
            }
            json.push_str(&frame.json_fragment);
            progress();
            if frame.complete {
                let response = serde_json::from_str(&json).map_err(|_| protocol())?;
                self.next_message = next_message;
                return Ok(response);
            }
        }
    }
}

#[cfg(test)]
mod frame_tests {
    use super::*;

    #[tokio::test]
    async fn v3_accepts_current_frames_and_rejects_old_empty_and_oversized_frames() {
        for (magic, length, payload, valid) in [
            (*b"LTLOCAL3", 4_u32, b"null".as_slice(), true),
            (*b"LTLOCAL2", 4, b"null".as_slice(), false),
            (*b"LTLOCAL3", 0, b"".as_slice(), false),
            (*b"LTLOCAL3", (MAX_FRAME + 1) as u32, b"".as_slice(), false),
        ] {
            let (mut sender, mut receiver) = UnixStream::pair().unwrap();
            sender.write_all(&magic).await.unwrap();
            sender.write_u32(length).await.unwrap();
            sender.write_all(payload).await.unwrap();
            let result = read::<serde_json::Value>(&mut receiver).await;
            if valid {
                assert_eq!(result.unwrap(), serde_json::Value::Null);
            } else {
                assert_eq!(result.unwrap_err().kind, LocalErrorKind::Protocol);
            }
        }
    }

    #[tokio::test]
    async fn response_fragments_have_wire_byte_offsets_and_report_each_progress() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        let input = serde_json::json!({"body": "中\"\\\n".repeat(20_000)});
        let json = serde_json::to_string(&input).unwrap();
        let writer = tokio::spawn(async move {
            ResponseWriter::default()
                .write(&mut sender, &input)
                .await
                .unwrap();
        });
        let mut combined = String::new();
        let mut count = 0;
        loop {
            let frame: serde_json::Value = read(&mut receiver).await.unwrap();
            assert_eq!(frame["message_id"], 0);
            assert_eq!(frame["offset"], combined.len() as u64);
            assert!(serde_json::to_vec(&frame).unwrap().len() <= MAX_FRAME);
            let fragment = frame["json_fragment"].as_str().unwrap();
            assert!(!fragment.is_empty());
            combined.push_str(fragment);
            count += 1;
            if frame["complete"] == true {
                break;
            }
        }
        assert!(count > 1);
        assert_eq!(combined, json);
        writer.await.unwrap();

        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        let mut progress = 0;
        let reader = async {
            let value: String = ResponseReader::default()
                .read_with_progress(&mut receiver, || progress += 1)
                .await
                .unwrap();
            assert_eq!(value, "中");
        };
        let writer = async {
            for frame in [
                serde_json::json!({"message_id":0,"offset":0,"json_fragment":"\"中","complete":false}),
                serde_json::json!({"message_id":0,"offset":4,"json_fragment":"\"","complete":true}),
            ] {
                write(&mut sender, &frame).await.unwrap();
            }
        };
        tokio::join!(reader, writer);
        assert_eq!(progress, 2);
    }

    #[tokio::test]
    async fn response_round_trip_preserves_large_escaped_unicode_and_message_order() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        // 二次 JSON 转义会显著膨胀，逻辑对象本身也超过单帧上限。
        let text = "中\"\\\n".repeat(MAX_FRAME / 7 + 1);
        let expected = serde_json::json!({"body": text});
        let sent = expected.clone();
        let writer = tokio::spawn(async move {
            let mut responses = ResponseWriter::default();
            responses.write(&mut sender, &sent).await.unwrap();
            responses
                .write(&mut sender, &serde_json::json!({"next": 1}))
                .await
                .unwrap();
        });
        let mut responses = ResponseReader::default();
        let received: serde_json::Value = responses.read(&mut receiver).await.unwrap();
        assert_eq!(received, expected);
        let next: serde_json::Value = responses.read(&mut receiver).await.unwrap();
        assert_eq!(next, serde_json::json!({"next": 1}));
        writer.await.unwrap();
    }

    #[tokio::test]
    async fn response_rejects_noncontiguous_fragments_and_incomplete_eof() {
        for (id, offset, fragment, complete, expected_kind) in [
            (1, 0, "null", true, LocalErrorKind::Protocol),
            (0, 1, "null", true, LocalErrorKind::Protocol),
            (0, 0, "", false, LocalErrorKind::Protocol),
            (0, 0, "{", false, LocalErrorKind::Transport),
            (0, 0, "{", true, LocalErrorKind::Protocol),
        ] {
            let (mut sender, mut receiver) = UnixStream::pair().unwrap();
            write(
                &mut sender,
                &serde_json::json!({
                    "message_id": id, "offset": offset,
                    "json_fragment": fragment, "complete": complete
                }),
            )
            .await
            .unwrap();
            drop(sender);
            let error = ResponseReader::default()
                .read::<serde_json::Value>(&mut receiver)
                .await
                .unwrap_err();
            assert_eq!(error.kind, expected_kind);
        }
        for (id, offset) in [(1, 1), (0, 0), (0, 2)] {
            let (mut sender, mut receiver) = UnixStream::pair().unwrap();
            for frame in [
                serde_json::json!({"message_id":0,"offset":0,"json_fragment":"[","complete":false}),
                serde_json::json!({"message_id":id,"offset":offset,"json_fragment":"]","complete":true}),
            ] {
                write(&mut sender, &frame).await.unwrap();
            }
            let error = ResponseReader::default()
                .read::<serde_json::Value>(&mut receiver)
                .await
                .unwrap_err();
            assert_eq!(error.kind, LocalErrorKind::Protocol);
        }
    }
}
