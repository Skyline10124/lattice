// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 档位保留其元数据来源；协议无法表达时拒绝，不跨参数族回退。

use std::collections::BTreeMap;

use lattice_conversation::Effort;
use serde_json::Value;

use crate::{
    LlmError,
    metadata::Model,
    protocol::{Features, ThinkingBudget, ThinkingToggle},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReasoningPlan {
    Effort {
        effort: Effort,
        enable_toggle: bool,
    },
    Toggle {
        enabled: bool,
    },
    Budget {
        effort: Effort,
        tokens: u64,
        min: Option<u64>,
        enable_toggle: bool,
    },
}

impl ReasoningPlan {
    fn supported(self, features: Features) -> bool {
        match self {
            Self::Effort { effort, .. } => {
                features != Features::Interactions || effort != Effort::Off
            }
            Self::Toggle { .. } => match features {
                Features::ChatCompletions(chat) => chat.thinking_toggle != ThinkingToggle::None,
                Features::Messages(_) | Features::GenerateContent => true,
                _ => false,
            },
            Self::Budget { .. } => match features {
                Features::ChatCompletions(chat) => chat.thinking_budget != ThinkingBudget::None,
                Features::Messages(_) | Features::GenerateContent => true,
                _ => false,
            },
        }
    }
}

/// 只封装能力计算；候选名字和选中项共享同一份带来源的结果。
pub(crate) struct ReasoningCapabilities(BTreeMap<Effort, ReasoningPlan>);

impl ReasoningCapabilities {
    pub(crate) fn new(
        provider: &str,
        model_id: &str,
        model: Option<&Model>,
        features: Features,
    ) -> Result<Self, LlmError> {
        let options = model
            .and_then(|model| model.reasoning_options.as_ref())
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        let has = |kind| options.iter().any(|option| option["type"] == kind);
        let has_effort = has("effort");
        let has_budget = has("budget_tokens");
        let has_toggle = has("toggle");
        let enable_toggle = has_toggle
            && matches!(features,
            Features::ChatCompletions(chat) if chat.thinking_toggle != ThinkingToggle::None);
        let output = model
            .and_then(|model| model.limit.as_ref())
            .and_then(|limit| limit.output)
            .filter(|output| *output != 0)
            .unwrap_or(32_768);

        // None 仍保留来源，避免手写列表把无效预算重新解释为 effort。
        let mut sources = BTreeMap::<Effort, Option<ReasoningPlan>>::new();
        for option in options.iter().filter(|option| option["type"] == "effort") {
            if let Some(values) = option["values"].as_array() {
                for name in values.iter().filter_map(Value::as_str) {
                    let effort = if name == "none" {
                        Some(Effort::Off)
                    } else {
                        name.parse::<Effort>().ok()
                    };
                    if let Some(effort) = effort {
                        sources.insert(
                            effort,
                            Some(ReasoningPlan::Effort {
                                effort,
                                enable_toggle: enable_toggle && effort != Effort::Off,
                            }),
                        );
                    }
                }
            }
        }
        if has_toggle {
            sources
                .entry(Effort::Off)
                .or_insert(Some(ReasoningPlan::Toggle { enabled: false }));
            if !has_effort && !has_budget {
                sources.insert(Effort::High, Some(ReasoningPlan::Toggle { enabled: true }));
            }
        }
        if !has_effort
            && let Some(option) = options
                .iter()
                .find(|option| option["type"] == "budget_tokens")
        {
            let bounds = budget_bounds(option);
            for effort in [Effort::Low, Effort::Medium, Effort::High, Effort::Max] {
                let plan = bounds.and_then(|bounds| {
                    budget_plan(effort, output, bounds, features, enable_toggle)
                });
                sources.insert(effort, plan);
            }
        }
        let candidates = match model.and_then(|model| model.effort.as_deref()) {
            Some(handwritten) => handwritten
                .iter()
                .map(|&effort| {
                    Ok((
                        effort,
                        match sources.get(&effort) {
                            Some(plan) => *plan,
                            None => handwritten_plan(effort, output, features).map_err(|reason| {
                                LlmError::Config {
                                    message: format!(
                                        "provider {provider:?}, model {model_id:?}, effort {effort}: {reason}"
                                    ),
                                }
                            })?,
                        },
                    ))
                })
                .collect::<Result<BTreeMap<_, _>, LlmError>>()?,
            None => sources,
        };
        let mut previous_budget = None;
        Ok(Self(
            candidates
                .into_iter()
                .filter_map(|(effort, plan)| {
                    let plan = plan.filter(|plan| plan.supported(features))?;
                    if let ReasoningPlan::Budget { tokens, .. } = plan {
                        if previous_budget == Some(tokens) {
                            return None;
                        }
                        previous_budget = Some(tokens);
                    }
                    Some((effort, plan))
                })
                .collect(),
        ))
    }

    pub(crate) fn select(&self, effort: Effort) -> Option<ReasoningPlan> {
        self.0.get(&effort).copied()
    }
}

fn budget_plan(
    effort: Effort,
    output: u64,
    (min, max): (Option<u64>, Option<u64>),
    features: Features,
    enable_toggle: bool,
) -> Option<ReasoningPlan> {
    let base = match effort {
        Effort::Low => 2048_u64,
        Effort::Medium => 8192,
        Effort::High => 16384,
        Effort::Max => 32768,
        _ => return None,
    };
    let tokens = base
        .max(min.unwrap_or(0))
        .min(max.unwrap_or(u64::MAX))
        .min(output / 2);
    // Messages 在发送时遵守其协议下限；其他协议仍过滤低于元数据 min 的预算。
    (tokens > 0 && (matches!(features, Features::Messages(_)) || tokens >= min.unwrap_or(0)))
        .then_some(ReasoningPlan::Budget {
            effort,
            tokens,
            min,
            enable_toggle,
        })
}

fn handwritten_plan(
    effort: Effort,
    output: u64,
    features: Features,
) -> Result<Option<ReasoningPlan>, &'static str> {
    if let Features::ChatCompletions(chat) = features {
        let toggle = chat.thinking_toggle != ThinkingToggle::None;
        let budget = chat.thinking_budget != ThinkingBudget::None;
        if toggle && effort == Effort::Off {
            return Ok(Some(ReasoningPlan::Toggle { enabled: false }));
        }
        if budget {
            return match effort {
                Effort::Low | Effort::Medium | Effort::High | Effort::Max => {
                    Ok(budget_plan(effort, output, (None, None), features, toggle))
                }
                _ => Err(
                    "thinking_budget requires low, medium, high or max; off requires thinking_toggle",
                ),
            };
        }
        if toggle {
            return match effort {
                Effort::High => Ok(Some(ReasoningPlan::Toggle { enabled: true })),
                _ => Err("thinking_toggle without thinking_budget only supports off and high"),
            };
        }
    }
    Ok(Some(ReasoningPlan::Effort {
        effort,
        enable_toggle: false,
    }))
}

fn budget_bounds(option: &Value) -> Option<(Option<u64>, Option<u64>)> {
    let bound = |name| match option.get(name) {
        None => Some(None),
        Some(value) => value.as_u64().map(Some),
    };
    let min = bound("min")?;
    let max = bound("max")?;
    if min.zip(max).is_some_and(|(min, max)| min > max) {
        return None;
    }
    Some((min, max))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ChatCompletionsFeatures, Protocol};
    use serde_json::json;

    fn capabilities(
        options: Value,
        output: Option<u64>,
        handwritten: Option<&[Effort]>,
        features: Features,
    ) -> ReasoningCapabilities {
        let mut model: Model = serde_json::from_value(json!({"id":"model", "reasoning":true,"reasoning_options":options,"limit":{"output":output}})).unwrap();
        model.effort = handwritten.map(<[Effort]>::to_vec);
        ReasoningCapabilities::new("test", "model", Some(&model), features).unwrap()
    }

    fn plans(capabilities: &ReasoningCapabilities) -> Vec<(Effort, ReasoningPlan)> {
        Effort::ALL
            .into_iter()
            .filter_map(|effort| capabilities.select(effort).map(|plan| (effort, plan)))
            .collect()
    }

    #[test]
    fn sources_and_protocols_never_fall_back_to_another_parameter_family() {
        for protocol in [
            Protocol::ChatCompletions,
            Protocol::Messages,
            Protocol::GenerateContent,
            Protocol::Responses,
            Protocol::Interactions,
        ] {
            let features = Features::defaults(protocol);
            let effort = capabilities(
                json!([{"type":"effort","values":["default","unknown","none","high","minimal","xhigh"]},{"type":"budget_tokens"}]),
                None,
                None,
                features,
            );
            let expected: Vec<_> = [Effort::Off, Effort::Minimal, Effort::High, Effort::Xhigh]
                .into_iter()
                .filter(|effort| protocol != Protocol::Interactions || *effort != Effort::Off)
                .map(|effort| {
                    (
                        effort,
                        ReasoningPlan::Effort {
                            effort,
                            enable_toggle: false,
                        },
                    )
                })
                .collect();
            assert_eq!(plans(&effort), expected, "{protocol:?}");
            let toggle = capabilities(json!([{"type":"toggle"}]), None, None, features);
            let budget = capabilities(
                json!([{"type":"toggle"},{"type":"budget_tokens"}]),
                None,
                None,
                features,
            );
            if matches!(protocol, Protocol::Messages | Protocol::GenerateContent) {
                assert_eq!(
                    plans(&toggle),
                    vec![
                        (Effort::Off, ReasoningPlan::Toggle { enabled: false }),
                        (Effort::High, ReasoningPlan::Toggle { enabled: true })
                    ]
                );
                assert!(matches!(
                    budget.select(Effort::High),
                    Some(ReasoningPlan::Budget { tokens: 16384, .. })
                ));
                assert_eq!(budget.select(Effort::Max), None);
            } else {
                assert!(plans(&toggle).is_empty());
                assert!(plans(&budget).is_empty());
            }
        }
        for options in [
            Value::Null,
            json!([]),
            json!({"type":"toggle"}),
            json!([{"type":"unknown"}]),
            json!([{"type":"effort","values":["default","future"]},{"type":"budget_tokens"}]),
        ] {
            assert!(
                plans(&capabilities(
                    options,
                    None,
                    None,
                    Features::GenerateContent
                ))
                .is_empty()
            );
        }
        assert!(
            plans(
                &ReasoningCapabilities::new("test", "model", None, Features::GenerateContent)
                    .unwrap()
            )
            .is_empty()
        );
    }

    #[test]
    fn chat_toggle_combines_only_when_declared_and_effort_wins_duplicate_off() {
        let features = Features::ChatCompletions(ChatCompletionsFeatures {
            thinking_toggle: ThinkingToggle::ThinkingType,
            thinking_budget: ThinkingBudget::ThinkingBudget,
            ..Default::default()
        });
        for options in [
            json!([{"type":"toggle"},{"type":"effort","values":["none","high"]}]),
            json!([{"type":"effort","values":["none","high"]},{"type":"toggle"}]),
        ] {
            let caps = capabilities(options, None, None, features);
            assert_eq!(
                caps.select(Effort::Off),
                Some(ReasoningPlan::Effort {
                    effort: Effort::Off,
                    enable_toggle: false
                })
            );
            assert_eq!(
                caps.select(Effort::High),
                Some(ReasoningPlan::Effort {
                    effort: Effort::High,
                    enable_toggle: true
                })
            );
        }
        for (options, expected) in [
            (
                json!([{"type":"effort","values":["high"]}]),
                ReasoningPlan::Effort {
                    effort: Effort::High,
                    enable_toggle: false,
                },
            ),
            (
                json!([{"type":"toggle"}]),
                ReasoningPlan::Toggle { enabled: true },
            ),
            (
                json!([{"type":"budget_tokens"}]),
                ReasoningPlan::Budget {
                    effort: Effort::High,
                    tokens: 16384,
                    min: None,
                    enable_toggle: false,
                },
            ),
            (
                json!([{"type":"toggle"},{"type":"budget_tokens"}]),
                ReasoningPlan::Budget {
                    effort: Effort::High,
                    tokens: 16384,
                    min: None,
                    enable_toggle: true,
                },
            ),
        ] {
            assert_eq!(
                capabilities(options, None, None, features).select(Effort::High),
                Some(expected)
            );
        }
    }

    #[test]
    fn budgets_clamp_bounds_before_output_and_keep_the_lowest_remaining_name() {
        for (option, output, expected) in [
            (
                json!({"type":"budget_tokens"}),
                Some(8192),
                vec![(Effort::Low, 2048), (Effort::Medium, 4096)],
            ),
            (
                json!({"type":"budget_tokens"}),
                None,
                vec![
                    (Effort::Low, 2048),
                    (Effort::Medium, 8192),
                    (Effort::High, 16384),
                ],
            ),
            (
                json!({"type":"budget_tokens"}),
                Some(0),
                vec![
                    (Effort::Low, 2048),
                    (Effort::Medium, 8192),
                    (Effort::High, 16384),
                ],
            ),
            (
                json!({"type":"budget_tokens"}),
                Some(65536),
                vec![
                    (Effort::Low, 2048),
                    (Effort::Medium, 8192),
                    (Effort::High, 16384),
                    (Effort::Max, 32768),
                ],
            ),
            (
                json!({"type":"budget_tokens","min":4096,"max":10000}),
                Some(65536),
                vec![
                    (Effort::Low, 4096),
                    (Effort::Medium, 8192),
                    (Effort::High, 10000),
                ],
            ),
            (
                json!({"type":"budget_tokens","min":5000}),
                Some(8192),
                vec![],
            ),
            (
                json!({"type":"budget_tokens","max":1024}),
                None,
                vec![(Effort::Low, 1024)],
            ),
            (
                json!({"type":"budget_tokens","min":10,"max":9}),
                None,
                vec![],
            ),
            (json!({"type":"budget_tokens","min":-1}), None, vec![]),
            (json!({"type":"budget_tokens","max":"100"}), None, vec![]),
        ] {
            let caps = capabilities(json!([option]), output, None, Features::GenerateContent);
            let actual: Vec<_> = plans(&caps)
                .into_iter()
                .map(|(effort, plan)| {
                    let ReasoningPlan::Budget { tokens, .. } = plan else {
                        panic!("budget source")
                    };
                    (effort, tokens)
                })
                .collect();
            assert_eq!(actual, expected, "{option}, output={output:?}");
        }
    }

    #[test]
    fn zero_budgets_are_unavailable_for_every_budget_protocol() {
        for features in [
            Features::GenerateContent,
            Features::defaults(Protocol::Messages),
            Features::ChatCompletions(ChatCompletionsFeatures {
                thinking_budget: ThinkingBudget::ThinkingBudget,
                ..Default::default()
            }),
        ] {
            for handwritten in [None, Some(&[Effort::Low][..])] {
                assert!(
                    plans(&capabilities(
                        json!([{"type":"budget_tokens"}]),
                        Some(1),
                        handwritten,
                        features,
                    ))
                    .is_empty(),
                    "{features:?}"
                );
            }
        }
    }

    #[test]
    fn handwritten_names_preserve_sources_and_infer_missing_sources_from_features() {
        let features = Features::GenerateContent;
        let options = json!([{"type":"toggle"},{"type":"budget_tokens"}]);
        let caps = capabilities(
            options.clone(),
            Some(8192),
            Some(&[Effort::High, Effort::Off, Effort::Minimal, Effort::High]),
            features,
        );
        assert_eq!(
            plans(&caps),
            vec![
                (Effort::Off, ReasoningPlan::Toggle { enabled: false }),
                (
                    Effort::Minimal,
                    ReasoningPlan::Effort {
                        effort: Effort::Minimal,
                        enable_toggle: false
                    }
                ),
                (
                    Effort::High,
                    ReasoningPlan::Budget {
                        effort: Effort::High,
                        tokens: 4096,
                        min: None,
                        enable_toggle: false
                    }
                ),
            ]
        );
        assert!(plans(&capabilities(options.clone(), None, Some(&[]), features)).is_empty());
        let unsupported = Features::defaults(Protocol::ChatCompletions);
        assert_eq!(
            capabilities(
                options,
                None,
                Some(&[Effort::Off, Effort::High]),
                unsupported
            )
            .select(Effort::High),
            None
        );
        for option in [
            json!({"type":"budget_tokens","min":5000}),
            json!({"type":"budget_tokens","min":-1}),
        ] {
            assert_eq!(
                capabilities(json!([option]), Some(8192), Some(&[Effort::High]), features)
                    .select(Effort::High),
                None
            );
        }
        let chat = Features::ChatCompletions(ChatCompletionsFeatures {
            thinking_toggle: ThinkingToggle::ThinkingType,
            thinking_budget: ThinkingBudget::ThinkingBudget,
            ..Default::default()
        });
        assert_eq!(
            capabilities(
                json!([{"type":"effort","values":["none","high"]}]),
                None,
                Some(&[Effort::Off, Effort::High]),
                chat,
            )
            .select(Effort::High),
            Some(ReasoningPlan::Effort {
                effort: Effort::High,
                enable_toggle: false
            }),
        );
        assert_eq!(
            capabilities(
                json!([{"type":"toggle"}]),
                None,
                Some(&[Effort::High]),
                chat
            )
            .select(Effort::High),
            Some(ReasoningPlan::Toggle { enabled: true }),
        );
        assert_eq!(
            capabilities(Value::Null, None, Some(&[Effort::High]), unsupported)
                .select(Effort::High),
            Some(ReasoningPlan::Effort {
                effort: Effort::High,
                enable_toggle: false
            })
        );
    }
}
