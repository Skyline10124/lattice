// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 图片文件的有界读取、核验与重读比对，供 CLI 与 TUI 共用。

use std::{io::Read, path::Path};

use lattice_conversation::{ImageBytesError, ImageRef, MAX_IMAGE_BYTES, UserInputError};
use lattice_ipc::{LocalError, LocalErrorKind, Result};

/// 读取并核验第 `ordinal` 张图片文件（从 1 起），返回其 `ImageRef`；读到的字节随即丢弃。
pub fn image_file_ref(ordinal: usize, path: &Path) -> Result<ImageRef> {
    let (image, _bytes) = read_image(ordinal, path)?;
    Ok(image)
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

/// 读取第 `ordinal` 张图片（从 1 起）：须为常规文件，最多读取 `MAX_IMAGE_BYTES + 1` 字节，
/// 由 `ImageRef::from_bytes` 核验长度、魔数并计算摘要。
fn read_image(ordinal: usize, path: &Path) -> Result<(ImageRef, Vec<u8>)> {
    let read_err = |error: std::io::Error| {
        LocalError::new(
            LocalErrorKind::BadRequest,
            &format!("无法读取第 {ordinal} 张图片：{error}"),
        )
    };
    let metadata = std::fs::metadata(path).map_err(read_err)?;
    if !metadata.is_file() {
        return Err(LocalError::new(
            LocalErrorKind::BadRequest,
            &format!("第 {ordinal} 张图片不是常规文件"),
        ));
    }
    let file = std::fs::File::open(path).map_err(read_err)?;
    let mut bytes = Vec::new();
    file.take(MAX_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(read_err)?;
    let image = ImageRef::from_bytes(&bytes).map_err(|err| {
        let user_err = match err {
            ImageBytesError::InvalidLength { .. } => UserInputError::InvalidImageLength {
                image: ordinal,
                actual: metadata.len(),
            },
            ImageBytesError::UnsupportedFormat => {
                UserInputError::UnsupportedImageFormat { image: ordinal }
            }
        };
        LocalError::new(LocalErrorKind::BadRequest, &user_err.to_string())
    })?;
    Ok((image, bytes))
}
