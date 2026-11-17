// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 面向人阅读的图片占位标记格式化，供 TUI 与 CLI 会话展示共用。

use std::num::NonZeroUsize;

use lattice_conversation::ImageRef;

/// 将单条消息内的图片引用格式化为 `[IMAGE n · <ext> · <size>]`。
pub fn format_image_marker(ordinal: NonZeroUsize, image: &ImageRef) -> String {
    let size = format_binary_size(image.byte_length);
    format!(
        "[IMAGE {} \u{00b7} {} \u{00b7} {}]",
        ordinal.get(),
        image.media_type.extension(),
        size
    )
}

fn format_binary_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let n = u128::from(bytes);
    let kib = (n + 512) / 1024;
    if kib < 1024 {
        return format!("{kib} KiB");
    }
    let tenths_mib = (n * 10 + 524_288) / 1_048_576;
    format!("{}.{} MiB", tenths_mib / 10, tenths_mib % 10)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_conversation::ImageMediaType;

    fn img(media_type: ImageMediaType, byte_length: u64) -> ImageRef {
        ImageRef {
            sha256: [0xab; 32],
            media_type,
            byte_length,
        }
    }

    #[test]
    fn format_image_marker_locks_size_rounding_thresholds_and_extensions() {
        let one = NonZeroUsize::new(1).unwrap();
        let cases: &[(u64, &str)] = &[
            (1023, "[IMAGE 1 · png · 1023 B]"),
            (1024, "[IMAGE 1 · png · 1 KiB]"),
            (1535, "[IMAGE 1 · png · 1 KiB]"),
            (1536, "[IMAGE 1 · png · 2 KiB]"),
            (1_048_063, "[IMAGE 1 · png · 1023 KiB]"),
            (1_048_064, "[IMAGE 1 · png · 1.0 MiB]"),
            (1_048_576, "[IMAGE 1 · png · 1.0 MiB]"),
            (3_932_160, "[IMAGE 1 · png · 3.8 MiB]"),
        ];

        for &(byte_length, expected) in cases {
            let actual = format_image_marker(one, &img(ImageMediaType::Png, byte_length));
            assert_eq!(
                actual, expected,
                "字节长度 {byte_length} 的图片标记格式化结果不符"
            );
        }

        let two = NonZeroUsize::new(2).unwrap();
        assert_eq!(
            format_image_marker(two, &img(ImageMediaType::Jpeg, 2048)),
            "[IMAGE 2 · jpg · 2 KiB]",
            "JPEG 图片标记应显示 jpg 扩展名标签"
        );
        assert_eq!(
            format_image_marker(one, &img(ImageMediaType::Webp, 512)),
            "[IMAGE 1 · webp · 512 B]",
            "WebP 图片标记应显示 webp 扩展名标签"
        );
        assert_eq!(
            format_image_marker(one, &img(ImageMediaType::Gif, 512)),
            "[IMAGE 1 · gif · 512 B]",
            "GIF 图片标记应显示 gif 扩展名标签"
        );
    }
}
