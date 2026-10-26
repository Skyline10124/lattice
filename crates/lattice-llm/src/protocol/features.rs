// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use serde::Deserialize;

use super::Protocol;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum EffortParam {
    #[default]
    ReasoningEffort,
    ReasoningObject,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ThinkingToggle {
    #[default]
    None,
    ThinkingType,
    EnableThinking,
}

// 变体名与合同中的 feature 取值一一对应，`thinking-budget` 取值因此与类型同名。
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ThinkingBudget {
    #[default]
    None,
    ThinkingBudget,
    ReasoningMaxTokens,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ReasoningReplay {
    #[default]
    None,
    ReasoningContent,
    ContentChunks,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Cache {
    #[default]
    Automatic,
    None,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ChatCompletionsFeatures {
    pub(crate) effort_param: EffortParam,
    pub(crate) thinking_toggle: ThinkingToggle,
    pub(crate) thinking_budget: ThinkingBudget,
    pub(crate) reasoning_replay: ReasoningReplay,
    pub(crate) cache_markers: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct MessagesFeatures {
    pub(crate) cache: Cache,
}

/// Fully resolved features; protocols without optional behavior carry no fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Features {
    ChatCompletions(ChatCompletionsFeatures),
    Responses,
    Messages(MessagesFeatures),
    GenerateContent,
    Interactions,
}

impl Features {
    pub(crate) fn defaults(protocol: Protocol) -> Self {
        match protocol {
            Protocol::ChatCompletions => Self::ChatCompletions(ChatCompletionsFeatures::default()),
            Protocol::Responses => Self::Responses,
            Protocol::Messages => Self::Messages(MessagesFeatures::default()),
            Protocol::GenerateContent => Self::GenerateContent,
            Protocol::Interactions => Self::Interactions,
        }
    }

    pub(crate) fn protocol(self) -> Protocol {
        match self {
            Self::ChatCompletions(_) => Protocol::ChatCompletions,
            Self::Responses => Protocol::Responses,
            Self::Messages(_) => Protocol::Messages,
            Self::GenerateContent => Protocol::GenerateContent,
            Self::Interactions => Protocol::Interactions,
        }
    }

    pub(crate) fn apply(&mut self, name: &str, value: toml::Value) -> Result<(), String> {
        fn parse<T: serde::de::DeserializeOwned>(value: toml::Value) -> Result<T, String> {
            // TOML also supports externally tagged enum tables; feature values do not.
            if !matches!(value, toml::Value::String(_) | toml::Value::Boolean(_)) {
                return Err("invalid type: feature values must be strings or booleans".to_owned());
            }
            value
                .try_into()
                .map_err(|error: toml::de::Error| error.message().to_owned())
        }

        match (self, name) {
            (Self::ChatCompletions(features), "effort_param") => {
                features.effort_param = parse(value)?
            }
            (Self::ChatCompletions(features), "thinking_toggle") => {
                features.thinking_toggle = parse(value)?
            }
            (Self::ChatCompletions(features), "thinking_budget") => {
                features.thinking_budget = parse(value)?
            }
            (Self::ChatCompletions(features), "reasoning_replay") => {
                features.reasoning_replay = parse(value)?
            }
            (Self::ChatCompletions(features), "cache_markers") => {
                features.cache_markers = parse(value)?
            }
            (Self::Messages(features), "cache") => features.cache = parse(value)?,
            (features, _) => {
                return Err(format!(
                    "unknown feature `{name}` for {}",
                    features.protocol().as_str()
                ));
            }
        }
        Ok(())
    }
}
