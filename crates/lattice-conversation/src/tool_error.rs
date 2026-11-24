// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 工具执行器返回的结构化失败载荷。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{MAX_TOOL_CONTENT_BYTES, canonical_json};

/// 工具执行器返回的结构化错误。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolError {
    pub code: String,
    pub message: String,
}

/// 按 [`MAX_TOOL_CONTENT_BYTES`]（65,536 字节）规范 JSON 字符串编码上限，返回 `message` 最长的 UTF-8 字符边界前缀。
///
/// 返回切片 `prefix` 满足 `canonical_json(&Value::String(prefix.to_owned())).len() <= MAX_TOOL_CONTENT_BYTES`，
/// 供 `lattice-llm` 协议投影与 `lattice-context` 统计共同作为唯一裁剪标尺。
pub fn bound_tool_error_message(message: &str) -> &str {
    // 任意 UTF-8 字符串经 JSON 字符串编码后至少为 `len + 2` 字节（含两端引号）；
    // 仅在原文长度本身不大于上限时整段核验，避免对超长异常输入整段分配克隆。
    if message.len() <= MAX_TOOL_CONTENT_BYTES
        && canonical_json(&Value::String(message.to_owned())).len() <= MAX_TOOL_CONTENT_BYTES
    {
        return message;
    }

    let max_raw_len = message.len().min(MAX_TOOL_CONTENT_BYTES);
    let mut boundaries = Vec::new();
    boundaries.push(0usize);
    for (index, ch) in message.char_indices() {
        let end = index + ch.len_utf8();
        if end > max_raw_len {
            break;
        }
        boundaries.push(end);
    }

    let count = boundaries.partition_point(|&end| {
        canonical_json(&Value::String(message[..end].to_owned())).len() <= MAX_TOOL_CONTENT_BYTES
    });
    let best_end = boundaries[count.saturating_sub(1)];
    &message[..best_end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ResultPayload;
    use serde_json::json;

    #[test]
    fn tool_error_roundtrip_and_unknown_field_rejection() {
        let err = ToolError {
            code: "execution_failed".into(),
            message: "读取失败".into(),
        };
        let encoded = serde_json::to_value(&err).expect("序列化 ToolError");
        assert_eq!(
            encoded,
            json!({
                "code": "execution_failed",
                "message": "读取失败",
            })
        );
        let decoded: ToolError = serde_json::from_value(encoded).expect("反序列化 ToolError");
        assert_eq!(decoded, err);

        let unknown = json!({
            "code": "execution_failed",
            "message": "读取失败",
            "extra": true,
        });
        assert!(
            serde_json::from_value::<ToolError>(unknown).is_err(),
            "未知字段应被拒绝"
        );
    }

    #[test]
    fn result_payload_failed_keeps_adjacent_tag_shape() {
        let payload = ResultPayload::Failed(ToolError {
            code: "permission_denied".into(),
            message: "无权访问路径".into(),
        });
        let encoded = serde_json::to_value(&payload).expect("序列化 ResultPayload::Failed");
        assert_eq!(
            encoded,
            json!({
                "outcome": "failed",
                "payload": {
                    "code": "permission_denied",
                    "message": "无权访问路径",
                }
            })
        );
        let decoded: ResultPayload =
            serde_json::from_value(encoded).expect("反序列化 ResultPayload::Failed");
        assert_eq!(decoded, payload);
    }

    #[test]
    fn bound_tool_error_message_enforces_canonical_json_limit_on_utf8_boundaries() {
        let exact_ascii = "a".repeat(MAX_TOOL_CONTENT_BYTES - 2);
        let over_ascii = "a".repeat(MAX_TOOL_CONTENT_BYTES - 1);
        let over_cjk = "界".repeat(22_000);
        let expected_cjk = "界".repeat(21_844);
        let over_newlines = "\n".repeat(40_000);
        let expected_newlines = "\n".repeat(32_767);
        let over_escape = "\x1b".repeat(12_000);
        let expected_escape = "\x1b".repeat(10_922);

        // 1) 空串与短消息零截断直通（返回原切片）
        for short in ["", "读取文件失败：权限不足"] {
            let out = bound_tool_error_message(short);
            assert_eq!(out, short, "未超限消息应原样返回");
            assert_eq!(
                out.as_ptr(),
                short.as_ptr(),
                "未超限消息热路径应直接返回原切片"
            );
        }

        // 2～4) 纯 ASCII 精确边界、多字节中文边界与高转义膨胀边界
        let cases: &[(&str, &str, &str)] = &[
            ("纯 ASCII 恰好命中 65,536", &exact_ascii, &exact_ascii),
            ("纯 ASCII 超出 1 字节", &over_ascii, &exact_ascii),
            ("多字节中文边界", &over_cjk, &expected_cjk),
            ("换行转义膨胀边界", &over_newlines, &expected_newlines),
            (
                "控制字符 6 字节转义膨胀边界",
                &over_escape,
                &expected_escape,
            ),
        ];

        for &(label, input, expected) in cases {
            let out = bound_tool_error_message(input);
            assert_eq!(out, expected, "{label} 裁剪结果不匹配");

            // 5) 通用不变量：裁剪结果满足上限，且若发生裁剪则下一个字符边界必然超限
            let out_encoded_len = canonical_json(&Value::String(out.to_owned())).len();
            assert!(
                out_encoded_len <= MAX_TOOL_CONTENT_BYTES,
                "{label} 裁剪后规范 JSON 长度 {out_encoded_len} 不得超过 {MAX_TOOL_CONTENT_BYTES}"
            );
            if out.len() < input.len() {
                let next_char = input[out.len()..]
                    .chars()
                    .next()
                    .expect("被裁剪字符串必存在下一个 UTF-8 字符");
                let next_end = out.len() + next_char.len_utf8();
                let next_encoded_len =
                    canonical_json(&Value::String(input[..next_end].to_owned())).len();
                assert!(
                    next_encoded_len > MAX_TOOL_CONTENT_BYTES,
                    "{label} 下一个字符边界规范 JSON 长度 {next_encoded_len} 必须大于 {MAX_TOOL_CONTENT_BYTES}"
                );
            }
        }
    }
}
