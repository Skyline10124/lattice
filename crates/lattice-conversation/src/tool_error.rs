// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 工具执行器返回的结构化失败载荷。

use serde::{Deserialize, Serialize};

/// 工具执行器返回的结构化错误。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolError {
    pub code: String,
    pub message: String,
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
}
