// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 会话树模型与有效 profile 共享的纯字段类型，不依赖存储引擎、编解码或外部配置加载器。

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

mod project_path;

pub use project_path::{ProjectPath, ProjectPathError};

/// Agent 名称的最大 UTF-8 字节数。
pub const MAX_AGENT_NAME_BYTES: usize = 128;

/// 显式 Turn 超时的最大秒数（24 小时）。
pub const MAX_TURN_TIMEOUT_SECS: u64 = 86_400;

/// 单步模型调用尝试预算上限（1 次首试 + 最多 10 次重试）。
pub const MAX_MODEL_ATTEMPTS: u32 = 11;

/// 单次重试退避等待的最大毫秒数。
pub const MAX_RETRY_DELAY_MS: u64 = 60_000;

/// Agent 名称不符合语法规则的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AgentNameError {
    #[error("agent name must not be empty")]
    Empty,
    #[error("agent name must be at most {MAX_AGENT_NAME_BYTES} bytes")]
    TooLong,
    #[error("agent name must not contain control characters")]
    ControlCharacter,
    #[error("agent name must not contain '/'")]
    Slash,
    #[error("agent name must not be '.' or '..'")]
    DotComponent,
}

/// 单个正常路径分量的 Agent 名称；保留原始大小写、空格与合法 Unicode。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct AgentName(String);

impl AgentName {
    pub fn parse(value: &str) -> Result<Self, AgentNameError> {
        if value.is_empty() {
            return Err(AgentNameError::Empty);
        }
        if value.len() > MAX_AGENT_NAME_BYTES {
            return Err(AgentNameError::TooLong);
        }
        if value.chars().any(char::is_control) {
            return Err(AgentNameError::ControlCharacter);
        }
        if value.contains('/') {
            return Err(AgentNameError::Slash);
        }
        if matches!(value, "." | "..") {
            return Err(AgentNameError::DotComponent);
        }
        if value.trim().is_empty() {
            return Err(AgentNameError::Empty);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for AgentName {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for AgentName {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl fmt::Display for AgentName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for AgentName {
    type Err = AgentNameError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl<'de> Deserialize<'de> for AgentName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// Turn 超时秒数越界的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TurnTimeoutError {
    #[error("turn timeout must not be zero")]
    Zero,
    #[error("turn timeout must be at most {MAX_TURN_TIMEOUT_SECS} seconds")]
    TooLarge,
}

/// 有效的 Turn 超时秒数（`1..=86_400`）；未显式配置时统一经 [`Default`] 取 300 秒。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct TurnTimeout(u64);

impl TurnTimeout {
    pub fn new(seconds: u64) -> Result<Self, TurnTimeoutError> {
        if seconds == 0 {
            return Err(TurnTimeoutError::Zero);
        }
        if seconds > MAX_TURN_TIMEOUT_SECS {
            return Err(TurnTimeoutError::TooLarge);
        }
        Ok(Self(seconds))
    }

    pub fn as_secs(self) -> u64 {
        self.0
    }
}

impl Default for TurnTimeout {
    fn default() -> Self {
        Self(300)
    }
}

impl fmt::Display for TurnTimeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<'de> Deserialize<'de> for TurnTimeout {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let seconds = u64::deserialize(deserializer)?;
        Self::new(seconds).map_err(serde::de::Error::custom)
    }
}

/// 模型步骤重试策略不合法的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RetryPolicyError {
    #[error("max_retries must be at most {}", MAX_MODEL_ATTEMPTS - 1)]
    TooManyRetries,
    #[error("initial_delay_ms must be at most {MAX_RETRY_DELAY_MS}")]
    InitialDelayTooLarge,
    #[error("max_delay_ms must be at most {MAX_RETRY_DELAY_MS}")]
    MaxDelayTooLarge,
    #[error("initial_delay_ms must not exceed max_delay_ms")]
    InitialDelayExceedsMax,
}

/// 模型步骤的重试上限与指数退避参数。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct RetryPolicy {
    max_retries: u32,
    initial_delay_ms: u64,
    max_delay_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RetryPolicyWire {
    max_retries: u32,
    initial_delay_ms: u64,
    max_delay_ms: u64,
}

impl RetryPolicy {
    pub fn new(
        max_retries: u32,
        initial_delay_ms: u64,
        max_delay_ms: u64,
    ) -> Result<Self, RetryPolicyError> {
        if max_retries >= MAX_MODEL_ATTEMPTS {
            return Err(RetryPolicyError::TooManyRetries);
        }
        if initial_delay_ms > MAX_RETRY_DELAY_MS {
            return Err(RetryPolicyError::InitialDelayTooLarge);
        }
        if max_delay_ms > MAX_RETRY_DELAY_MS {
            return Err(RetryPolicyError::MaxDelayTooLarge);
        }
        if initial_delay_ms > max_delay_ms {
            return Err(RetryPolicyError::InitialDelayExceedsMax);
        }
        Ok(Self {
            max_retries,
            initial_delay_ms,
            max_delay_ms,
        })
    }

    pub fn max_retries(self) -> u32 {
        self.max_retries
    }

    pub fn initial_delay_ms(self) -> u64 {
        self.initial_delay_ms
    }

    pub fn max_delay_ms(self) -> u64 {
        self.max_delay_ms
    }

    pub fn max_attempts(self) -> u32 {
        self.max_retries + 1
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay_ms: 500,
            max_delay_ms: 3_000,
        }
    }
}

impl<'de> Deserialize<'de> for RetryPolicy {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RetryPolicyWire::deserialize(deserializer)?;
        Self::new(raw.max_retries, raw.initial_delay_ms, raw.max_delay_ms)
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_name_bounds_characters_and_serde() {
        for valid in [
            "a".into(),
            "coder_1-alpha".into(),
            ".hidden".into(),
            "...".into(),
            "my agent".into(),
            "研究员".into(),
            "a".repeat(MAX_AGENT_NAME_BYTES),
            "é".repeat(MAX_AGENT_NAME_BYTES / 2),
        ] {
            let parsed = AgentName::parse(&valid).unwrap();
            assert_eq!(parsed.as_str(), valid);
            assert_eq!(
                serde_json::from_value::<AgentName>(serde_json::to_value(&parsed).unwrap())
                    .unwrap(),
                parsed
            );
        }
        for (invalid, expected) in [
            ("".into(), AgentNameError::Empty),
            ("   ".into(), AgentNameError::Empty),
            (
                "a".repeat(MAX_AGENT_NAME_BYTES + 1),
                AgentNameError::TooLong,
            ),
            (
                format!("{}a", "é".repeat(MAX_AGENT_NAME_BYTES / 2)),
                AgentNameError::TooLong,
            ),
            ("a\0b".into(), AgentNameError::ControlCharacter),
            ("a\nb".into(), AgentNameError::ControlCharacter),
            ("a\u{7f}b".into(), AgentNameError::ControlCharacter),
            ("/outside".into(), AgentNameError::Slash),
            ("a/b".into(), AgentNameError::Slash),
            ("a/".into(), AgentNameError::Slash),
            (".".into(), AgentNameError::DotComponent),
            ("..".into(), AgentNameError::DotComponent),
        ] {
            assert_eq!(AgentName::parse(&invalid), Err(expected), "{invalid:?}");
            assert!(
                serde_json::from_value::<AgentName>(serde_json::json!(invalid)).is_err(),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn turn_timeout_bounds_default_and_serde() {
        assert_eq!(TurnTimeout::default().as_secs(), 300);
        for valid in [1, 300, 3_600, 3_601, MAX_TURN_TIMEOUT_SECS] {
            let parsed = TurnTimeout::new(valid).unwrap();
            assert_eq!(parsed.as_secs(), valid);
            assert_eq!(
                serde_json::from_value::<TurnTimeout>(serde_json::json!(valid)).unwrap(),
                parsed
            );
        }
        for (invalid, expected) in [
            (0, TurnTimeoutError::Zero),
            (MAX_TURN_TIMEOUT_SECS + 1, TurnTimeoutError::TooLarge),
        ] {
            assert_eq!(TurnTimeout::new(invalid), Err(expected), "{invalid}");
            assert!(serde_json::from_value::<TurnTimeout>(serde_json::json!(invalid)).is_err());
        }
    }

    #[test]
    fn retry_policy_bounds_check_order_and_serde() {
        let default = RetryPolicy::default();
        assert_eq!(default.max_retries(), 3);
        assert_eq!(default.initial_delay_ms(), 500);
        assert_eq!(default.max_delay_ms(), 3_000);
        assert_eq!(default.max_attempts(), 4);

        for (retries, initial, max, attempts) in
            [(0, 0, 0, 1), (10, 60_000, 60_000, 11), (1, 500, 3_000, 2)]
        {
            let policy = RetryPolicy::new(retries, initial, max).unwrap();
            assert_eq!(policy.max_attempts(), attempts);
            let wire = serde_json::to_value(policy).unwrap();
            assert_eq!(
                wire,
                serde_json::json!({
                    "max_retries": retries,
                    "initial_delay_ms": initial,
                    "max_delay_ms": max,
                })
            );
            assert_eq!(serde_json::from_value::<RetryPolicy>(wire).unwrap(), policy);
        }

        for (retries, initial, max, expected) in [
            (11, 60_001, 60_001, RetryPolicyError::TooManyRetries),
            (10, 60_001, 60_001, RetryPolicyError::InitialDelayTooLarge),
            (10, 60_000, 60_001, RetryPolicyError::MaxDelayTooLarge),
            (10, 10, 5, RetryPolicyError::InitialDelayExceedsMax),
        ] {
            assert_eq!(
                RetryPolicy::new(retries, initial, max),
                Err(expected),
                "{retries}/{initial}/{max}"
            );
            assert!(
                serde_json::from_value::<RetryPolicy>(serde_json::json!({
                    "max_retries": retries,
                    "initial_delay_ms": initial,
                    "max_delay_ms": max,
                }))
                .is_err()
            );
        }
        assert!(
            serde_json::from_value::<RetryPolicy>(serde_json::json!({
                "max_retries": 1,
                "initial_delay_ms": 1,
                "max_delay_ms": 1,
                "extra": true,
            }))
            .is_err()
        );
    }
}
