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

/// 单项已知小计与覆盖范围；溢出的小计饱和，但不再标为完整。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricSummary<T> {
    pub known: Option<T>,
    pub unknown_attempts: u64,
    pub overflowed: bool,
}

impl<T> MetricSummary<T> {
    pub fn is_complete(&self) -> bool {
        self.unknown_attempts == 0 && !self.overflowed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageCompleteness {
    Complete,
    Partial,
}

/// 六项指标独立汇总，整体完整性由它们共同决定。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageSummary {
    pub attempts: u64,
    pub kind: UsageCompleteness,
    pub input: MetricSummary<u64>,
    pub cache_read: MetricSummary<u64>,
    pub cache_write: MetricSummary<u64>,
    pub output: MetricSummary<u64>,
    pub reasoning: MetricSummary<u64>,
    pub estimated_cost_usd: MetricSummary<f64>,
}

impl UsageSummary {
    pub fn from_attempts(
        attempts: impl IntoIterator<Item = (Option<TokenUsage>, Option<f64>)>,
    ) -> Self {
        let empty = MetricSummary {
            known: None,
            unknown_attempts: 0,
            overflowed: false,
        };
        let mut result = Self {
            attempts: 0,
            kind: UsageCompleteness::Complete,
            input: empty,
            cache_read: empty,
            cache_write: empty,
            output: empty,
            reasoning: empty,
            estimated_cost_usd: MetricSummary {
                known: None,
                unknown_attempts: 0,
                overflowed: false,
            },
        };
        for (usage, cost) in attempts {
            result.attempts += 1;
            let usage = usage.unwrap_or_default();
            for (summary, value) in [
                (&mut result.input, usage.input),
                (&mut result.cache_read, usage.cache_read),
                (&mut result.cache_write, usage.cache_write),
                (&mut result.output, usage.output),
                (&mut result.reasoning, usage.reasoning),
            ] {
                match value {
                    Some(value) => {
                        let (sum, overflow) = summary.known.unwrap_or(0).overflowing_add(value);
                        summary.known = Some(if overflow { u64::MAX } else { sum });
                        summary.overflowed |= overflow;
                    }
                    None => summary.unknown_attempts += 1,
                }
            }
            match cost {
                Some(value) => {
                    let sum = result.estimated_cost_usd.known.unwrap_or(0.0) + value;
                    result.estimated_cost_usd.known = Some(sum.min(f64::MAX));
                    result.estimated_cost_usd.overflowed |= !sum.is_finite();
                }
                None => result.estimated_cost_usd.unknown_attempts += 1,
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
            result.estimated_cost_usd.known = Some(0.0);
        }
        if !result.input.is_complete()
            || !result.cache_read.is_complete()
            || !result.cache_write.is_complete()
            || !result.output.is_complete()
            || !result.reasoning.is_complete()
            || !result.estimated_cost_usd.is_complete()
        {
            result.kind = UsageCompleteness::Partial;
        }
        result
    }
}

/// 金额编码拒绝无效值，避免非有限浮点数被 JSON 静默写成 null。
pub mod estimated_cost_usd {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(value: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error> {
        if value.is_some_and(|value| !value.is_finite() || value < 0.0) {
            return Err(serde::ser::Error::custom(
                "estimated USD cost must be finite and nonnegative",
            ));
        }
        value.serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<f64>, D::Error> {
        let value = Option::<f64>::deserialize(deserializer)?;
        if value.is_some_and(|value| !value.is_finite() || value < 0.0) {
            return Err(serde::de::Error::custom(
                "estimated USD cost must be finite and nonnegative",
            ));
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_keep_independent_known_values_and_coverage() {
        let summary = UsageSummary::from_attempts([
            (
                Some(TokenUsage {
                    input: Some(10),
                    cache_read: Some(0),
                    ..Default::default()
                }),
                Some(0.25),
            ),
            (
                Some(TokenUsage {
                    output: Some(3),
                    reasoning: Some(7),
                    ..Default::default()
                }),
                None,
            ),
            (None, Some(0.0)),
        ]);
        assert_eq!(summary.attempts, 3);
        assert_eq!(
            summary.input,
            MetricSummary {
                known: Some(10),
                unknown_attempts: 2,
                overflowed: false
            }
        );
        assert_eq!(summary.cache_read.known, Some(0));
        assert_eq!(summary.cache_write.known, None);
        assert_eq!(summary.cache_write.unknown_attempts, 3);
        assert_eq!(summary.output.known, Some(3));
        assert_eq!(summary.reasoning.known, Some(7));
        assert_eq!(
            summary.estimated_cost_usd,
            MetricSummary {
                known: Some(0.25),
                unknown_attempts: 1,
                overflowed: false
            }
        );
        assert_eq!(summary.kind, UsageCompleteness::Partial);
        let empty = UsageSummary::from_attempts([]);
        assert_eq!(empty.attempts, 0);
        assert_eq!(empty.input.known, Some(0));
        assert_eq!(empty.estimated_cost_usd.known, Some(0.0));
        assert_eq!(empty.kind, UsageCompleteness::Complete);
        let unknown = UsageSummary::from_attempts([(None, None)]);
        assert_eq!(unknown.input.known, None);
        assert_eq!(unknown.estimated_cost_usd.known, None);
        assert_eq!(unknown.kind, UsageCompleteness::Partial);
    }

    #[test]
    fn summaries_only_lose_completeness_for_the_changed_metric() {
        let usage = TokenUsage {
            input: Some(10),
            cache_read: Some(0),
            cache_write: Some(1),
            output: Some(2),
            reasoning: Some(1),
        };
        let complete = UsageSummary {
            attempts: 2,
            kind: UsageCompleteness::Complete,
            input: MetricSummary {
                known: Some(20),
                unknown_attempts: 0,
                overflowed: false,
            },
            cache_read: MetricSummary {
                known: Some(0),
                unknown_attempts: 0,
                overflowed: false,
            },
            cache_write: MetricSummary {
                known: Some(2),
                unknown_attempts: 0,
                overflowed: false,
            },
            output: MetricSummary {
                known: Some(4),
                unknown_attempts: 0,
                overflowed: false,
            },
            reasoning: MetricSummary {
                known: Some(2),
                unknown_attempts: 0,
                overflowed: false,
            },
            estimated_cost_usd: MetricSummary {
                known: Some(0.25),
                unknown_attempts: 0,
                overflowed: false,
            },
        };
        for (name, attempts, expected) in [
            (
                "complete",
                [(Some(usage), Some(0.125)); 2],
                complete.clone(),
            ),
            (
                "missing token",
                [
                    (Some(usage), Some(0.125)),
                    (
                        Some(TokenUsage {
                            cache_write: None,
                            ..usage
                        }),
                        Some(0.125),
                    ),
                ],
                UsageSummary {
                    kind: UsageCompleteness::Partial,
                    cache_write: MetricSummary {
                        known: Some(1),
                        unknown_attempts: 1,
                        overflowed: false,
                    },
                    ..complete.clone()
                },
            ),
            (
                "missing cost",
                [(Some(usage), Some(0.125)), (Some(usage), None)],
                UsageSummary {
                    kind: UsageCompleteness::Partial,
                    estimated_cost_usd: MetricSummary {
                        known: Some(0.125),
                        unknown_attempts: 1,
                        overflowed: false,
                    },
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
                        Some(0.125),
                    ),
                    (
                        Some(TokenUsage {
                            input: Some(1),
                            ..usage
                        }),
                        Some(0.125),
                    ),
                ],
                UsageSummary {
                    kind: UsageCompleteness::Partial,
                    input: MetricSummary {
                        known: Some(u64::MAX),
                        unknown_attempts: 0,
                        overflowed: true,
                    },
                    ..complete.clone()
                },
            ),
            (
                "cost overflow",
                [(Some(usage), Some(f64::MAX)); 2],
                UsageSummary {
                    kind: UsageCompleteness::Partial,
                    estimated_cost_usd: MetricSummary {
                        known: Some(f64::MAX),
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
}
