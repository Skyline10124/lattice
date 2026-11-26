// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 平台无关的 IM 身份键（发送者、对话与入站事件去重键）以及会话/轮次身份生成辅助。

use std::fmt;

use lattice_ipc::{Channel, SessionId, TurnId};
use serde::{Deserialize, Deserializer, Serialize, de};

/// 验证 `<platform>:<id>` 形式的身份键字符串。
fn validate_prefixed_key(raw: &str) -> Result<(), &'static str> {
    let Some((prefix, value)) = raw.split_once(':') else {
        return Err("身份键缺少平台前缀分隔符 ':'");
    };
    if prefix.is_empty()
        || !prefix
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return Err("身份键平台前缀必须是非空小写 ASCII 字母或数字");
    }
    if value.is_empty()
        || value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err("身份键主体不能为空且不得包含空白或控制字符");
    }
    Ok(())
}

macro_rules! define_identity_key {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub(crate) struct $name(String);

        impl $name {
            /// 由平台模块用已校验的平台前缀与标识构造身份键。
            pub(crate) fn from_platform_id(prefix: &str, value: &str) -> Self {
                let raw = format!("{prefix}:{value}");
                debug_assert!(validate_prefixed_key(&raw).is_ok(), "内部构造的身份键必须合法");
                Self(raw)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(deserializer)?;
                validate_prefixed_key(&raw).map_err(de::Error::custom)?;
                Ok(Self(raw))
            }
        }
    };
}

define_identity_key!(
    /// 发送者身份键（例如 `tg:12345`），仅用于白名单判定。
    SenderKey
);

define_identity_key!(
    /// 对话身份键（例如 `tg:12345`），用于会话绑定、回复目标、草稿节流与同对话准入串行化。
    ChatKey
);

impl ChatKey {
    /// 剥离指定平台前缀并返回主体切片。
    pub(crate) fn strip_platform<'a>(&'a self, prefix: &str) -> Option<&'a str> {
        let (found_prefix, rest) = self.0.split_once(':')?;
        (found_prefix == prefix).then_some(rest)
    }
}

/// 将 Telegram 对话身份键映射为通道标识 `im/telegram:<chat_id>`。
pub(crate) fn telegram_channel(chat: &ChatKey) -> Channel {
    let chat_id = chat
        .strip_platform("tg")
        .expect("Telegram 对话身份键必须以 tg: 开头");
    Channel::parse(&format!("im/telegram:{chat_id}"))
        .expect("Telegram 通道标识 im/telegram:<chat_id> 必须合法")
}

define_identity_key!(
    /// 入站事件唯一身份键（例如 `tg:98765`），用于台账幂等去重与崩溃找回。
    InboundId
);

impl InboundId {
    /// 剥离指定平台前缀并返回主体切片。
    pub(crate) fn strip_platform<'a>(&'a self, prefix: &str) -> Option<&'a str> {
        let (found_prefix, rest) = self.0.split_once(':')?;
        (found_prefix == prefix).then_some(rest)
    }
}

/// 生成随机的 32 位小写十六进制会话身份。
pub(crate) fn new_session_id() -> SessionId {
    uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .parse()
        .expect("UUID v4 无连字符格式必为合法 SessionId")
}

/// 生成随机的 36 位带连字符轮次身份。
pub(crate) fn new_turn_id() -> TurnId {
    uuid::Uuid::new_v4()
        .to_string()
        .parse()
        .expect("UUID v4 标准格式必为合法 TurnId")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_and_validates_prefixed_identity_keys() {
        let sender = SenderKey::from_platform_id("tg", "42");
        let chat = ChatKey::from_platform_id("tg", "-10099");
        let inbound = InboundId::from_platform_id("tg", "777");

        assert_eq!(sender.to_string(), "tg:42");
        assert_eq!(chat.strip_platform("tg"), Some("-10099"));
        assert_eq!(chat.strip_platform("other"), None);
        assert_eq!(serde_json::to_string(&inbound).unwrap(), "\"tg:777\"");

        let parsed: InboundId = serde_json::from_str("\"tg:777\"").expect("合法身份键应反序列化");
        assert_eq!(parsed, inbound);

        for invalid in [
            "\"12345\"",
            "\"tg:\"",
            "\":123\"",
            "\"TG:123\"",
            "\"tg:12 3\"",
        ] {
            assert!(
                serde_json::from_str::<ChatKey>(invalid).is_err(),
                "非法身份键应被拒绝: {invalid}"
            );
        }
    }

    #[test]
    fn generates_valid_session_and_turn_ids() {
        let s1 = new_session_id();
        let s2 = new_session_id();
        assert_ne!(s1, s2);
        assert_eq!(s1.to_string().len(), 32);

        let t1 = new_turn_id();
        let t2 = new_turn_id();
        assert_ne!(t1, t2);
        assert_eq!(t1.to_string().len(), 36);
    }

    #[test]
    fn formats_telegram_channel_from_chat_key() {
        let positive = ChatKey::from_platform_id("tg", "7001");
        let negative = ChatKey::from_platform_id("tg", "-10099");
        assert_eq!(telegram_channel(&positive).as_str(), "im/telegram:7001");
        assert_eq!(telegram_channel(&negative).as_str(), "im/telegram:-10099");
    }
}
