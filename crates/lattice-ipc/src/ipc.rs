// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 本地 IPC 协议版本、16 MiB 单帧编解码、响应分片与对端 UID 核验。

use std::time::Duration;

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::UnixStream,
};
use zeroize::{Zeroize, Zeroizing};

use crate::types::{LocalError, LocalErrorKind, Result};

mod defs {
    pub const PROTOCOL_VERSION: u8 = 63;
    pub const MAGIC: &[u8; 8] = &magic_for_version(PROTOCOL_VERSION);
    /// 帧魔数固定为 7 字节前缀 `LTLOCAL` 加 1 个无符号协议版本字节（`0..=255`），帧长度不变。
    pub const fn magic_for_version(version: u8) -> [u8; 8] {
        let mut magic = *b"LTLOCAL\0";
        magic[7] = version;
        magic
    }
    pub const MAX_FRAME: usize = 16 * 1024 * 1024;
}

#[cfg(not(any(test, feature = "test-utils")))]
use defs::{MAGIC, MAX_FRAME, PROTOCOL_VERSION, magic_for_version};
#[cfg(any(test, feature = "test-utils"))]
pub use defs::{MAGIC, MAX_FRAME, PROTOCOL_VERSION, magic_for_version};

const LOGIN_FRAME_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const INCOMPATIBLE_BACKEND: &str = "the running backend is an incompatible older version; stop it with the older binary's `lattice daemon stop`";
const NEWER_BACKEND: &str = "the running backend is newer than this client; use the newer binary";
const SILENT_CLOSE: &str = "the backend closed the connection without replying; if an older backend is still running, stop it with the older binary's `lattice daemon stop`";

pub fn permission() -> LocalError {
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

/// 前缀相符时取出版本字节；新旧由 `check_magic` 按版本数值比较。
fn magic_version(magic: &[u8; 8]) -> Option<u8> {
    let version = magic[7];
    (*magic == magic_for_version(version)).then_some(version)
}

fn check_magic(magic: &[u8; 8]) -> Result<()> {
    match magic_version(magic) {
        Some(version) if version == PROTOCOL_VERSION => Ok(()),
        Some(version) if version < PROTOCOL_VERSION => Err(LocalError::new(
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

pub fn uid() -> u32 {
    // Linux geteuid 无参数、无内存访问；通信两端必须匹配实际有效 UID。
    rustix::process::geteuid().as_raw()
}

pub fn check_peer(stream: &UnixStream) -> Result<()> {
    let cred = rustix::net::sockopt::socket_peercred(stream).map_err(std::io::Error::from)?;
    if cred.uid.as_raw() != uid() {
        return Err(permission());
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
    stream: &mut (impl AsyncRead + Unpin),
    reset_is_eof: bool,
) -> Result<Option<T>> {
    read_frame_with_timeout(stream, reset_is_eof, None).await
}

pub async fn read_login_action<T: serde::de::DeserializeOwned>(
    stream: &mut (impl AsyncRead + Unpin),
) -> Result<Option<T>> {
    read_frame_with_timeout(stream, false, Some(LOGIN_FRAME_TIMEOUT)).await
}

async fn read_frame_with_timeout<T: serde::de::DeserializeOwned>(
    stream: &mut (impl AsyncRead + Unpin),
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
        check_magic(&magic)?;
        read_body(stream).await.map(Some)
    };
    match timeout {
        Some(timeout) => tokio::time::timeout(timeout, remainder)
            .await
            .map_err(|_| LocalError::new(LocalErrorKind::Transport, "login frame timed out"))?,
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
            return Err(std::io::Error::other("frame exceeds limit"));
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
        stream: &mut (impl AsyncRead + Unpin),
    ) -> Result<T> {
        self.read_with_progress(stream, || {}).await
    }

    /// 完整合法分片即为读取进度；不必等整个大对象解码完才刷新连接 idle。
    pub async fn read_with_progress<T: serde::de::DeserializeOwned>(
        &mut self,
        stream: &mut (impl AsyncRead + Unpin),
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
        stream: &mut (impl AsyncRead + Unpin),
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
        check_magic(MAGIC).expect("当前协议版本魔数必须通过");
        for version in 0..PROTOCOL_VERSION {
            let error = check_magic(&magic_for_version(version)).unwrap_err();
            assert_eq!(
                (error.kind, error.message.as_str()),
                (LocalErrorKind::IncompatibleBackend, INCOMPATIBLE_BACKEND),
                "version={version}"
            );
        }
        for version in (PROTOCOL_VERSION + 1)..=u8::MAX {
            let error = check_magic(&magic_for_version(version)).unwrap_err();
            assert_eq!(
                (error.kind, error.message.as_str()),
                (LocalErrorKind::IncompatibleBackend, NEWER_BACKEND),
                "version={version}"
            );
        }
        // 版本 61 必须固定判为较旧协议。
        let error = check_magic(&magic_for_version(61)).unwrap_err();
        assert_eq!(
            (error.kind, error.message.as_str()),
            (LocalErrorKind::IncompatibleBackend, INCOMPATIBLE_BACKEND)
        );

        let cases = [
            (*MAGIC, 4, b"null".as_slice(), None),
            (
                magic_for_version(PROTOCOL_VERSION - 1),
                4,
                b"null".as_slice(),
                Some((
                    LocalErrorKind::IncompatibleBackend,
                    Some(INCOMPATIBLE_BACKEND),
                )),
            ),
            (
                magic_for_version(PROTOCOL_VERSION + 1),
                4,
                b"null".as_slice(),
                Some((LocalErrorKind::IncompatibleBackend, Some(NEWER_BACKEND))),
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
        let older = oversized_request_against(Some(magic_for_version(PROTOCOL_VERSION - 1))).await;
        assert_eq!(
            (older.kind, older.message.as_str()),
            (LocalErrorKind::IncompatibleBackend, INCOMPATIBLE_BACKEND)
        );
        let newer = oversized_request_against(Some(magic_for_version(PROTOCOL_VERSION + 1))).await;
        assert_eq!(
            (newer.kind, newer.message.as_str()),
            (LocalErrorKind::IncompatibleBackend, NEWER_BACKEND)
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
