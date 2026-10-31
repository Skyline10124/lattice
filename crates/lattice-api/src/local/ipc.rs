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

pub(super) const MAGIC: &[u8; 8] = b"LTLOCAL;";
pub(super) const MAX_FRAME: usize = 16 * 1024 * 1024;
pub(super) const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const INCOMPATIBLE_BACKEND: &str = "the running backend is an incompatible older version; stop it with the older binary's `lattice daemon stop`";
const NEWER_BACKEND: &str = "the running backend is newer than this client; use the newer binary";
const SILENT_CLOSE: &str = "the backend closed the connection without replying; if an older backend is still running, stop it with the older binary's `lattice daemon stop`";

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

/// 魔数格式固定为 `LTLOCAL` 加一个版本字节，按字节值比较；`9` 之后继续用 `:`、`;` 等更大的字节，
/// 不改帧长度。
fn magic_version(magic: &[u8; 8]) -> Option<u8> {
    let (prefix, _) = MAGIC.split_at(MAGIC.len() - 1);
    magic.starts_with(prefix).then_some(magic[prefix.len()])
}

fn check_magic(magic: &[u8; 8]) -> Result<()> {
    let current = MAGIC[MAGIC.len() - 1];
    match magic_version(magic) {
        Some(version) if version == current => Ok(()),
        Some(version) if version < current => Err(LocalError::new(
            LocalErrorKind::IncompatibleBackend,
            INCOMPATIBLE_BACKEND,
        )),
        Some(_) => Err(LocalError::new(
            LocalErrorKind::IncompatibleBackend,
            NEWER_BACKEND,
        )),
        None => Err(protocol()),
    }
}

// 这里仍用 libc::geteuid：lattice-api 的 flock、recv、setsid 等也直接用 libc，
// rustix 在本 crate 只是 dev-dependency；只换这一处没有意义，也不为此新增常规依赖。
// 整体改用 rustix 留到之后拆分 lattice-api 时再做。
pub(crate) fn uid() -> u32 {
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

    /// 只删除本实例发布的 socket；实例锁随 Instance 释放，调用方决定释放的时机。
    pub(super) fn cleanup(&self) -> Result<()> {
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

/// stop 回执只说明后台已排空；回执之后它还要关闭 listener、删除 socket 并释放实例锁。
/// 停止方在回执之后等实例锁可取得，再交还给调用方；不等待后台进程退出。
pub(super) struct Departure {
    layout: crate::StateLayout,
}

impl Departure {
    pub(super) fn new(layout: crate::StateLayout) -> Self {
        Self { layout }
    }

    /// 在 `timeout` 内等旧后台释放实例锁；超时报 Stopping，后台可能仍在收尾。
    pub(super) async fn wait(self, timeout: Duration) -> Result<()> {
        // 用真实时钟计上限：测试运行时的暂停时钟不应让等待提前判为超时。
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if self.released().await? {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(LocalError::new(
                    LocalErrorKind::Stopping,
                    "the backend acknowledged stop but has not released STATE_ROOT in time; check again with `lattice daemon status`",
                ));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// 以非阻塞共享 flock 试探实例锁并立即放开，不创建锁文件。
    async fn released(&self) -> Result<bool> {
        let layout = self.layout.clone();
        tokio::task::spawn_blocking(move || {
            let lock = endpoint(&layout)?.with_file_name("owner.lock");
            let file = match OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(lock)
            {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
                Err(error) => return Err(error.into()),
            };
            // SAFETY: flock 只作用于本函数打开的 fd；关闭 fd 即释放试探取得的锁。
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
                return Ok(true);
            }
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::WouldBlock {
                Ok(false)
            } else {
                Err(error.into())
            }
        })
        .await
        .map_err(|_| LocalError::new(LocalErrorKind::Internal, "endpoint worker failed"))?
    }
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

/// 测试替身读整帧；后台读请求走 `read_request`。
#[cfg(test)]
pub(super) async fn read<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> Result<T> {
    read_frame(stream, false).await?.ok_or_else(eof)
}

/// 后台读请求专用。魔数不符时先回写一帧本端魔数再断开，让对端客户端能分辨是哪一边版本旧。
pub(super) async fn read_request<T: serde::de::DeserializeOwned>(
    stream: &mut UnixStream,
) -> Result<T> {
    let mut magic = [0; 8];
    if stream.read(&mut magic[..1]).await? == 0 {
        return Err(eof());
    }
    stream.read_exact(&mut magic[1..]).await?;
    if let Err(error) = check_magic(&magic) {
        let _ = tokio::time::timeout(WRITE_TIMEOUT, async {
            stream.write_all(MAGIC).await?;
            stream.write_u32(4).await?;
            stream.write_all(b"null").await
        })
        .await;
        return Err(error);
    }
    read_body(stream).await
}
fn eof() -> LocalError {
    LocalError::new(
        LocalErrorKind::Transport,
        "local connection ended before the message was complete",
    )
}
/// 不回写就关连接是本改动之前的后台读到新魔数时的行为；当前后台在过载、崩溃时也可能这样，
/// 所以只作提示，种类仍是 Transport。
fn silent_close() -> LocalError {
    LocalError::new(LocalErrorKind::Transport, SILENT_CLOSE)
}
/// `reset_is_eof`：对端带着未读完的请求关闭 Unix 套接字时，Linux 让这一端读到 ECONNRESET 而不是 EOF。
async fn read_frame<T: serde::de::DeserializeOwned>(
    stream: &mut UnixStream,
    reset_is_eof: bool,
) -> Result<Option<T>> {
    let mut magic = [0; 8];
    match stream.read(&mut magic[..1]).await {
        Ok(0) => return Ok(None),
        Ok(_) => {}
        Err(error) if reset_is_eof && error.kind() == std::io::ErrorKind::ConnectionReset => {
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    }
    stream.read_exact(&mut magic[1..]).await?;
    check_magic(&magic)?;
    read_body(stream).await.map(Some)
}
async fn read_body<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> Result<T> {
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
    Ok(write_wire(stream, value).await??)
}

/// 客户端写请求专用。请求大于套接字缓冲区时，旧后台读完魔数就关连接，这一端写到一半会碰上
/// EPIPE 或 ECONNRESET；先读一眼对端在断开前有没有回写自己的魔数，没有就按疑似旧后台提示。
pub(super) async fn write_request<T: serde::Serialize>(
    stream: &mut UnixStream,
    value: &T,
) -> Result<()> {
    match write_wire(stream, value).await? {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
            ) =>
        {
            match tokio::time::timeout(WRITE_TIMEOUT, read_frame::<serde_json::Value>(stream, true))
                .await
            {
                Ok(Err(error)) if error.kind == LocalErrorKind::IncompatibleBackend => Err(error),
                _ => Err(silent_close()),
            }
        }
        Err(error) => Err(error.into()),
    }
}

/// 外层是编码和超时失败，内层保留 io 错误种类给 `write_request` 分辨对端已关闭。
async fn write_wire<T: serde::Serialize>(
    stream: &mut UnixStream,
    value: &T,
) -> Result<std::io::Result<()>> {
    let mut frame = Frame(Zeroizing::new(Vec::new()));
    serde_json::to_writer(&mut frame, value).map_err(|_| protocol())?;
    // 魔数、长度和正文一次写出：小请求整帧进入对端接收队列，旧后台只读魔数就关闭时，
    // 这一端看到的是读不到回复，而不是写到一半的 EPIPE。
    let mut wire = Zeroizing::new(Vec::with_capacity(12 + frame.0.len()));
    wire.extend_from_slice(MAGIC);
    wire.extend_from_slice(&(frame.0.len() as u32).to_be_bytes());
    wire.extend_from_slice(&frame.0);
    tokio::time::timeout(WRITE_TIMEOUT, stream.write_all(&wire))
        .await
        .map_err(|_| {
            LocalError::new(
                LocalErrorKind::Transport,
                "local write timed out; acknowledgement may be unknown",
            )
        })
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
        progress: impl FnMut(),
    ) -> Result<T> {
        let first = self.next_message == 0;
        self.next_with_progress(stream, progress)
            .await?
            .ok_or_else(|| if first { silent_close() } else { eof() })
    }
    /// 只有完整消息之间的 EOF 是正常结束，半帧或半条消息仍是传输失败。
    pub(super) async fn next_with_progress<T: serde::de::DeserializeOwned>(
        &mut self,
        stream: &mut UnixStream,
        mut progress: impl FnMut(),
    ) -> Result<Option<T>> {
        let next_message = self.next_message.checked_add(1).ok_or_else(protocol)?;
        let mut json = String::new();
        loop {
            let nothing_received = self.next_message == 0 && json.is_empty();
            let Some(frame) = read_frame::<ResponseFrame>(stream, nothing_received).await? else {
                return if json.is_empty() {
                    Ok(None)
                } else {
                    Err(eof())
                };
            };
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
                return Ok(Some(response));
            }
        }
    }
}

#[cfg(test)]
mod frame_tests {
    use super::*;

    #[tokio::test]
    async fn current_frames_reject_old_empty_and_oversized_frames() {
        for (magic, length, payload, expected) in [
            (*b"LTLOCAL;", 4_u32, b"null".as_slice(), None),
            (
                *b"LTLOCAL:",
                4,
                b"null".as_slice(),
                Some(LocalErrorKind::IncompatibleBackend),
            ),
            (
                *b"LTLOCAL9",
                4,
                b"null".as_slice(),
                Some(LocalErrorKind::IncompatibleBackend),
            ),
            (
                *b"LTLOCAL8",
                4,
                b"null".as_slice(),
                Some(LocalErrorKind::IncompatibleBackend),
            ),
            (
                *b"LTLOCAL7",
                4,
                b"null".as_slice(),
                Some(LocalErrorKind::IncompatibleBackend),
            ),
            (
                *b"LTLOCAL6",
                4,
                b"null".as_slice(),
                Some(LocalErrorKind::IncompatibleBackend),
            ),
            (
                *b"LTLOCAL2",
                4,
                b"null".as_slice(),
                Some(LocalErrorKind::IncompatibleBackend),
            ),
            (
                *b"LTLOCAL<",
                4,
                b"null".as_slice(),
                Some(LocalErrorKind::IncompatibleBackend),
            ),
            (
                *b"XXLOCAL9",
                4,
                b"null".as_slice(),
                Some(LocalErrorKind::Protocol),
            ),
            (
                *b"LTLOCAL;",
                0,
                b"".as_slice(),
                Some(LocalErrorKind::Protocol),
            ),
            (
                *b"LTLOCAL;",
                (MAX_FRAME + 1) as u32,
                b"".as_slice(),
                Some(LocalErrorKind::Protocol),
            ),
        ] {
            tokio::time::timeout(Duration::from_secs(1), async {
                let (mut sender, mut receiver) = UnixStream::pair().unwrap();
                sender.write_all(&magic).await.unwrap();
                sender.write_u32(length).await.unwrap();
                sender.write_all(payload).await.unwrap();
                let result = read::<serde_json::Value>(&mut receiver).await;
                match expected {
                    None => assert_eq!(result.unwrap(), serde_json::Value::Null),
                    Some(kind) => {
                        let error = result.unwrap_err();
                        assert_eq!(error.kind, kind, "{}", String::from_utf8_lossy(&magic));
                        if kind == LocalErrorKind::IncompatibleBackend {
                            let expected = if magic > *MAGIC {
                                NEWER_BACKEND
                            } else {
                                INCOMPATIBLE_BACKEND
                            };
                            assert_eq!(error.message, expected);
                        }
                    }
                }
            })
            .await
            .expect("IPC frame case timed out");
        }
    }

    #[tokio::test]
    async fn backend_answers_a_foreign_magic_with_its_own_before_closing() {
        tokio::time::timeout(Duration::from_secs(1), async {
            let (mut client, mut backend) = UnixStream::pair().unwrap();
            // 字面量模拟下一版客户端，不能从当前魔数推导。
            client.write_all(b"LTLOCAL<").await.unwrap();
            client.write_u32(4).await.unwrap();
            client.write_all(b"null").await.unwrap();
            let error = read_request::<serde_json::Value>(&mut backend)
                .await
                .unwrap_err();
            assert_eq!(error.kind, LocalErrorKind::IncompatibleBackend);
            drop(backend);
            let mut reply = [0; 8];
            client.read_exact(&mut reply).await.unwrap();
            assert_eq!(&reply, MAGIC, "backend must announce its own magic");
            let len = client.read_u32().await.unwrap() as usize;
            client.read_exact(&mut vec![0; len]).await.unwrap();
            // 后台没读完的请求留在它的接收队列里，Linux 让这一端读到 ECONNRESET；两者都表示已关闭。
            let closed = client.read(&mut [0; 1]).await;
            assert!(
                matches!(&closed, Ok(0))
                    || matches!(&closed, Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset),
                "connection must close after the announcement: {closed:?}"
            );
        })
        .await
        .expect("foreign IPC magic case timed out");
    }

    /// harness 不能依赖产品 crate，只能写死魔数；升级时在这里失败并指出要改哪个文件。
    #[test]
    fn harness_frame_magic_tracks_the_current_magic() {
        let harness = include_str!("../../../lattice-system-harness/tests/support/mod.rs");
        let current = std::str::from_utf8(MAGIC).unwrap();
        assert!(
            harness.contains(&format!("const FRAME_MAGIC: &[u8; 8] = b\"{current}\";")),
            "update FRAME_MAGIC in crates/lattice-system-harness/tests/support/mod.rs to {current}"
        );
    }

    /// 远大于套接字缓冲区的请求，对端只读魔数就关闭。
    async fn oversized_request_against(reply: Option<&'static [u8; 8]>) -> LocalError {
        let (mut client, mut backend) = UnixStream::pair().unwrap();
        let backend = tokio::spawn(async move {
            let mut magic = [0; 8];
            backend.read_exact(&mut magic).await.unwrap();
            if let Some(reply) = reply {
                backend.write_all(reply).await.unwrap();
                backend.write_u32(4).await.unwrap();
                backend.write_all(b"null").await.unwrap();
            }
        });
        let prompt = "x".repeat(4 * 1024 * 1024);
        let error = write_request(&mut client, &prompt).await.unwrap_err();
        backend.await.unwrap();
        error
    }

    #[tokio::test]
    async fn oversized_request_cut_off_by_a_silent_older_backend_gets_the_stop_hint() {
        let error = oversized_request_against(None).await;
        assert_eq!(
            (error.kind, error.message.as_str()),
            (LocalErrorKind::Transport, SILENT_CLOSE)
        );
    }

    #[tokio::test]
    async fn oversized_request_cut_off_by_an_announcing_backend_reports_its_version() {
        // 字面量模拟旧后台，不能从当前魔数推导。
        let error = oversized_request_against(Some(b"LTLOCAL5")).await;
        assert_eq!(
            (error.kind, error.message.as_str()),
            (LocalErrorKind::IncompatibleBackend, INCOMPATIBLE_BACKEND)
        );
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
