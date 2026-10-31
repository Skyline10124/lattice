// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 档位保留其元数据来源；协议无法表达时拒绝，不跨参数族回退。

use std::collections::BTreeMap;

use lattice_conversation::Effort;
use serde_json::Value;

use crate::{
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
    pub(crate) fn new(model: Option<&Model>, features: Features) -> Self {
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
            let output = model
                .and_then(|model| model.limit.as_ref())
                .and_then(|limit| limit.output)
                .filter(|output| *output != 0)
                .unwrap_or(32_768);
            for (effort, base) in [
                (Effort::Low, 2048_u64),
                (Effort::Medium, 8192),
                (Effort::High, 16384),
                (Effort::Max, 32768),
            ] {
                let plan = bounds.and_then(|(min, max)| {
                    let tokens = base
                        .max(min.unwrap_or(0))
                        .min(max.unwrap_or(u64::MAX))
                        .min(output / 2);
                    (tokens >= min.unwrap_or(0)).then_some(ReasoningPlan::Budget {
                        effort,
                        tokens,
                        min,
                        enable_toggle,
                    })
                });
                sources.insert(effort, plan);
            }
        }
        let candidates = match model.and_then(|model| model.effort.as_deref()) {
            Some(handwritten) => handwritten
                .iter()
                .map(|&effort| {
                    (
                        effort,
                        sources
                            .get(&effort)
                            .copied()
                            .unwrap_or(Some(ReasoningPlan::Effort {
                                effort,
                                enable_toggle: enable_toggle && effort != Effort::Off,
                            })),
                    )
                })
                .collect(),
            None => sources,
        };
        let mut previous_budget = None;
        Self(
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
        )
    }

    pub(crate) fn select(&self, effort: Effort) -> Option<ReasoningPlan> {
        self.0.get(&effort).copied()
    }

    pub(crate) fn supported_efforts(&self) -> Vec<Effort> {
        self.0.keys().copied().collect()
    }
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
        ReasoningCapabilities::new(Some(&model), features)
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
        assert!(plans(&ReasoningCapabilities::new(None, Features::GenerateContent)).is_empty());
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
    fn handwritten_names_preserve_sources_and_only_missing_sources_become_effort() {
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
