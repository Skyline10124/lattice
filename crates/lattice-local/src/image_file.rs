// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 图片字节与文件的分块摘要核验、粘贴探测与重读比对，供 CLI 与 TUI 共用。

use std::{io::Read, path::Path};

use lattice_conversation::{
    BoundToolCall, ImageBytesError, ImageMediaType, ImageRef, ImageRefHasher, MAX_IMAGE_BYTES,
    MessageSemantic, UserInputError, normalize_user_message,
};
use lattice_ipc::{LocalError, LocalErrorKind, Result};

/// 分块计算文件摘要时每次读取的固定栈缓冲大小（字节）。
const HASH_CHUNK_BYTES: usize = 8192;
/// 探测受支持图片格式所需的文件头最大字节数。
const HEADER_SNIFF_BYTES: usize = 12;

/// 按固定小块流式读取并核验第 `ordinal` 张图片文件（从 1 起），返回其 `ImageRef`，不持有整张图片字节。
pub fn image_file_ref(ordinal: usize, path: &Path) -> Result<ImageRef> {
    let (file_len, mut file) =
        open_bounded(path).map_err(|err| map_bounded_read_error(ordinal, err))?;
    let image_result = finish_chunked_hash(&mut file, ImageRefHasher::new())
        .map_err(|error| map_bounded_read_error(ordinal, BoundedReadError::Io(error)))?;
    map_image_ref_result(ordinal, file_len, image_result)
}

/// 核验内存中的图片字节，并连同 `existing` 检查单轮张数与总字节上限（图片序号为 `existing.len() + 1`）。
pub fn image_bytes_ref(bytes: &[u8], existing: &[ImageRef]) -> Result<ImageRef> {
    let ordinal = existing.len().saturating_add(1);
    let actual_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let image = map_image_ref_result(ordinal, actual_len, ImageRef::from_bytes(bytes))?;
    check_image_turn_limits(image, existing)?;
    Ok(image)
}

/// 判断路径是否指向受支持的图片，并连同 `existing` 核验单轮张数与总字节上限：
/// 不存在、不是常规文件、读取失败或魔数不是 PNG/JPEG/WebP/GIF 时返回 `Ok(None)`；
/// 单张长度不合规或加入后超过单轮张数、总量上限时返回 `BadRequest`（说明取 `UserInputError`，图片序号为 `existing.len() + 1`）。
pub fn probe_image_file(path: &Path, existing: &[ImageRef]) -> Result<Option<ImageRef>> {
    let Ok((file_len, mut file)) = open_bounded(path) else {
        return Ok(None);
    };
    // 先只读文件头判断是不是图片，粘贴普通大文件的路径时不必继续读取。
    let mut header = [0u8; HEADER_SNIFF_BYTES];
    let Ok(header_len) = read_up_to(&mut file, &mut header) else {
        return Ok(None);
    };
    if ImageMediaType::sniff(&header[..header_len]).is_none() {
        return Ok(None);
    }
    let mut hasher = ImageRefHasher::new();
    let image_result = match hasher.update(&header[..header_len]) {
        Ok(()) => match finish_chunked_hash(&mut file, hasher) {
            Ok(result) => result,
            Err(_) => return Ok(None),
        },
        Err(err) => Err(err),
    };
    let ordinal = existing.len().saturating_add(1);
    let image = map_image_ref_result(ordinal, file_len, image_result)?;
    check_image_turn_limits(image, existing)?;
    Ok(Some(image))
}

/// 重读第 `ordinal` 张图片，与 `expected` 完全一致时返回字节；读取失败保留原因，
/// 读到不同的图片时报告“第 {ordinal} 张图片在预检后已改变，请重新提交”。
pub fn read_verified_image(ordinal: usize, path: &Path, expected: &ImageRef) -> Result<Vec<u8>> {
    let (file_len, bytes) =
        read_bounded(path).map_err(|err| map_bounded_read_error(ordinal, err))?;
    let actual = map_image_ref_result(ordinal, file_len, ImageRef::from_bytes(&bytes))?;
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

/// 从 `reader` 最多读取 `buf.len()` 字节，遇到短读或中断继续，直至填满或到达 EOF。
fn read_up_to(reader: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(filled)
}

/// 按固定栈缓冲分块读取 `reader` 并喂入 `hasher`，越过上限或到达 EOF 时立即返回，不持有整张图片字节。
fn finish_chunked_hash(
    reader: &mut impl Read,
    mut hasher: ImageRefHasher,
) -> std::io::Result<std::result::Result<ImageRef, ImageBytesError>> {
    let mut buf = [0u8; HASH_CHUNK_BYTES];
    loop {
        let read = match reader.read(&mut buf) {
            Ok(0) => return Ok(hasher.finish()),
            Ok(n) => n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        if let Err(err) = hasher.update(&buf[..read]) {
            return Ok(Err(err));
        }
    }
}

fn check_image_turn_limits(image: ImageRef, existing: &[ImageRef]) -> Result<()> {
    let parts = existing
        .iter()
        .copied()
        .chain(std::iter::once(image))
        .map(|image| MessageSemantic::<BoundToolCall>::Image { image })
        .collect();
    normalize_user_message(parts)
        .map_err(|error| LocalError::new(LocalErrorKind::BadRequest, &error.to_string()))?;
    Ok(())
}

fn map_image_ref_result(
    ordinal: usize,
    file_len: u64,
    result: std::result::Result<ImageRef, ImageBytesError>,
) -> Result<ImageRef> {
    result.map_err(|err| {
        let user_err = match err {
            ImageBytesError::InvalidLength { actual } => UserInputError::InvalidImageLength {
                image: ordinal,
                actual: if actual == 0 { 0 } else { file_len.max(actual) },
            },
            ImageBytesError::UnsupportedFormat => {
                UserInputError::UnsupportedImageFormat { image: ordinal }
            }
        };
        LocalError::new(LocalErrorKind::BadRequest, &user_err.to_string())
    })
}

fn map_bounded_read_error(ordinal: usize, err: BoundedReadError) -> LocalError {
    match err {
        BoundedReadError::NotRegularFile => LocalError::new(
            LocalErrorKind::BadRequest,
            &format!("第 {ordinal} 张图片不是常规文件"),
        ),
        BoundedReadError::Io(error) => LocalError::new(
            LocalErrorKind::BadRequest,
            &format!("无法读取第 {ordinal} 张图片：{error}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};

    struct ShortInterruptingReader<'a> {
        data: &'a [u8],
        pos: usize,
        max_step: usize,
        interrupt_next: bool,
    }

    impl<'a> ShortInterruptingReader<'a> {
        fn new(data: &'a [u8], max_step: usize) -> Self {
            Self {
                data,
                pos: 0,
                max_step,
                interrupt_next: true,
            }
        }
    }

    impl Read for ShortInterruptingReader<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.interrupt_next {
                self.interrupt_next = false;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "模拟信号中断",
                ));
            }
            self.interrupt_next = true;
            if self.pos >= self.data.len() {
                return Ok(0);
            }
            let take = buf.len().min(self.max_step).min(self.data.len() - self.pos);
            buf[..take].copy_from_slice(&self.data[self.pos..self.pos + take]);
            self.pos += take;
            Ok(take)
        }
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path = PathBuf::from("/tmp").join(format!(
                "ltc-img-file-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            fs::create_dir_all(&path).expect("创建图片测试临时目录");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn chunked_reader_handles_short_reads_interrupted_and_stops_when_limit_exceeded() {
        let mut png = vec![0x3Cu8; HASH_CHUNK_BYTES * 3 + 17];
        png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        let expected = ImageRef::from_bytes(&png).expect("构造预期 PNG 引用");

        // 模拟每次仅返回 3 字节且穿插 Interrupted 的读取流，核对头读取与剩余分块摘要。
        let mut reader = ShortInterruptingReader::new(&png, 3);
        let mut header = [0u8; HEADER_SNIFF_BYTES];
        let header_len = read_up_to(&mut reader, &mut header).expect("应能跨短读填满文件头");
        assert_eq!(header_len, HEADER_SNIFF_BYTES);
        assert_eq!(ImageMediaType::sniff(&header), Some(ImageMediaType::Png));

        let mut hasher = ImageRefHasher::new();
        hasher.update(&header).expect("12 字节头不应超限");
        let actual = finish_chunked_hash(&mut reader, hasher)
            .expect("分块读取不应报 IO 错误")
            .expect("分块计算应返回合法 ImageRef");
        assert_eq!(actual, expected, "分块摘要必须与整块 from_bytes 一致");
        assert_eq!(reader.pos, png.len());

        // 超过单图上限的流应在越限的当块立刻停止读取，不把尾部全部读完。
        let mut huge = vec![0x55u8; MAX_IMAGE_BYTES + HASH_CHUNK_BYTES * 8];
        huge[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        let mut huge_reader = ShortInterruptingReader::new(&huge, HASH_CHUNK_BYTES);
        let err = finish_chunked_hash(&mut huge_reader, ImageRefHasher::new())
            .expect("越限应由摘要器返回业务错误而非 IO 错误")
            .expect_err("超过 MAX_IMAGE_BYTES 必须拒绝");
        assert!(
            matches!(err, ImageBytesError::InvalidLength { actual } if actual > MAX_IMAGE_BYTES as u64),
            "越限错误应为 InvalidLength：{err:?}"
        );
        assert!(
            huge_reader.pos <= MAX_IMAGE_BYTES + HASH_CHUNK_BYTES,
            "越限后必须立刻停读，实际读取了 {} 字节（流总长 {}）",
            huge_reader.pos,
            huge.len()
        );
    }

    #[test]
    fn image_file_ref_and_probe_image_file_match_from_bytes_and_report_limits() {
        let dir = TempDir::new();

        // 跨多个 8 KiB 分块的合法 PNG。
        let mut multi_chunk_png = vec![0x7Au8; HASH_CHUNK_BYTES * 2 + 513];
        multi_chunk_png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        let png_path = dir.0.join("multi.png");
        fs::write(&png_path, &multi_chunk_png).unwrap();
        let expected_png = ImageRef::from_bytes(&multi_chunk_png).unwrap();
        assert_eq!(image_file_ref(1, &png_path).unwrap(), expected_png);
        assert_eq!(
            probe_image_file(&png_path, &[]).unwrap(),
            Some(expected_png)
        );

        // 短于 12 字节的合法 GIF。
        let gif_bytes = b"GIF89a";
        let gif_path = dir.0.join("tiny.gif");
        fs::write(&gif_path, gif_bytes).unwrap();
        let expected_gif = ImageRef::from_bytes(gif_bytes).unwrap();
        assert_eq!(image_file_ref(2, &gif_path).unwrap(), expected_gif);
        assert_eq!(
            probe_image_file(&gif_path, &[expected_png]).unwrap(),
            Some(expected_gif)
        );

        // 空文件：CLI 报长度无效（0 字节），TUI 探测返回 None 回退为纯文本。
        let empty_path = dir.0.join("empty.png");
        fs::write(&empty_path, b"").unwrap();
        let empty_err = image_file_ref(1, &empty_path).unwrap_err();
        assert_eq!(empty_err.kind, LocalErrorKind::BadRequest);
        assert!(
            empty_err.message.contains("为 0 字节"),
            "{}",
            empty_err.message
        );
        assert_eq!(probe_image_file(&empty_path, &[]).unwrap(), None);

        // 超过 3.75 MiB 的真 PNG：CLI 与 TUI 均报 BadRequest 且报告真实文件字节数。
        let oversized_len = MAX_IMAGE_BYTES + 12_345;
        let mut oversized_png = vec![0x99u8; oversized_len];
        oversized_png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        let oversized_png_path = dir.0.join("oversized.png");
        fs::write(&oversized_png_path, &oversized_png).unwrap();
        let expected_len_str = oversized_len.to_string();
        let cli_over = image_file_ref(1, &oversized_png_path).unwrap_err();
        assert_eq!(cli_over.kind, LocalErrorKind::BadRequest);
        assert!(
            cli_over.message.contains(&expected_len_str),
            "{}",
            cli_over.message
        );
        let tui_over = probe_image_file(&oversized_png_path, &[]).unwrap_err();
        assert_eq!(tui_over.kind, LocalErrorKind::BadRequest);
        assert!(
            tui_over.message.contains(&expected_len_str),
            "{}",
            tui_over.message
        );

        // 超过 3.75 MiB 的非图片文件：TUI 读 12 字节即返回 None；CLI 按超限拒绝。
        let oversized_bin_path = dir.0.join("oversized.bin");
        fs::write(&oversized_bin_path, vec![b'a'; MAX_IMAGE_BYTES + 1024]).unwrap();
        assert_eq!(probe_image_file(&oversized_bin_path, &[]).unwrap(), None);
        let cli_bin = image_file_ref(1, &oversized_bin_path).unwrap_err();
        assert_eq!(cli_bin.kind, LocalErrorKind::BadRequest);
        assert!(cli_bin.message.contains("3,932,160"), "{}", cli_bin.message);
    }
}
