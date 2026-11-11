// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 调用槽位的持久绑定事实。

use lattice_conversation::{InvocationId, ModelMessageAddress, SessionId, ToolCallAddress, TurnId};
use serde::{Deserialize, Serialize};

/// 已登记工具调用槽位的不可变非秘密绑定身份。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingIdentity {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub model_step_ordinal: u32,
    pub part_ordinal: u32,
    /// 模型选择的工具名。
    pub method: String,
    /// 来自本轮冻结工具目录的不透明工具身份。
    pub tool_identity: String,
    /// 完整工具参数对象的规范摘要。
    #[serde(with = "super::hex_digest")]
    pub arguments_digest: [u8; 32],
}

impl BindingIdentity {
    /// 还原规范的结构化 `ToolCallAddress`。
    #[must_use]
    pub fn address(&self) -> ToolCallAddress {
        ToolCallAddress {
            message: ModelMessageAddress {
                session_id: self.session_id,
                turn_id: self.turn_id,
                model_step_ordinal: self.model_step_ordinal,
            },
            part_ordinal: self.part_ordinal,
        }
    }
}

/// 已登记调用槽位的完整身份与绑定事实。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationBinding {
    /// 调用槽位唯一身份。
    pub invocation_id: InvocationId,
    /// 与持久调用槽位完全一致的绑定事实。
    pub identity: BindingIdentity,
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_conversation::arguments_digest;
    use serde_json::json;

    fn binding() -> InvocationBinding {
        let arguments = json!({"path": "a.txt"});
        InvocationBinding {
            invocation_id: InvocationId::parse("11111111-1111-4111-8111-111111111111")
                .expect("解析 InvocationId"),
            identity: BindingIdentity {
                session_id: SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c")
                    .expect("解析 SessionId"),
                turn_id: TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c")
                    .expect("解析 TurnId"),
                model_step_ordinal: 2,
                part_ordinal: 1,
                method: "observe__workspace".to_string(),
                tool_identity: "lancet-node:observe@v1".to_string(),
                arguments_digest: arguments_digest(&arguments),
            },
        }
    }

    #[test]
    fn binding_round_trips_and_rejects_unknown_fields() {
        let binding = binding();
        let encoded = serde_json::to_string(&binding).expect("序列化 InvocationBinding");
        let decoded: InvocationBinding =
            serde_json::from_str(&encoded).expect("反序列化 InvocationBinding");
        assert_eq!(decoded, binding);
        assert_eq!(
            binding.identity.address(),
            ToolCallAddress {
                message: ModelMessageAddress {
                    session_id: binding.identity.session_id,
                    turn_id: binding.identity.turn_id,
                    model_step_ordinal: 2,
                },
                part_ordinal: 1,
            }
        );

        let mut with_unknown = serde_json::to_value(&binding).expect("转成 Value");
        with_unknown["extra"] = json!(true);
        assert!(
            serde_json::from_value::<InvocationBinding>(with_unknown).is_err(),
            "未知字段必须被拒绝"
        );
    }
}
