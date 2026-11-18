// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 图片字节与文件的有界读取、核验与重读比对，供 CLI 与 TUI 共用。

use std::{io::Read, path::Path};

use lattice_conversation::{
    BoundToolCall, ImageBytesError, ImageMediaType, ImageRef, MAX_IMAGE_BYTES, MessageSemantic,
    UserInputError, normalize_user_message,
};
use lattice_ipc::{LocalError, LocalErrorKind, Result};

/// 读取并核验第 `ordinal` 张图片文件（从 1 起），返回其 `ImageRef`；读到的字节随即丢弃。
pub fn image_file_ref(ordinal: usize, path: &Path) -> Result<ImageRef> {
    let (image, _bytes) = read_image(ordinal, path)?;
    Ok(image)
}

/// 核验内存中的图片字节，并连同 `existing` 检查单轮张数与总字节上限（图片序号为 `existing.len() + 1`）。
pub fn image_bytes_ref(bytes: &[u8], existing: &[ImageRef]) -> Result<ImageRef> {
    checked_image_ref(
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        bytes,
        existing,
    )
}

/// 判断路径是否指向受支持的图片，并连同 `existing` 核验单轮张数与总字节上限：
/// 不存在、不是常规文件、读取失败或魔数不是 PNG/JPEG/WebP/GIF 时返回 `Ok(None)`；
/// 单张长度不合规或加入后超过单轮张数、总量上限时返回 `BadRequest`（说明取 `UserInputError`，图片序号为 `existing.len() + 1`）。
pub fn probe_image_file(path: &Path, existing: &[ImageRef]) -> Result<Option<ImageRef>> {
    let Ok((file_len, mut file)) = open_bounded(path) else {
        return Ok(None);
    };
    // 先只读文件头判断是不是图片，粘贴普通大文件的路径时不必读满上限。
    let mut bytes = Vec::new();
    if (&mut file).take(12).read_to_end(&mut bytes).is_err()
        || ImageMediaType::sniff(&bytes).is_none()
    {
        return Ok(None);
    }
    if file.read_to_end(&mut bytes).is_err() {
        return Ok(None);
    }
    checked_image_ref(file_len, &bytes, existing).map(Some)
}

/// 重读第 `ordinal` 张图片，与 `expected` 完全一致时返回字节；读取失败保留原因，
/// 读到不同的图片时报告“第 {ordinal} 张图片在预检后已改变，请重新提交”。
pub fn read_verified_image(ordinal: usize, path: &Path, expected: &ImageRef) -> Result<Vec<u8>> {
    let (actual, bytes) = read_image(ordinal, path)?;
    if actual != *expected {
        return Err(LocalError::new(
            LocalErrorKind::BadRequest,
            &format!("第 {ordinal} 张图片在预检后已改变，请重新提交"),
        ));
    }
    Ok(bytes)
}

enum BoundedReadError {
    NotRegularFile,
    Io(std::io::Error),
}

/// 取路径元数据、核验为常规文件并打开，返回文件长度与最多读取 `MAX_IMAGE_BYTES + 1` 字节的读取端。
fn open_bounded(
    path: &Path,
) -> std::result::Result<(u64, std::io::Take<std::fs::File>), BoundedReadError> {
    let metadata = std::fs::metadata(path).map_err(BoundedReadError::Io)?;
    if !metadata.is_file() {
        return Err(BoundedReadError::NotRegularFile);
    }
    let file = std::fs::File::open(path).map_err(BoundedReadError::Io)?;
    Ok((metadata.len(), file.take(MAX_IMAGE_BYTES as u64 + 1)))
}

/// 经 `open_bounded` 读取文件，最多 `MAX_IMAGE_BYTES + 1` 字节。
fn read_bounded(path: &Path) -> std::result::Result<(u64, Vec<u8>), BoundedReadError> {
    let (file_len, mut file) = open_bounded(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(BoundedReadError::Io)?;
    Ok((file_len, bytes))
}

fn checked_image_ref(actual_len: u64, bytes: &[u8], existing: &[ImageRef]) -> Result<ImageRef> {
    let ordinal = existing.len().saturating_add(1);
    let image = parse_image_ref(ordinal, actual_len, bytes)?;
    let parts = existing
        .iter()
        .copied()
        .chain(std::iter::once(image))
        .map(|image| MessageSemantic::<BoundToolCall>::Image { image })
        .collect();
    normalize_user_message(parts)
        .map_err(|error| LocalError::new(LocalErrorKind::BadRequest, &error.to_string()))?;
    Ok(image)
}

fn parse_image_ref(ordinal: usize, file_len: u64, bytes: &[u8]) -> Result<ImageRef> {
    ImageRef::from_bytes(bytes).map_err(|err| {
        let user_err = match err {
            ImageBytesError::InvalidLength { .. } => UserInputError::InvalidImageLength {
                image: ordinal,
                actual: file_len,
            },
            ImageBytesError::UnsupportedFormat => {
                UserInputError::UnsupportedImageFormat { image: ordinal }
            }
        };
        LocalError::new(LocalErrorKind::BadRequest, &user_err.to_string())
    })
}

/// 读取第 `ordinal` 张图片（从 1 起）：须为常规文件，最多读取 `MAX_IMAGE_BYTES + 1` 字节，
/// 由 `ImageRef::from_bytes` 核验长度、魔数并计算摘要。
fn read_image(ordinal: usize, path: &Path) -> Result<(ImageRef, Vec<u8>)> {
    let (file_len, bytes) = read_bounded(path).map_err(|err| match err {
        BoundedReadError::NotRegularFile => LocalError::new(
            LocalErrorKind::BadRequest,
            &format!("第 {ordinal} 张图片不是常规文件"),
        ),
        BoundedReadError::Io(error) => LocalError::new(
            LocalErrorKind::BadRequest,
            &format!("无法读取第 {ordinal} 张图片：{error}"),
        ),
    })?;
    let image = parse_image_ref(ordinal, file_len, &bytes)?;
    Ok((image, bytes))
}
