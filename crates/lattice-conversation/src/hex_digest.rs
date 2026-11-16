// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 固定 32 字节摘要的小写十六进制 serde 编解码：线上与持久层为 64 位小写十六进制字符串，内存中为 `[u8; 32]`。

use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

pub fn serialize<S: Serializer>(digest: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in digest.iter() {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    serializer.serialize_str(&out)
}

pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 32], D::Error> {
    let raw = String::deserialize(deserializer)?;
    let bytes = raw.as_bytes();
    if bytes.len() != 64 {
        return Err(D::Error::custom(
            "digest must be 64 lowercase hex characters",
        ));
    }
    let mut digest = [0u8; 32];
    for (index, pair) in bytes.chunks_exact(2).enumerate() {
        let hi = hex_value(pair[0]).ok_or_else(|| D::Error::custom("non-hex digest"))?;
        let lo = hex_value(pair[1]).ok_or_else(|| D::Error::custom("non-hex digest"))?;
        digest[index] = (hi << 4) | lo;
    }
    Ok(digest)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}
