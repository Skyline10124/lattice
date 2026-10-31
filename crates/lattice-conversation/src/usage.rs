// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 每次尝试的用量是权威事实，汇总只从各次尝试派生。

use serde::{Deserialize, Serialize};

/// 输入包含缓存读写，输出包含推理；未报告的计数保持未知。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenUsage {
    pub input: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
    pub output: Option<u64>,
    pub reasoning: Option<u64>,
}

impl TokenUsage {
    /// 全部未知时不产生一份空报告。
    pub fn normalized(self) -> Option<Self> {
        (self != Self::default()).then_some(self)
    }

    /// 上下文只计一次输入和输出，缺失加数不补成 provider 报告的零。
    pub fn context_tokens(self) -> Option<u64> {
        self.input
            .into_iter()
            .chain(self.output)
            .reduce(u64::saturating_add)
    }
}

/// 尝试的费用结果；金额有限且非负，确切标记与金额一同保存。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CostEstimate {
    #[serde(serialize_with = "serialize_usd", deserialize_with = "deserialize_usd")]
    pub usd: f64,
    pub exact: bool,
}

fn serialize_usd<S: serde::Serializer>(usd: &f64, serializer: S) -> Result<S::Ok, S::Error> {
    if !usd.is_finite() || *usd < 0.0 {
        return Err(serde::ser::Error::custom(
            "USD cost must be finite and nonnegative",
        ));
    }
    usd.serialize(serializer)
}

fn deserialize_usd<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
    let usd = f64::deserialize(deserializer)?;
    if !usd.is_finite() || usd < 0.0 {
        return Err(serde::de::Error::custom(
            "USD cost must be finite and nonnegative",
        ));
    }
    Ok(usd)
}

/// 单项已报告计数的小计与完整性；完整性只看计数，不依赖价格。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenSummary {
    pub known: Option<u64>,
    pub complete: bool,
    pub overflowed: bool,
}

/// 费用小计与未知费用的尝试数；未知尝试不贡献金额。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CostSummary {
    pub known: Option<CostEstimate>,
    pub unknown_attempts: u64,
    pub overflowed: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageCompleteness {
    Complete,
    #[default]
    Partial,
}

/// 各项累计已知值，整体完整性由尝试的判定与溢出决定。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageSummary {
    pub attempts: u64,
    pub partial_attempts: u64,
    pub kind: UsageCompleteness,
    pub input: TokenSummary,
    pub cache_read: TokenSummary,
    pub cache_write: TokenSummary,
    pub output: TokenSummary,
    pub reasoning: TokenSummary,
    pub cost: CostSummary,
}

impl UsageSummary {
    pub fn from_attempts(
        attempts: impl IntoIterator<
            Item = (Option<TokenUsage>, UsageCompleteness, Option<CostEstimate>),
        >,
    ) -> Self {
        let empty = TokenSummary {
            known: None,
            complete: true,
            overflowed: false,
        };
        let mut result = Self {
            attempts: 0,
            partial_attempts: 0,
            kind: UsageCompleteness::Complete,
            input: empty,
            cache_read: empty,
            cache_write: empty,
            output: empty,
            reasoning: empty,
            cost: CostSummary {
                known: None,
                unknown_attempts: 0,
                overflowed: false,
            },
        };
        for (usage, usage_kind, cost) in attempts {
            result.attempts += 1;
            result.partial_attempts += u64::from(usage_kind == UsageCompleteness::Partial);
            let usage = usage.unwrap_or_default();
            for (summary, value) in [
                (&mut result.input, usage.input),
                (&mut result.cache_read, usage.cache_read),
                (&mut result.cache_write, usage.cache_write),
                (&mut result.output, usage.output),
                (&mut result.reasoning, usage.reasoning),
            ] {
                if let Some(value) = value {
                    let (sum, overflow) = summary.known.unwrap_or(0).overflowing_add(value);
                    summary.known = Some(if overflow { u64::MAX } else { sum });
                    summary.overflowed |= overflow;
                    summary.complete &= !overflow;
                } else {
                    summary.complete = false;
                }
            }
            match cost {
                Some(value) => {
                    let previous = result.cost.known.unwrap_or(CostEstimate {
                        usd: 0.0,
                        exact: true,
                    });
                    let sum = previous.usd + value.usd;
                    result.cost.known = Some(CostEstimate {
                        usd: sum.min(f64::MAX),
                        exact: previous.exact && value.exact,
                    });
                    result.cost.overflowed |= !sum.is_finite();
                }
                None => result.cost.unknown_attempts += 1,
            }
        }
        if result.attempts == 0 {
            for metric in [
                &mut result.input,
                &mut result.cache_read,
                &mut result.cache_write,
                &mut result.output,
                &mut result.reasoning,
            ] {
                metric.known = Some(0);
            }
            result.cost.known = Some(CostEstimate {
                usd: 0.0,
                exact: true,
            });
        }
        if let Some(cost) = &mut result.cost.known {
            cost.exact &= result.cost.unknown_attempts == 0 && !result.cost.overflowed;
        }
        if result.partial_attempts != 0
            || result.input.overflowed
            || result.cache_read.overflowed
            || result.cache_write.overflowed
            || result.output.overflowed
            || result.reasoning.overflowed
            || result.cost.overflowed
        {
            result.kind = UsageCompleteness::Partial;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_keep_independent_known_values() {
        let summary = UsageSummary::from_attempts([
            (
                Some(TokenUsage {
                    input: Some(10),
                    cache_read: Some(0),
                    ..Default::default()
                }),
                UsageCompleteness::Complete,
                Some(CostEstimate {
                    usd: 0.25,
                    exact: true,
                }),
            ),
            (
                Some(TokenUsage {
                    output: Some(3),
                    reasoning: Some(7),
                    ..Default::default()
                }),
                UsageCompleteness::Partial,
                None,
            ),
            (
                None,
                UsageCompleteness::Partial,
                Some(CostEstimate {
                    usd: 0.0,
                    exact: true,
                }),
            ),
        ]);
        assert_eq!(summary.attempts, 3);
        assert_eq!(summary.partial_attempts, 2);
        assert_eq!(
            summary.input,
            TokenSummary {
                known: Some(10),
                complete: false,
                overflowed: false
            }
        );
        assert_eq!(summary.cache_read.known, Some(0));
        assert!(!summary.cache_read.complete);
        assert_eq!(summary.cache_write.known, None);
        assert_eq!(summary.output.known, Some(3));
        assert_eq!(summary.reasoning.known, Some(7));
        assert!(!summary.reasoning.complete);
        assert_eq!(
            summary.cost,
            CostSummary {
                known: Some(CostEstimate {
                    usd: 0.25,
                    exact: false
                }),
                unknown_attempts: 1,
                overflowed: false
            }
        );
        assert_eq!(summary.kind, UsageCompleteness::Partial);
        let empty = UsageSummary::from_attempts([]);
        assert_eq!(empty.attempts, 0);
        assert_eq!(empty.partial_attempts, 0);
        assert_eq!(empty.input.known, Some(0));
        assert!(empty.input.complete);
        assert_eq!(
            empty.cost.known,
            Some(CostEstimate {
                usd: 0.0,
                exact: true
            })
        );
        assert_eq!(empty.kind, UsageCompleteness::Complete);
        let unknown = UsageSummary::from_attempts([(None, UsageCompleteness::Partial, None)]);
        assert_eq!(unknown.input.known, None);
        assert!(!unknown.input.complete);
        assert_eq!(unknown.cost.known, None);
        assert_eq!(unknown.kind, UsageCompleteness::Partial);
    }

    #[test]
    fn summaries_use_attempt_completeness_and_independent_overflow_flags() {
        let usage = TokenUsage {
            input: Some(10),
            cache_read: Some(0),
            cache_write: Some(1),
            output: Some(2),
            reasoning: Some(1),
        };
        let cost = CostEstimate {
            usd: 0.125,
            exact: true,
        };
        let complete = UsageSummary {
            attempts: 2,
            partial_attempts: 0,
            kind: UsageCompleteness::Complete,
            input: TokenSummary {
                known: Some(20),
                complete: true,
                overflowed: false,
            },
            cache_read: TokenSummary {
                known: Some(0),
                complete: true,
                overflowed: false,
            },
            cache_write: TokenSummary {
                known: Some(2),
                complete: true,
                overflowed: false,
            },
            output: TokenSummary {
                known: Some(4),
                complete: true,
                overflowed: false,
            },
            reasoning: TokenSummary {
                known: Some(2),
                complete: true,
                overflowed: false,
            },
            cost: CostSummary {
                known: Some(CostEstimate {
                    usd: 0.25,
                    exact: true,
                }),
                unknown_attempts: 0,
                overflowed: false,
            },
        };
        for (name, attempts, expected) in [
            (
                "complete",
                [(Some(usage), UsageCompleteness::Complete, Some(cost)); 2],
                complete.clone(),
            ),
            (
                "unreported token does not decide kind",
                [
                    (Some(usage), UsageCompleteness::Complete, Some(cost)),
                    (
                        Some(TokenUsage {
                            cache_write: None,
                            ..usage
                        }),
                        UsageCompleteness::Complete,
                        Some(cost),
                    ),
                ],
                UsageSummary {
                    cache_write: TokenSummary {
                        known: Some(1),
                        complete: false,
                        overflowed: false,
                    },
                    ..complete.clone()
                },
            ),
            (
                "unknown cost does not decide kind",
                [
                    (Some(usage), UsageCompleteness::Complete, Some(cost)),
                    (Some(usage), UsageCompleteness::Complete, None),
                ],
                UsageSummary {
                    cost: CostSummary {
                        known: Some(CostEstimate {
                            usd: 0.125,
                            exact: false,
                        }),
                        unknown_attempts: 1,
                        overflowed: false,
                    },
                    ..complete.clone()
                },
            ),
            (
                "partial attempt with all metrics known",
                [
                    (Some(usage), UsageCompleteness::Partial, Some(cost)),
                    (Some(usage), UsageCompleteness::Complete, Some(cost)),
                ],
                UsageSummary {
                    partial_attempts: 1,
                    kind: UsageCompleteness::Partial,
                    ..complete.clone()
                },
            ),
            (
                "token overflow",
                [
                    (
                        Some(TokenUsage {
                            input: Some(u64::MAX),
                            ..usage
                        }),
                        UsageCompleteness::Complete,
                        Some(cost),
                    ),
                    (
                        Some(TokenUsage {
                            input: Some(1),
                            ..usage
                        }),
                        UsageCompleteness::Complete,
                        Some(cost),
                    ),
                ],
                UsageSummary {
                    kind: UsageCompleteness::Partial,
                    input: TokenSummary {
                        known: Some(u64::MAX),
                        complete: false,
                        overflowed: true,
                    },
                    ..complete.clone()
                },
            ),
            (
                "cost overflow",
                [(
                    Some(usage),
                    UsageCompleteness::Complete,
                    Some(CostEstimate {
                        usd: f64::MAX,
                        exact: true,
                    }),
                ); 2],
                UsageSummary {
                    kind: UsageCompleteness::Partial,
                    cost: CostSummary {
                        known: Some(CostEstimate {
                            usd: f64::MAX,
                            exact: false,
                        }),
                        unknown_attempts: 0,
                        overflowed: true,
                    },
                    ..complete.clone()
                },
            ),
        ] {
            assert_eq!(UsageSummary::from_attempts(attempts), expected, "{name}");
        }
    }

    #[test]
    fn cost_totals_preserve_estimates_and_unknown_attempts() {
        for (costs, expected, unknown) in [
            (
                vec![
                    Some(CostEstimate {
                        usd: 0.0028,
                        exact: true,
                    }),
                    Some(CostEstimate {
                        usd: 0.0028,
                        exact: true,
                    }),
                    Some(CostEstimate {
                        usd: 0.0027,
                        exact: false,
                    }),
                ],
                0.0083,
                0,
            ),
            (
                vec![
                    Some(CostEstimate {
                        usd: 0.0028,
                        exact: true,
                    }),
                    Some(CostEstimate {
                        usd: 0.0028,
                        exact: true,
                    }),
                    None,
                ],
                0.0056,
                1,
            ),
        ] {
            let usage = TokenUsage {
                input: Some(1000),
                cache_read: Some(200),
                cache_write: Some(100),
                output: Some(100),
                reasoning: Some(20),
            };
            let summary = UsageSummary::from_attempts(
                costs
                    .into_iter()
                    .map(|cost| (Some(usage), UsageCompleteness::Complete, cost)),
            );
            let known = summary.cost.known.unwrap();
            assert!((known.usd - expected).abs() < 1e-12);
            assert!(!known.exact);
            assert_eq!(summary.cost.unknown_attempts, unknown);
            assert_eq!(summary.partial_attempts, 0);
            assert_eq!(summary.kind, UsageCompleteness::Complete);
        }
    }
}
