// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `Ctrl+V` 读取剪贴板，以及剪贴板图片的私有暂存目录。

use std::ffi::OsStr;
use std::fmt::Write;
use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use lattice_local::{ImageRef, PrivateDirectory};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(1);
const IMAGE_TYPES: &[&str] = &["image/png", "image/webp", "image/jpeg", "image/gif"];
const WAYLAND_TEXT_TYPES: &[&str] = &["text/plain;charset=utf-8", "text/plain"];
const X11_TEXT_TYPES: &[&str] = &["UTF8_STRING"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Backend {
    Wayland,
    X11,
}

/// 剪贴板读到的内容。可能含密码等敏感文字或图片字节，因此不实现 `Debug`。
pub(crate) enum Content {
    Image(Vec<u8>),
    Text(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ClipboardError {
    NoSession,
    Missing(Backend),
    TooLarge,
    Failed(String),
}

impl Backend {
    pub(crate) fn program(self) -> &'static str {
        match self {
            Self::Wayland => "wl-paste",
            Self::X11 => "xclip",
        }
    }

    fn list_args(self) -> &'static [&'static str] {
        match self {
            Self::Wayland => &["--list-types"],
            Self::X11 => &["-selection", "clipboard", "-t", "TARGETS", "-o"],
        }
    }

    fn text_types(self) -> &'static [&'static str] {
        match self {
            Self::Wayland => WAYLAND_TEXT_TYPES,
            Self::X11 => X11_TEXT_TYPES,
        }
    }
}

fn detect() -> Option<Backend> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()) {
        Some(Backend::Wayland)
    } else if std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty()) {
        Some(Backend::X11)
    } else {
        None
    }
}

pub(crate) async fn read_system() -> Result<Content, ClipboardError> {
    let Some(backend) = detect() else {
        return Err(ClipboardError::NoSession);
    };
    read(backend, Path::new(backend.program()), COMMAND_TIMEOUT).await
}

pub(crate) async fn read(
    backend: Backend,
    program: &Path,
    timeout: Duration,
) -> Result<Content, ClipboardError> {
    let raw_types = run_command(backend, program, backend.list_args(), timeout).await?;
    let listing = String::from_utf8_lossy(&raw_types);
    let available: Vec<&str> = listing
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();

    if let Some(mime) = IMAGE_TYPES
        .iter()
        .copied()
        .find(|candidate| available.contains(candidate))
    {
        let bytes = read_mime(backend, program, mime, timeout).await?;
        return Ok(Content::Image(bytes));
    }

    if let Some(mime) = backend
        .text_types()
        .iter()
        .copied()
        .find(|candidate| available.contains(candidate))
    {
        let bytes = read_mime(backend, program, mime, timeout).await?;
        return Ok(Content::Text(String::from_utf8_lossy(&bytes).into_owned()));
    }

    Err(ClipboardError::Failed("剪贴板中没有图片或文字".into()))
}

async fn read_mime(
    backend: Backend,
    program: &Path,
    mime: &str,
    timeout: Duration,
) -> Result<Vec<u8>, ClipboardError> {
    match backend {
        Backend::Wayland => {
            run_command(backend, program, &["--no-newline", "--type", mime], timeout).await
        }
        Backend::X11 => {
            run_command(
                backend,
                program,
                &["-selection", "clipboard", "-t", mime, "-o"],
                timeout,
            )
            .await
        }
    }
}

async fn run_command(
    backend: Backend,
    program: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<Vec<u8>, ClipboardError> {
    let name = program
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_else(|| backend.program());
    let mut child = match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(ClipboardError::Missing(backend));
        }
        Err(err) => {
            return Err(ClipboardError::Failed(format!("启动 {name} 失败：{err}")));
        }
    };

    let mut stdout = child.stdout.take().expect("已将 stdout 设为管道");
    let limit = lattice_local::MAX_IMAGE_BYTES + 1;

    enum Outcome {
        Done(std::process::ExitStatus, Vec<u8>),
        TooLarge,
        Io(std::io::Error),
    }

    let outcome = tokio::time::timeout(timeout, async {
        let mut out = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let remaining = limit - out.len();
            let max_read = chunk.len().min(remaining);
            match stdout.read(&mut chunk[..max_read]).await {
                Ok(0) => break,
                Ok(n) => {
                    out.extend_from_slice(&chunk[..n]);
                    if out.len() >= limit {
                        return Outcome::TooLarge;
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(err) => return Outcome::Io(err),
            }
        }
        drop(stdout);
        match child.wait().await {
            Ok(status) => Outcome::Done(status, out),
            Err(err) => Outcome::Io(err),
        }
    })
    .await;

    match outcome {
        Ok(Outcome::Done(status, out)) => {
            if status.success() {
                Ok(out)
            } else {
                let code = match status.code() {
                    Some(code) => format!("退出码 {code}"),
                    None => "被信号终止".into(),
                };
                Err(ClipboardError::Failed(format!("{name} 执行失败（{code}）")))
            }
        }
        Ok(Outcome::TooLarge) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(ClipboardError::TooLarge)
        }
        Ok(Outcome::Io(err)) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(ClipboardError::Failed(format!(
                "读取 {name} 输出失败：{err}"
            )))
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(ClipboardError::Failed(format!("{name} 读取超时")))
        }
    }
}

pub(crate) struct Staging {
    dir: tempfile::TempDir,
    private: PrivateDirectory,
}

impl Staging {
    pub(crate) fn create_in(base: &Path) -> anyhow::Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix("lattice-tui-")
            .permissions(Permissions::from_mode(0o700))
            .tempdir_in(base)?;
        let private = PrivateDirectory::open(dir.path())?
            .ok_or_else(|| anyhow::anyhow!("剪贴板暂存目录在创建后消失"))?;
        Ok(Self { dir, private })
    }

    pub(crate) fn store(&self, image: &ImageRef, bytes: &[u8]) -> anyhow::Result<PathBuf> {
        let name = hex_sha256(&image.sha256);
        self.private.replace(OsStr::new(&name), bytes)?;
        Ok(self.dir.path().join(name))
    }
}

fn hex_sha256(sha256: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in sha256 {
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("写入测试剪贴板脚本");
        std::fs::set_permissions(&path, Permissions::from_mode(0o755))
            .expect("设置测试剪贴板脚本权限");
        path
    }

    #[tokio::test]
    async fn wayland_prefers_png_over_earlier_listed_jpeg_and_text() {
        let dir = tempfile::tempdir().expect("创建测试临时目录");
        let script = write_script(
            dir.path(),
            "wl-paste",
            r#"if [ "$*" = "--list-types" ]; then
  printf 'text/plain\nimage/jpeg\nimage/png\n'
  exit 0
fi
if [ "$*" = "--no-newline --type image/png" ]; then
  printf '\211PNG\r\n\032\npng-payload'
  exit 0
fi
exit 2"#,
        );
        let Ok(Content::Image(bytes)) =
            read(Backend::Wayland, &script, Duration::from_secs(1)).await
        else {
            panic!("Wayland 应按优先级读取到图片");
        };
        assert!(
            bytes == b"\x89PNG\r\n\x1a\npng-payload",
            "Wayland 应按优先级读取 image/png"
        );
    }

    #[tokio::test]
    async fn x11_lists_targets_and_reads_png_with_selection_clipboard() {
        let dir = tempfile::tempdir().expect("创建测试临时目录");
        let script = write_script(
            dir.path(),
            "xclip",
            r#"if [ "$*" = "-selection clipboard -t TARGETS -o" ]; then
  printf 'UTF8_STRING\nimage/png\n'
  exit 0
fi
if [ "$*" = "-selection clipboard -t image/png -o" ]; then
  printf '\211PNG\r\n\032\nx11-payload'
  exit 0
fi
exit 2"#,
        );
        let Ok(Content::Image(bytes)) = read(Backend::X11, &script, Duration::from_secs(1)).await
        else {
            panic!("X11 应按剪贴板参数读取到图片");
        };
        assert!(
            bytes == b"\x89PNG\r\n\x1a\nx11-payload",
            "X11 应按剪贴板参数读取 image/png"
        );
    }

    #[tokio::test]
    async fn falls_back_to_utf8_plain_text_when_no_image_type_is_present() {
        let dir = tempfile::tempdir().expect("创建测试临时目录");
        let script = write_script(
            dir.path(),
            "wl-paste",
            r#"if [ "$*" = "--list-types" ]; then
  printf 'text/plain\ntext/plain;charset=utf-8\n'
  exit 0
fi
if [ "$*" = "--no-newline --type text/plain;charset=utf-8" ]; then
  printf '剪贴板文字内容'
  exit 0
fi
exit 2"#,
        );
        let Ok(Content::Text(text)) = read(Backend::Wayland, &script, Duration::from_secs(1)).await
        else {
            panic!("没有图片类型时应读取到文字");
        };
        assert!(
            text == "剪贴板文字内容",
            "没有图片类型时应返回 text/plain;charset=utf-8 文字"
        );
    }

    #[tokio::test]
    async fn continuous_output_stops_at_limit_and_returns_too_large_within_timeout() {
        let dir = tempfile::tempdir().expect("创建测试临时目录");
        let script = write_script(
            dir.path(),
            "wl-paste",
            r#"if [ "$*" = "--list-types" ]; then
  printf 'image/png\n'
  exit 0
fi
if [ "$*" = "--no-newline --type image/png" ]; then
  exec cat /dev/zero
fi
exit 2"#,
        );
        let timeout = Duration::from_secs(2);
        let started = Instant::now();
        let result = read(Backend::Wayland, &script, timeout).await;
        assert!(
            matches!(result, Err(ClipboardError::TooLarge)),
            "持续输出超过上限时应返回 TooLarge"
        );
        assert!(
            started.elapsed() < timeout,
            "应在时限内截断返回，实际耗时：{:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn slow_command_times_out_and_returns_failed_quickly() {
        let dir = tempfile::tempdir().expect("创建测试临时目录");
        let script = write_script(dir.path(), "wl-paste", "sleep 5");
        let started = Instant::now();
        let result = read(Backend::Wayland, &script, Duration::from_secs(1)).await;
        assert!(
            matches!(&result, Err(ClipboardError::Failed(detail)) if detail.contains("超时")),
            "超时子进程应返回写明超时的 Failed"
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "超时终止并回收用时应小于 3 秒，实际耗时：{:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn missing_program_returns_missing_backend() {
        let dir = tempfile::tempdir().expect("创建测试临时目录");
        let missing = dir.path().join("nonexistent-wl-paste");
        let result = read(Backend::Wayland, &missing, Duration::from_secs(1)).await;
        assert!(
            matches!(result, Err(ClipboardError::Missing(Backend::Wayland))),
            "程序不存在时应返回 Missing(Backend::Wayland)"
        );
    }

    #[test]
    fn staging_creates_0700_dir_writes_0600_sha256_file_and_cleans_up_on_drop() {
        let base = tempfile::tempdir().expect("创建外层临时目录");
        let staging = Staging::create_in(base.path()).expect("创建剪贴板私有暂存目录");
        let staging_dir = staging.dir.path().to_path_buf();

        let dir_mode = std::fs::metadata(&staging_dir)
            .expect("读取暂存目录元数据")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "暂存目录权限模式必须为 0700");

        let bytes = b"\x89PNG\r\n\x1a\nstaged-image";
        let image = ImageRef::from_bytes(bytes).expect("构造测试图片引用");
        let path = staging.store(&image, bytes).expect("写入暂存图片文件");

        assert_eq!(
            path.parent(),
            Some(staging_dir.as_path()),
            "暂存文件应位于私有暂存目录内"
        );
        assert_eq!(
            path.file_name().and_then(|s| s.to_str()),
            Some(hex_sha256(&image.sha256).as_str()),
            "暂存文件名必须为图片 SHA-256 的 64 位小写十六进制"
        );
        let file_mode = std::fs::metadata(&path)
            .expect("读取暂存文件元数据")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600, "暂存文件权限模式必须为 0600");
        assert_eq!(
            std::fs::read(&path).expect("读取暂存文件内容"),
            bytes,
            "暂存文件内容应与写入字节一致"
        );

        drop(staging);
        assert!(!staging_dir.exists(), "丢弃 Staging 后暂存目录应被删除");
    }
}
