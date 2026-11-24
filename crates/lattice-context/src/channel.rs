// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 上下文来源标识 `Channel` 及其严格语法校验（ADR 0014 第 2 节）。

use std::borrow::{Borrow, Cow};
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

/// 上下文来源标识 `Channel` 的最大 UTF-8 字节长度。
pub const MAX_CHANNEL_BYTES: usize = 256;

/// 解析或反序列化 [`Channel`] 失败的原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum ChannelError {
    /// 来源标识为空字符串。
    #[error("上下文来源 Channel 不能为空")]
    Empty,
    /// 来源标识超过最大 UTF-8 字节上限。
    #[error(
        "上下文来源 Channel 为 {actual} 字节（按 UTF-8 计），不能超过 {MAX_CHANNEL_BYTES} 字节"
    )]
    TooLong {
        /// 实际 UTF-8 字节长度。
        actual: usize,
    },
    /// 来源标识包含控制字符或 Unicode 行分隔符。
    #[error("上下文来源 Channel 不能包含控制字符或换行")]
    ControlCharacter,
    /// 来源标识首尾包含空白字符。
    #[error("上下文来源 Channel 首尾不能包含空白字符")]
    SurroundingWhitespace,
}

/// 开放受检的上下文来源标识（例如 `tui`、`tui/comment`、`cli`、`im/telegram:<chat_id>`、`screenshot`）。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Channel(String);

impl Channel {
    /// 严格核验并构造 [`Channel`]：非空、UTF-8 字节长度 `<= 256`、不含控制字符与换行、首尾无空白字符。
    pub fn parse(value: &str) -> Result<Self, ChannelError> {
        if value.is_empty() {
            return Err(ChannelError::Empty);
        }
        if value.len() > MAX_CHANNEL_BYTES {
            return Err(ChannelError::TooLong {
                actual: value.len(),
            });
        }
        if value
            .chars()
            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
        {
            return Err(ChannelError::ControlCharacter);
        }
        if value.chars().next().is_some_and(char::is_whitespace)
            || value.chars().next_back().is_some_and(char::is_whitespace)
        {
            return Err(ChannelError::SurroundingWhitespace);
        }
        Ok(Self(value.to_owned()))
    }

    /// 返回来源标识原始字符串切片。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Channel {
    type Err = ChannelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Borrow<str> for Channel {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for Channel {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq<str> for Channel {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for Channel {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl<'de> Deserialize<'de> for Channel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = <Cow<'de, str>>::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}
