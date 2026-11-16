// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 模型输入模态能力原语。

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 模型支持的输入模态类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputModality {
    Text,
    Image,
    Audio,
    Video,
}

impl InputModality {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Video => "video",
        }
    }
}

impl fmt::Display for InputModality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("输入模态无效，仅允许 text、image、audio、video")]
pub struct ParseInputModalityError;

impl FromStr for InputModality {
    type Err = ParseInputModalityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "text" => Ok(Self::Text),
            "image" => Ok(Self::Image),
            "audio" => Ok(Self::Audio),
            "video" => Ok(Self::Video),
            _ => Err(ParseInputModalityError),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_modality_strings_serde_and_strict_from_str() {
        for (modality, expected) in [
            (InputModality::Text, "text"),
            (InputModality::Image, "image"),
            (InputModality::Audio, "audio"),
            (InputModality::Video, "video"),
        ] {
            assert_eq!(modality.as_str(), expected, "as_str 不匹配");
            assert_eq!(modality.to_string(), expected, "Display 不匹配");
            assert_eq!(
                expected.parse::<InputModality>(),
                Ok(modality),
                "FromStr 应接受小写标签"
            );
            let json = serde_json::to_string(&modality).unwrap();
            assert_eq!(json, format!("\"{expected}\""), "JSON 序列化标签不匹配");
            assert_eq!(
                serde_json::from_str::<InputModality>(&json).unwrap(),
                modality,
                "JSON 反序列化不匹配"
            );
        }

        for invalid in [
            "", "TEXT", "Image", "voice", "pdf", " text", "image ", "unknown",
        ] {
            assert!(
                invalid.parse::<InputModality>().is_err(),
                "FromStr 必须拒绝非法输入模态 {invalid:?}"
            );
            assert!(
                serde_json::from_value::<InputModality>(serde_json::json!(invalid)).is_err(),
                "serde 必须拒绝非法输入模态 {invalid:?}"
            );
        }
    }
}
