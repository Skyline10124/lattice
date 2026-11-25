// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 本地 IPC 协议版本、16 MiB 单帧编解码、响应分片与本地访问边界核验。

use std::{
    os::fd::{AsFd, BorrowedFd},
    os::unix::fs::MetadataExt,
    path::Path,
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{UnixStream, unix::OwnedReadHalf},
};
use zeroize::{Zeroize, Zeroizing};

use crate::types::{
    LocalError, LocalErrorKind, PlannedContextOp, PlannedContextOpRequest, PlannedFeed,
    PlannedFeedRequest, PlannedSubmit, PlannedTurnRequest, Result, UserInputPart,
};

mod defs {
    pub const PROTOCOL_VERSION: u8 = 68;
    pub const MAGIC: &[u8; 8] = &magic_for_version(PROTOCOL_VERSION);
    /// 帧魔数固定为 7 字节前缀 `LTLOCAL` 加 1 个无符号协议版本字节（`0..=255`），帧长度不变。
    pub const fn magic_for_version(version: u8) -> [u8; 8] {
        let mut magic = *b"LTLOCAL\0";
        magic[7] = version;
        magic
    }
    pub const MAX_FRAME: usize = 16 * 1024 * 1024;
}

pub use defs::MAX_FRAME;
#[cfg(not(any(test, feature = "test-utils")))]
use defs::{MAGIC, PROTOCOL_VERSION, magic_for_version};
#[cfg(any(test, feature = "test-utils"))]
pub use defs::{MAGIC, PROTOCOL_VERSION, magic_for_version};

const LOGIN_FRAME_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const SILENT_CLOSE: &str = "后台未回写响应即关闭了连接；若仍有较旧版本的后台在运行，请用旧二进制执行 `lattice daemon stop` 停止它";

/// 携带底层套接字描述符的异步读取流，供版本不兼容诊断在持有连接期间读取对端进程号。
pub trait PeerStream: AsyncRead + Unpin {
    fn peer_fd(&self) -> Option<BorrowedFd<'_>>;
}

impl PeerStream for UnixStream {
    fn peer_fd(&self) -> Option<BorrowedFd<'_>> {
        Some(self.as_fd())
    }
}

impl PeerStream for OwnedReadHalf {
    fn peer_fd(&self) -> Option<BorrowedFd<'_>> {
        Some(self.as_ref().as_fd())
    }
}

/// 经 `SO_PEERCRED` 读取已连接本地套接字的对端进程号，仅用于诊断展示。
pub fn peer_pid(fd: impl AsFd) -> Option<i32> {
    rustix::net::sockopt::socket_peercred(fd)
        .ok()
        .map(|cred| cred.pid.as_raw_pid())
}

fn format_peer_pid(peer: Option<BorrowedFd<'_>>) -> String {
    match peer.and_then(peer_pid) {
        Some(pid) => format!("对端进程号：{pid}"),
        None => "对端进程号不可用".into(),
    }
}

fn older_backend_message(peer: Option<BorrowedFd<'_>>) -> String {
    format!(
        "运行中的后台版本较旧且不兼容（{}）；请用旧二进制执行 `lattice daemon stop` 停止它",
        format_peer_pid(peer)
    )
}

fn newer_backend_message(peer: Option<BorrowedFd<'_>>) -> String {
    format!(
        "运行中的后台版本较新且不兼容（{}）；请换用匹配的前端",
        format_peer_pid(peer)
    )
}

pub fn permission() -> LocalError {
    LocalError::new(LocalErrorKind::Permission, "本地端点属主或权限校验未通过")
}

fn protocol() -> LocalError {
    LocalError::new(LocalErrorKind::Protocol, "不支持或格式错误的本地协议帧")
}

/// 前缀相符时取出版本字节；新旧由 `check_magic` 按版本数值比较。
fn magic_version(magic: &[u8; 8]) -> Option<u8> {
    let version = magic[7];
    (*magic == magic_for_version(version)).then_some(version)
}

fn check_magic(magic: &[u8; 8], peer: Option<BorrowedFd<'_>>) -> Result<()> {
    match magic_version(magic) {
        Some(version) if version == PROTOCOL_VERSION => Ok(()),
        Some(version) if version < PROTOCOL_VERSION => Err(LocalError::new(
            LocalErrorKind::IncompatibleBackend,
            &older_backend_message(peer),
        )),
        Some(_) => Err(LocalError::new(
            LocalErrorKind::IncompatibleBackend,
            &newer_backend_message(peer),
        )),
        None => Err(protocol()),
    }
}

pub fn uid() -> u32 {
    // Linux geteuid 无参数、无内存访问；通信两端必须匹配实际有效 UID。
    rustix::process::geteuid().as_raw()
}

pub fn check_peer(stream: &UnixStream) -> Result<()> {
    let cred = rustix::net::sockopt::socket_peercred(stream).map_err(|errno| {
        let error = std::io::Error::from(errno);
        let kind = if error.kind() == std::io::ErrorKind::PermissionDenied {
            LocalErrorKind::Permission
        } else {
            LocalErrorKind::Transport
        };
        LocalError::new(kind, &format!("读取本地连接对端凭据失败：{error}"))
    })?;
    let current_uid = uid();
    if cred.uid.as_raw() != current_uid {
        return Err(LocalError::new(
            LocalErrorKind::Permission,
            &format!(
                "本地连接对端用户不匹配（对端 UID：{}，当前有效 UID：{current_uid}）",
                cred.uid.as_raw()
            ),
        ));
    }
    Ok(())
}

/// 核验状态根或 `local/` 目录类型、有效 UID 属主与权限位。
pub fn check_dir(path: &Path, private: bool) -> Result<()> {
    let meta = std::fs::symlink_metadata(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => LocalError::new(
            LocalErrorKind::NotRunning,
            &format!("本地状态目录不存在：{}：{error}", path.display()),
        ),
        std::io::ErrorKind::PermissionDenied => LocalError::new(
            LocalErrorKind::Permission,
            &format!(
                "读取本地状态目录元数据权限不足：{}：{error}",
                path.display()
            ),
        ),
        std::io::ErrorKind::NotADirectory => LocalError::new(
            LocalErrorKind::Permission,
            &format!("本地状态目录路径不是目录：{}：{error}", path.display()),
        ),
        _ => LocalError::new(
            LocalErrorKind::Transport,
            &format!("读取本地状态目录元数据失败：{}：{error}", path.display()),
        ),
    })?;
    if !meta.is_dir() {
        return Err(LocalError::new(
            LocalErrorKind::Permission,
            &format!("本地端点目录不是真实目录：{}", path.display()),
        ));
    }
    let current_uid = uid();
    if meta.uid() != current_uid {
        return Err(LocalError::new(
            LocalErrorKind::Permission,
            &format!(
                "本地端点目录属主不匹配：{}（目录 UID：{}，当前有效 UID：{current_uid}）",
                path.display(),
                meta.uid()
            ),
        ));
    }
    let forbidden = if private { 0o077 } else { 0o022 };
    if meta.mode() & forbidden != 0 {
        let mode = meta.mode() & 0o777;
        let detail = if private {
            format!(
                "本地端点目录权限过宽（须仅当前用户可读写执行，如 0700，当前为 {mode:04o}）：{}",
                path.display()
            )
        } else {
            format!(
                "状态根目录权限过宽（不得允许同组或其他用户写入，当前为 {mode:04o}）：{}",
                path.display()
            )
        };
        return Err(LocalError::new(LocalErrorKind::Permission, &detail));
    }
    Ok(())
}

/// 测试替身读整帧；后台读请求走 `read_request`。
#[cfg(any(test, feature = "test-utils"))]
pub async fn read<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> Result<T> {
    read_frame(stream, false).await?.ok_or_else(eof)
}

/// 后台读请求专用。魔数不符时先回写一帧本端魔数再断开，让对端客户端能分辨是哪一边版本旧。
pub async fn read_request<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> Result<T> {
    let mut magic = [0; 8];
    if stream.read(&mut magic[..1]).await? == 0 {
        return Err(eof());
    }
    stream.read_exact(&mut magic[1..]).await?;
    if let Err(error) = check_magic(&magic, stream.peer_fd()) {
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
    LocalError::new(LocalErrorKind::Transport, "本地连接在消息完整接收前已断开")
}

/// 不回写就关连接是本改动之前的后台读到新魔数时的行为；当前后台在过载、崩溃时也可能这样，
/// 所以只作提示，种类仍是 Transport。
fn silent_close() -> LocalError {
    LocalError::new(LocalErrorKind::Transport, SILENT_CLOSE)
}

/// `reset_is_eof`：对端带着未读完的请求关闭 Unix 套接字时，Linux 让这一端读到 ECONNRESET 而不是 EOF。
async fn read_frame<T: serde::de::DeserializeOwned>(
    stream: &mut impl PeerStream,
    reset_is_eof: bool,
) -> Result<Option<T>> {
    read_frame_with_timeout(stream, reset_is_eof, None).await
}

pub async fn read_login_action<T: serde::de::DeserializeOwned>(
    stream: &mut impl PeerStream,
) -> Result<Option<T>> {
    read_frame_with_timeout(stream, false, Some(LOGIN_FRAME_TIMEOUT)).await
}

async fn read_frame_with_timeout<T: serde::de::DeserializeOwned>(
    stream: &mut impl PeerStream,
    reset_is_eof: bool,
    timeout: Option<Duration>,
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
    let remainder = async {
        stream.read_exact(&mut magic[1..]).await?;
        check_magic(&magic, stream.peer_fd())?;
        read_body(stream).await.map(Some)
    };
    match timeout {
        Some(timeout) => tokio::time::timeout(timeout, remainder)
            .await
            .map_err(|_| LocalError::new(LocalErrorKind::Transport, "读取登录帧超时"))?,
        None => remainder.await,
    }
}

async fn read_body<T: serde::de::DeserializeOwned>(
    stream: &mut (impl AsyncRead + Unpin),
) -> Result<T> {
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
            return Err(std::io::Error::other("帧大小超过上限"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub async fn write<T: serde::Serialize>(
    stream: &mut (impl AsyncWrite + Unpin),
    value: &T,
) -> Result<()> {
    Ok(write_wire(stream, value).await??)
}

/// 客户端写请求专用。请求大于套接字缓冲区时，旧后台读完魔数就关连接，这一端写到一半会碰上
/// EPIPE 或 ECONNRESET；先读一眼对端在断开前有没有回写自己的魔数，没有就按疑似旧后台提示。
pub async fn write_request<T: serde::Serialize>(stream: &mut UnixStream, value: &T) -> Result<()> {
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
    stream: &mut (impl AsyncWrite + Unpin),
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
        .map_err(|_| LocalError::new(LocalErrorKind::Transport, "本地写入超时；回执状态可能未知"))
}

struct ByteCounter(usize);

impl std::io::Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn encoded_parts_image_bytes(parts: &[UserInputPart<lattice_conversation::ImageRef>]) -> u64 {
    parts
        .iter()
        .map(|part| match part {
            UserInputPart::Image { image } => image.byte_length.div_ceil(3) * 4,
            UserInputPart::Text { .. } => 0,
        })
        .sum()
}

/// 实发 `Request::Submit` 的 JSON 正文字节数（不含 8 字节魔数与 4 字节长度头），与 `MAX_FRAME` 的口径一致。
/// 图片按标准 base64 计入 `byte_length.div_ceil(3) * 4` 字节加两个引号，不分配正文。
pub fn encoded_submit_len(plan: &PlannedTurnRequest) -> usize {
    let mut counter = ByteCounter(0);
    serde_json::to_writer(&mut counter, &PlannedSubmit::Submit { request: plan })
        .expect("PlannedSubmit 序列化到字节计数器不应失败");
    let images: u64 = plan
        .inputs
        .iter()
        .map(|item| encoded_parts_image_bytes(&item.parts))
        .sum();
    counter.0 + images as usize
}

/// 实发 `Request::Feed` 的 JSON 正文字节数（不含 8 字节魔数与 4 字节长度头），与 `MAX_FRAME` 的口径一致。
pub fn encoded_feed_len(plan: &PlannedFeedRequest) -> usize {
    let mut counter = ByteCounter(0);
    serde_json::to_writer(&mut counter, &PlannedFeed::Feed { request: plan })
        .expect("PlannedFeed 序列化到字节计数器不应失败");
    let images = encoded_parts_image_bytes(&plan.parts);
    counter.0 + images as usize
}

/// 实发 `Request::ContextOp` 的 JSON 正文字节数（不含 8 字节魔数与 4 字节长度头），与 `MAX_FRAME` 的口径一致。
pub fn encoded_context_op_len(plan: &PlannedContextOpRequest) -> usize {
    let mut counter = ByteCounter(0);
    serde_json::to_writer(&mut counter, &PlannedContextOp::ContextOp { request: plan })
        .expect("PlannedContextOp 序列化到字节计数器不应失败");
    let images: u64 = plan
        .images
        .iter()
        .map(|item| item.image.byte_length.div_ceil(3) * 4)
        .sum();
    counter.0 + images as usize
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

impl Drop for ResponseFrame {
    fn drop(&mut self) {
        self.json_fragment.zeroize();
    }
}

#[derive(Default)]
pub struct ResponseWriter {
    next_message: u64,
}

impl ResponseWriter {
    pub async fn write<T: serde::Serialize>(
        &mut self,
        stream: &mut (impl AsyncWrite + Unpin),
        response: &T,
    ) -> Result<()> {
        let next_message = self.next_message.checked_add(1).ok_or_else(protocol)?;
        // ponytail: 一次物化逻辑响应；实际出现内存压力时再改增量编码，不拒绝合法大记录。
        let json = Zeroizing::new(serde_json::to_string(response).map_err(|_| protocol())?);
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
pub struct ResponseReader {
    next_message: u64,
    frame_timeout: Option<Duration>,
}

impl ResponseReader {
    pub fn for_login() -> Self {
        Self {
            next_message: 0,
            frame_timeout: Some(LOGIN_FRAME_TIMEOUT),
        }
    }

    /// 由连接的持续读取任务持有；调用方不能取消后在同一 socket 重启半条响应。
    pub async fn read<T: serde::de::DeserializeOwned>(
        &mut self,
        stream: &mut impl PeerStream,
    ) -> Result<T> {
        self.read_with_progress(stream, || {}).await
    }

    /// 完整合法分片即为读取进度；不必等整个大对象解码完才刷新连接 idle。
    pub async fn read_with_progress<T: serde::de::DeserializeOwned>(
        &mut self,
        stream: &mut impl PeerStream,
        progress: impl FnMut(),
    ) -> Result<T> {
        let first = self.next_message == 0;
        self.next_with_progress(stream, progress)
            .await?
            .ok_or_else(|| if first { silent_close() } else { eof() })
    }

    /// 只有完整消息之间的 EOF 是正常结束，半帧或半条消息仍是传输失败。
    pub async fn next_with_progress<T: serde::de::DeserializeOwned>(
        &mut self,
        stream: &mut impl PeerStream,
        mut progress: impl FnMut(),
    ) -> Result<Option<T>> {
        let next_message = self.next_message.checked_add(1).ok_or_else(protocol)?;
        let mut json = Zeroizing::new(String::new());
        loop {
            let nothing_received = self.next_message == 0 && json.is_empty();
            let Some(frame) = read_frame_with_timeout::<ResponseFrame>(
                stream,
                nothing_received,
                self.frame_timeout,
            )
            .await?
            else {
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
        let older_unavailable = "运行中的后台版本较旧且不兼容（对端进程号不可用）；请用旧二进制执行 `lattice daemon stop` 停止它";
        let newer_unavailable =
            "运行中的后台版本较新且不兼容（对端进程号不可用）；请换用匹配的前端";
        check_magic(MAGIC, None).expect("当前协议版本魔数必须通过");
        for version in 0..PROTOCOL_VERSION {
            let error = check_magic(&magic_for_version(version), None).unwrap_err();
            assert_eq!(
                (error.kind, error.message.as_str()),
                (LocalErrorKind::IncompatibleBackend, older_unavailable),
                "version={version}"
            );
        }
        for version in (PROTOCOL_VERSION + 1)..=u8::MAX {
            let error = check_magic(&magic_for_version(version), None).unwrap_err();
            assert_eq!(
                (error.kind, error.message.as_str()),
                (LocalErrorKind::IncompatibleBackend, newer_unavailable),
                "version={version}"
            );
        }
        // 版本 61 必须固定判为较旧协议。
        let error = check_magic(&magic_for_version(61), None).unwrap_err();
        assert_eq!(
            (error.kind, error.message.as_str()),
            (LocalErrorKind::IncompatibleBackend, older_unavailable)
        );
        // 非套接字描述符（例如管道）无法经 SO_PEERCRED 取对端进程号，必须回退为“对端进程号不可用”。
        let (pipe_reader, _pipe_writer) = std::io::pipe().unwrap();
        let pipe_error = check_magic(
            &magic_for_version(PROTOCOL_VERSION - 1),
            Some(pipe_reader.as_fd()),
        )
        .unwrap_err();
        assert_eq!(
            (pipe_error.kind, pipe_error.message.as_str()),
            (LocalErrorKind::IncompatibleBackend, older_unavailable)
        );

        let self_pid = std::process::id();
        let older_with_pid = format!(
            "运行中的后台版本较旧且不兼容（对端进程号：{self_pid}）；请用旧二进制执行 `lattice daemon stop` 停止它"
        );
        let newer_with_pid =
            format!("运行中的后台版本较新且不兼容（对端进程号：{self_pid}）；请换用匹配的前端");
        let cases = [
            (*MAGIC, 4, b"null".as_slice(), None),
            (
                magic_for_version(PROTOCOL_VERSION - 1),
                4,
                b"null".as_slice(),
                Some((
                    LocalErrorKind::IncompatibleBackend,
                    Some(older_with_pid.as_str()),
                )),
            ),
            (
                magic_for_version(PROTOCOL_VERSION + 1),
                4,
                b"null".as_slice(),
                Some((
                    LocalErrorKind::IncompatibleBackend,
                    Some(newer_with_pid.as_str()),
                )),
            ),
            (
                *b"XXLOCAL9",
                4,
                b"null".as_slice(),
                Some((LocalErrorKind::Protocol, None)),
            ),
            (
                *MAGIC,
                0,
                b"".as_slice(),
                Some((LocalErrorKind::Protocol, None)),
            ),
            (
                *MAGIC,
                (MAX_FRAME + 1) as u32,
                b"".as_slice(),
                Some((LocalErrorKind::Protocol, None)),
            ),
        ];

        for (magic, length, payload, expected) in cases {
            tokio::time::timeout(Duration::from_secs(1), async {
                let (mut sender, mut receiver) = UnixStream::pair().unwrap();
                sender.write_all(&magic).await.unwrap();
                sender.write_u32(length).await.unwrap();
                sender.write_all(payload).await.unwrap();
                let result = read::<serde_json::Value>(&mut receiver).await;
                match expected {
                    None => assert_eq!(result.unwrap(), serde_json::Value::Null),
                    Some((kind, expected_message)) => {
                        let error = result.unwrap_err();
                        assert_eq!(error.kind, kind, "{magic:?}");
                        if let Some(expected_message) = expected_message {
                            assert_eq!(error.message, expected_message, "{magic:?}");
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
        for magic in [
            magic_for_version(PROTOCOL_VERSION - 1),
            magic_for_version(PROTOCOL_VERSION + 1),
        ] {
            tokio::time::timeout(Duration::from_secs(1), async {
                let (mut client, mut backend) = UnixStream::pair().unwrap();
                client.write_all(&magic).await.unwrap();
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
    }

    /// harness 不能依赖产品 crate，只能记录同一协议版本号；升级时在这里失败并指出要改哪个文件。
    #[test]
    fn harness_frame_magic_tracks_the_current_magic() {
        let harness = include_str!("../../lattice-system-harness/tests/support/mod.rs");
        assert!(
            harness.contains(&format!("const PROTOCOL_VERSION: u8 = {PROTOCOL_VERSION};")),
            "update PROTOCOL_VERSION in crates/lattice-system-harness/tests/support/mod.rs to {PROTOCOL_VERSION}"
        );
    }

    /// 远大于套接字缓冲区的请求，对端只读魔数就关闭。
    async fn oversized_request_against(reply: Option<[u8; 8]>) -> LocalError {
        let (mut client, mut backend) = UnixStream::pair().unwrap();
        let backend = tokio::spawn(async move {
            let mut magic = [0; 8];
            backend.read_exact(&mut magic).await.unwrap();
            if let Some(reply) = reply {
                backend.write_all(&reply).await.unwrap();
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
        let self_pid = std::process::id();
        let expected_older = format!(
            "运行中的后台版本较旧且不兼容（对端进程号：{self_pid}）；请用旧二进制执行 `lattice daemon stop` 停止它"
        );
        let expected_newer =
            format!("运行中的后台版本较新且不兼容（对端进程号：{self_pid}）；请换用匹配的前端");
        let older = oversized_request_against(Some(magic_for_version(PROTOCOL_VERSION - 1))).await;
        assert_eq!(
            (older.kind, older.message.as_str()),
            (LocalErrorKind::IncompatibleBackend, expected_older.as_str())
        );
        let newer = oversized_request_against(Some(magic_for_version(PROTOCOL_VERSION + 1))).await;
        assert_eq!(
            (newer.kind, newer.message.as_str()),
            (LocalErrorKind::IncompatibleBackend, expected_newer.as_str())
        );
    }

    #[test]
    fn generic_io_error_conversion_and_dir_checks_use_chinese_without_client_guidance() {
        for kind in [
            std::io::ErrorKind::NotFound,
            std::io::ErrorKind::ConnectionRefused,
        ] {
            let error = LocalError::from(std::io::Error::from(kind));
            assert_eq!(error.kind, LocalErrorKind::NotRunning);
            assert_eq!(error.message, "本地连接目标不存在或拒绝连接");
            assert!(
                !error.message.contains("后台未运行")
                    && !error.message.contains("systemctl")
                    && !error.message.contains("lattice daemon install"),
                "通用 io::Error 转换不得附带客户端操作建议：{}",
                error.message
            );
        }
        for (kind, expected_kind, expected_message) in [
            (
                std::io::ErrorKind::PermissionDenied,
                LocalErrorKind::Permission,
                "本地连接权限不足；回执状态可能未知",
            ),
            (
                std::io::ErrorKind::BrokenPipe,
                LocalErrorKind::Transport,
                "本地连接失败；回执状态可能未知",
            ),
        ] {
            let error = LocalError::from(std::io::Error::from(kind));
            assert_eq!(error.kind, expected_kind);
            assert_eq!(error.message, expected_message);
        }

        use std::os::unix::fs::PermissionsExt;
        struct TempDir(std::path::PathBuf);
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = TempDir(std::env::temp_dir().join(format!(
            "ltc-ipc-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )));
        std::fs::create_dir_all(&dir.0).unwrap();
        let local_dir = dir.0.join("local");
        std::fs::create_dir(&local_dir).unwrap();
        std::fs::set_permissions(&local_dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        let perm_err = check_dir(&local_dir, true).unwrap_err();
        assert_eq!(perm_err.kind, LocalErrorKind::Permission);
        assert!(
            perm_err.message.contains(&local_dir.display().to_string())
                && perm_err.message.contains("0777")
                && !perm_err
                    .message
                    .contains("local endpoint ownership or permissions rejected"),
            "0777 目录校验应返回含路径与权限的中文报错：{}",
            perm_err.message
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

    #[test]
    fn encoded_request_lengths_match_actual_json_bytes() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt, path::PathBuf};

        use lattice_context::{Channel, ContextEdit, ContextOp};
        use lattice_conversation::{ImageRef, MessageSemantic, SessionId, TurnId};
        use lattice_session::store::{NodeId, SettingsVersion};

        use crate::types::{
            ContextImage, ContextOpRequest, FeedRequest, Request, TurnInputItem, TurnRequest,
        };

        enum PartSpec {
            Text(String),
            Image(Vec<u8>),
        }

        fn png_with_len(total_len: usize, fill: u8) -> Vec<u8> {
            const HEADER: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
            assert!(total_len >= HEADER.len(), "PNG 测试字节长度不能小于魔数");
            let mut bytes = vec![fill; total_len];
            bytes[..HEADER.len()].copy_from_slice(HEADER);
            bytes
        }

        let sid = SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c").unwrap();
        let tid = TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").unwrap();
        let non_utf8_ws = PathBuf::from(OsStr::from_bytes(b"/workspace/proj-\xff\xfe/repo"));
        let utf8_ws = PathBuf::from("/workspace/proj/repo");
        let version: SettingsVersion =
            serde_json::from_value(serde_json::json!("ab".repeat(32))).unwrap();
        let cli_channel = Channel::parse("cli").unwrap();
        let tui_channel = Channel::parse("tui").unwrap();

        let img_mod0 = png_with_len(9, 0x11);
        let img_mod1 = png_with_len(10, 0x22);
        let img_mod2 = png_with_len(8, 0x33);
        let img_large = png_with_len(300, 0x44);
        let escaped_text = "\"\\\n\r\t\u{0001}\u{001f}中文测试😀🚀".repeat(32);

        let cases = [
            (
                "纯文本_转义字符_非UTF8工作区_无基线",
                non_utf8_ws.clone(),
                None,
                None,
                vec![(
                    cli_channel.clone(),
                    vec![PartSpec::Text(escaped_text.clone())],
                )],
            ),
            (
                "纯图片_余数0_1_2_有设置与位置基线",
                non_utf8_ws.clone(),
                Some(version),
                Some(NodeId(7)),
                vec![(
                    cli_channel.clone(),
                    vec![
                        PartSpec::Image(img_mod0.clone()),
                        PartSpec::Image(img_mod1.clone()),
                        PartSpec::Image(img_mod2.clone()),
                    ],
                )],
            ),
            (
                "多条输入_图文交错_含大量转义与三种长度余数",
                non_utf8_ws.clone(),
                Some(version),
                Some(NodeId(12)),
                vec![
                    (
                        cli_channel.clone(),
                        vec![
                            PartSpec::Text(escaped_text),
                            PartSpec::Image(img_mod0.clone()),
                            PartSpec::Text("中间文本\\\"\n\u{1}".into()),
                        ],
                    ),
                    (
                        tui_channel.clone(),
                        vec![
                            PartSpec::Image(img_mod1.clone()),
                            PartSpec::Image(img_mod2.clone()),
                            PartSpec::Text("结尾文本🚀".into()),
                        ],
                    ),
                ],
            ),
            (
                "同一图片出现两次",
                non_utf8_ws.clone(),
                None,
                Some(NodeId(3)),
                vec![(
                    cli_channel.clone(),
                    vec![
                        PartSpec::Image(img_large.clone()),
                        PartSpec::Text("间隔文本\u{1}\\\"".into()),
                        PartSpec::Image(img_large.clone()),
                    ],
                )],
            ),
            (
                "UTF8工作区_单图余数0",
                utf8_ws,
                Some(version),
                None,
                vec![(tui_channel.clone(), vec![PartSpec::Image(img_mod0.clone())])],
            ),
        ];

        for (label, workspace, expected_settings, expected_position, items) in cases {
            let mut real_inputs = Vec::with_capacity(items.len());
            let mut plan_inputs = Vec::with_capacity(items.len());
            for (channel, specs) in items {
                let mut real_parts = Vec::with_capacity(specs.len());
                let mut plan_parts = Vec::with_capacity(specs.len());
                for spec in specs {
                    match spec {
                        PartSpec::Text(text) => {
                            real_parts.push(UserInputPart::Text { text: text.clone() });
                            plan_parts.push(UserInputPart::Text { text });
                        }
                        PartSpec::Image(bytes) => {
                            let image_ref =
                                ImageRef::from_bytes(&bytes).expect("构造测试图片引用应成功");
                            real_parts.push(UserInputPart::Image { image: bytes });
                            plan_parts.push(UserInputPart::Image { image: image_ref });
                        }
                    }
                }
                real_inputs.push(TurnInputItem {
                    channel: channel.clone(),
                    parts: real_parts,
                });
                plan_inputs.push(TurnInputItem {
                    channel,
                    parts: plan_parts,
                });
            }

            let feed_real = FeedRequest {
                session: sid,
                workspace: workspace.clone(),
                channel: real_inputs[0].channel.clone(),
                parts: real_inputs[0].parts.clone(),
                expected_position,
            };
            let feed_plan = PlannedFeedRequest {
                session: sid,
                workspace: workspace.clone(),
                channel: plan_inputs[0].channel.clone(),
                parts: plan_inputs[0].parts.clone(),
                expected_position,
            };
            let actual_feed_len = serde_json::to_vec(&Request::Feed { request: feed_real })
                .expect("实发 feed 请求序列化不应失败")
                .len();
            assert_eq!(
                encoded_feed_len(&feed_plan),
                actual_feed_len,
                "用例 {label} 的 feed 零载荷计长结果必须与实发 JSON 字节数完全一致"
            );

            let real = TurnRequest {
                turn_id: tid,
                session: sid,
                workspace: workspace.clone(),
                trigger_channel: cli_channel.clone(),
                inputs: real_inputs,
                expected_settings,
                expected_position,
            };
            let plan = PlannedTurnRequest {
                turn_id: tid,
                session: sid,
                workspace,
                trigger_channel: cli_channel.clone(),
                inputs: plan_inputs,
                expected_settings,
                expected_position,
            };
            let actual_len = serde_json::to_vec(&Request::Submit { request: real })
                .expect("实发请求序列化不应失败")
                .len();
            assert_eq!(
                encoded_submit_len(&plan),
                actual_len,
                "用例 {label} 的 submit 零载荷计长结果必须与实发 JSON 字节数完全一致"
            );
        }

        let img0_ref = ImageRef::from_bytes(&img_mod0).expect("图片 0 引用应合法");
        let img1_ref = ImageRef::from_bytes(&img_mod1).expect("图片 1 引用应合法");
        let img2_ref = ImageRef::from_bytes(&img_mod2).expect("图片 2 引用应合法");
        let op_with_images = ContextOp::new(
            tui_channel,
            vec![
                ContextEdit::replace_input(
                    NodeId(1),
                    vec![
                        MessageSemantic::Text {
                            text: "替换文本\\\"\n".into(),
                        },
                        MessageSemantic::Image { image: img0_ref },
                        MessageSemantic::Image { image: img1_ref },
                        MessageSemantic::Image { image: img2_ref },
                    ],
                )
                .expect("ReplaceInput 编辑应合法"),
            ],
        )
        .expect("ContextOp 应合法");
        for (label, images_real, images_plan, expected_position) in [
            ("无附带图片", Vec::new(), Vec::new(), None),
            (
                "三张不同余数附带图片",
                vec![
                    ContextImage { image: img_mod0 },
                    ContextImage { image: img_mod1 },
                    ContextImage { image: img_mod2 },
                ],
                vec![
                    ContextImage { image: img0_ref },
                    ContextImage { image: img1_ref },
                    ContextImage { image: img2_ref },
                ],
                Some(NodeId(5)),
            ),
        ] {
            let real = ContextOpRequest {
                session: sid,
                workspace: non_utf8_ws.clone(),
                op: op_with_images.clone(),
                images: images_real,
                expected_position,
            };
            let plan = PlannedContextOpRequest {
                session: sid,
                workspace: non_utf8_ws.clone(),
                op: op_with_images.clone(),
                images: images_plan,
                expected_position,
            };
            let actual = serde_json::to_vec(&Request::ContextOp { request: real })
                .expect("实发 context_op 请求序列化不应失败")
                .len();
            assert_eq!(
                encoded_context_op_len(&plan),
                actual,
                "用例 {label} 的 context_op 零载荷计长结果必须与实发 JSON 字节数完全一致"
            );
        }
    }
}
