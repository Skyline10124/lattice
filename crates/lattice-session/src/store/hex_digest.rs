// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 固定 32 字节摘要的规范 serde：64 个小写十六进制字符。

use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

pub(super) fn encode_hex(digest: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in digest.iter() {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

pub fn serialize<S: Serializer>(digest: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&encode_hex(digest))
}

pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 32], D::Error> {
    let raw = String::deserialize(deserializer)?;
    decode_hex(&raw).ok_or_else(|| D::Error::custom("digest must be 64 lowercase hex characters"))
}

pub(super) fn decode_hex(raw: &str) -> Option<[u8; 32]> {
    let bytes = raw.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let hex_value = |byte: u8| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    };
    let mut digest = [0u8; 32];
    for (index, pair) in bytes.chunks_exact(2).enumerate() {
        digest[index] = (hex_value(pair[0])? << 4) | hex_value(pair[1])?;
    }
    Some(digest)
}
