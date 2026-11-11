// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 推理档位只保存选择；协议参数与预算由请求网关解释。

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

/// 声明顺序也是从低到高的选择顺序。
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub const ALL: [Self; 7] = [
        Self::Off,
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::Xhigh,
        Self::Max,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

impl fmt::Display for Effort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("expected one of off, minimal, low, medium, high, xhigh, max")]
pub struct ParseEffortError;

impl FromStr for Effort {
    type Err = ParseEffortError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|effort| effort.as_str() == value)
            .ok_or(ParseEffortError)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effort_names_serde_and_order_agree() {
        let names = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
        for (effort, name) in Effort::ALL.into_iter().zip(names) {
            assert_eq!(name.parse::<Effort>().unwrap(), effort);
            assert_eq!(effort.to_string(), name);
            let json = serde_json::to_string(&name).unwrap();
            assert_eq!(serde_json::to_string(&effort).unwrap(), json);
            assert_eq!(serde_json::from_str::<Effort>(&json).unwrap(), effort);
        }
        assert!(Effort::ALL.windows(2).all(|pair| pair[0] < pair[1]));
        for name in ["none", "default", "HIGH", " high", "high ", "", "unknown"] {
            assert!(name.parse::<Effort>().is_err(), "{name:?}");
            assert!(serde_json::from_value::<Effort>(serde_json::json!(name)).is_err());
        }
    }
}
