// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 模型请求编码阶段使用的只读图片来源接缝与默认无图实现。

use lattice_conversation::ImageRef;

use crate::LlmError;

/// 请求编码时的只读图片来源。
pub trait ImageSource: Send + Sync {
    /// 打开一个读取作用域并恰好调用一次 `encode`；作用域内借出的图片字节在返回前全部释放。
    fn with_reader(
        &self,
        encode: &mut dyn FnMut(&dyn ImageReader) -> Result<(), LlmError>,
    ) -> Result<(), LlmError>;
}

/// 单次读取作用域内的原始图片字节读取器。
pub trait ImageReader {
    /// 借出 `image` 对应的原始字节并恰好调用一次 `consume`；借出的字节不能逃逸出这次调用。
    fn with_image(
        &self,
        image: &ImageRef,
        consume: &mut dyn FnMut(&[u8]) -> Result<(), LlmError>,
    ) -> Result<(), LlmError>;
}

/// 不持有任何图片字节的默认图片来源；进入读取作用域后拒绝所有图片读取请求。
#[derive(Clone, Copy, Debug, Default)]
pub struct NoImages;

struct NoImagesReader;

impl ImageSource for NoImages {
    fn with_reader(
        &self,
        encode: &mut dyn FnMut(&dyn ImageReader) -> Result<(), LlmError>,
    ) -> Result<(), LlmError> {
        encode(&NoImagesReader)
    }
}

impl ImageReader for NoImagesReader {
    fn with_image(
        &self,
        image: &ImageRef,
        _consume: &mut dyn FnMut(&[u8]) -> Result<(), LlmError>,
    ) -> Result<(), LlmError> {
        Err(LlmError::Other(format!(
            "当前图片来源无法读取图片 {}",
            short_image_digest(&image.sha256)
        )))
    }
}

pub(crate) fn no_images_reader() -> &'static dyn ImageReader {
    &NoImagesReader
}

/// 提取图片 SHA-256 摘要的前 12 位小写十六进制字符，用于不含原始字节内容的诊断消息。
pub(crate) fn short_image_digest(sha256: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(12);
    for &byte in &sha256[..6] {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}
