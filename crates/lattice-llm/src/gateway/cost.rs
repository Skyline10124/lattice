// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 在终态交付前读取有效价格，费用未知不改变尝试的真实结局。

use std::path::{Path, PathBuf};

use futures::{StreamExt, stream};
use lattice_conversation::{CostEstimate, UsageCompleteness};

use crate::metadata::{Cost, Metadata};
use crate::provider::config::ProviderRegistry;
use crate::{LlmEvent, LlmStream, ModelTarget, ReplyOutcome, TokenUsage};

#[derive(Debug, PartialEq)]
enum Estimate {
    Exact(f64),
    Approx(f64),
    Unknown,
}

impl Estimate {
    fn into_cost(self) -> Option<CostEstimate> {
        match self {
            Self::Exact(usd) => Some(CostEstimate { usd, exact: true }),
            Self::Approx(usd) => Some(CostEstimate { usd, exact: false }),
            Self::Unknown => None,
        }
    }
}

pub(super) fn with_estimated_cost(
    input: LlmStream,
    directory: PathBuf,
    target: ModelTarget,
) -> LlmStream {
    Box::pin(stream::unfold(
        (input, directory, target),
        |(mut input, directory, target)| async move {
            let mut event = input.next().await?;
            if let LlmEvent::Finished { outcome, report } = &mut event {
                let (kind, estimate) = if let Some(usage) = report.usage {
                    let directory = directory.clone();
                    let target = target.clone();
                    let dispatch = tracing::dispatcher::get_default(Clone::clone);
                    tokio::task::spawn_blocking(move || {
                        tracing::dispatcher::with_default(&dispatch, || {
                            estimate_at_finish(&directory, &target, usage)
                        })
                    })
                    .await
                    .unwrap_or_else(|_| unavailable("cost estimation task failed"))
                } else {
                    (UsageCompleteness::Partial, Estimate::Unknown)
                };
                report.usage_kind = kind;
                report.cost = estimate.into_cost();
                if matches!(outcome, ReplyOutcome::Interrupted { .. }) {
                    report.usage_kind = UsageCompleteness::Partial;
                    if let Some(cost) = &mut report.cost {
                        cost.exact = false;
                    }
                }
            }
            Some((event, (input, directory, target)))
        },
    ))
}

fn estimate_at_finish(
    directory: &Path,
    target: &ModelTarget,
    usage: TokenUsage,
) -> (UsageCompleteness, Estimate) {
    let registry = match ProviderRegistry::load(directory.join("providers.toml")) {
        Ok(registry) => registry,
        Err(_) => return unavailable("provider configuration unavailable"),
    };
    let Some(provider) = registry.get(&target.provider) else {
        return unavailable("provider unavailable");
    };
    let models = Metadata::load(directory).merge(provider);
    let Some(cost) = models
        .get(&target.model)
        .and_then(|model| model.cost.as_ref())
    else {
        // 模型无价不影响已报告的用量完整性。
        tracing::warn!(
            reason = "model price unavailable",
            "attempt cost estimate unavailable"
        );
        return (UsageCompleteness::Complete, Estimate::Unknown);
    };
    estimate(usage, cost)
}

fn unavailable(reason: &'static str) -> (UsageCompleteness, Estimate) {
    // 原始配置错误可能带有秘密；诊断只使用固定原因。
    tracing::warn!(reason, "attempt cost estimate unavailable");
    (UsageCompleteness::Partial, Estimate::Unknown)
}

fn estimate(usage: TokenUsage, cost: &Cost) -> (UsageCompleteness, Estimate) {
    let prices = [cost.input, cost.cache_read, cost.cache_write, cost.output];
    let reported = [
        usage.input,
        usage.cache_read,
        usage.cache_write,
        usage.output,
    ];
    let partial = reported
        .iter()
        .zip(prices)
        .any(|(count, price)| price.is_some() && count.is_none());
    let kind = if partial {
        UsageCompleteness::Partial
    } else {
        UsageCompleteness::Complete
    };
    if !reported
        .iter()
        .zip(prices)
        .any(|(count, price)| count.is_some() && price.is_some())
    {
        return (kind, Estimate::Unknown);
    }
    let approximate = partial
        || reported
            .iter()
            .zip(prices)
            .any(|(count, price)| price.is_none() && count.is_some_and(|count| count > 0));
    if prices
        .iter()
        .flatten()
        .any(|price| !price.is_finite() || *price < 0.0)
    {
        tracing::warn!(
            reason = "invalid model price",
            "attempt cost estimate unavailable"
        );
        return (kind, Estimate::Unknown);
    }
    let read = usage.cache_read.unwrap_or(0);
    let write = usage.cache_write.unwrap_or(0);
    // 只钳制计费差额；推理已含在输出中，原始计数保持不变。
    let counts = [
        usage
            .input
            .unwrap_or(0)
            .saturating_sub(read)
            .saturating_sub(write),
        read,
        write,
        usage.output.unwrap_or(0),
    ];
    let usd: f64 = counts
        .into_iter()
        .zip(prices)
        // 先换算计数单位，避免乘积在最终金额仍可表示时溢出。
        .filter_map(|(count, price)| price.map(|price| (count as f64 / 1_000_000.0) * price))
        .sum();
    if usd.is_finite() && usd >= 0.0 {
        (
            kind,
            if approximate {
                Estimate::Approx(usd)
            } else {
                Estimate::Exact(usd)
            },
        )
    } else {
        tracing::warn!(
            reason = "cost estimate exceeds finite range",
            "attempt cost estimate unavailable"
        );
        (kind, Estimate::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Estimate::{Approx, Exact, Unknown};
    use UsageCompleteness::{Complete, Partial};

    fn usage() -> TokenUsage {
        TokenUsage {
            input: Some(1000),
            cache_read: Some(200),
            cache_write: Some(100),
            output: Some(100),
            reasoning: Some(20),
        }
    }

    fn prices() -> Cost {
        Cost {
            input: Some(2.0),
            cache_read: Some(0.5),
            cache_write: Some(3.0),
            output: Some(10.0),
        }
    }

    fn assert_estimate(
        name: &str,
        usage: TokenUsage,
        prices: &Cost,
        kind: UsageCompleteness,
        expected: Estimate,
    ) -> Option<CostEstimate> {
        let (actual_kind, actual) = estimate(usage, prices);
        assert_eq!(actual_kind, kind, "{name}");
        let actual = actual.into_cost();
        let expected = expected.into_cost();
        assert_eq!(
            actual.map(|cost| cost.exact),
            expected.map(|cost| cost.exact),
            "{name}"
        );
        if let (Some(actual), Some(expected)) = (actual, expected) {
            assert!(
                (actual.usd - expected.usd).abs() <= expected.usd * 8.0 * f64::EPSILON,
                "{name}: {} != {}",
                actual.usd,
                expected.usd
            );
        }
        actual
    }

    #[test]
    fn token_classes_compare_reported_counts_with_their_own_prices() {
        for (name, usage, cost, kind, expected) in [
            ("all classes", usage(), prices(), Complete, Exact(0.0028)),
            (
                "read price missing",
                usage(),
                Cost {
                    cache_read: None,
                    ..prices()
                },
                Complete,
                Approx(0.0027),
            ),
            (
                "write price missing",
                usage(),
                Cost {
                    cache_write: None,
                    ..prices()
                },
                Complete,
                Approx(0.0025),
            ),
            (
                "input price missing",
                usage(),
                Cost {
                    input: None,
                    ..prices()
                },
                Complete,
                Approx(0.0014),
            ),
            (
                "output price missing",
                usage(),
                Cost {
                    output: None,
                    ..prices()
                },
                Complete,
                Approx(0.0018),
            ),
            (
                "reasoning count missing",
                TokenUsage {
                    reasoning: None,
                    ..usage()
                },
                prices(),
                Complete,
                Exact(0.0028),
            ),
            (
                "zero detail prices",
                usage(),
                Cost {
                    cache_read: Some(0.0),
                    cache_write: Some(0.0),
                    ..prices()
                },
                Complete,
                Exact(0.0024),
            ),
            (
                "zero base prices with unpriced positive details",
                usage(),
                Cost {
                    input: Some(0.0),
                    output: Some(0.0),
                    ..Default::default()
                },
                Complete,
                Approx(0.0),
            ),
            (
                "zero prices with every priced count reported",
                TokenUsage {
                    cache_read: None,
                    cache_write: None,
                    ..usage()
                },
                Cost {
                    input: Some(0.0),
                    output: Some(0.0),
                    ..Default::default()
                },
                Complete,
                Exact(0.0),
            ),
            (
                "zero counts",
                TokenUsage {
                    input: Some(0),
                    output: Some(0),
                    cache_read: Some(0),
                    cache_write: Some(0),
                    ..Default::default()
                },
                prices(),
                Complete,
                Exact(0.0),
            ),
            (
                "zero read count with price",
                TokenUsage {
                    cache_read: Some(0),
                    ..usage()
                },
                prices(),
                Complete,
                Exact(0.0031),
            ),
            (
                "zero read count without price",
                TokenUsage {
                    cache_read: Some(0),
                    ..usage()
                },
                Cost {
                    cache_read: None,
                    ..prices()
                },
                Complete,
                Exact(0.0031),
            ),
            (
                "zero write count without price",
                TokenUsage {
                    cache_write: Some(0),
                    ..usage()
                },
                Cost {
                    cache_write: None,
                    ..prices()
                },
                Complete,
                Exact(0.0027),
            ),
            (
                "inconsistent counts",
                TokenUsage {
                    input: Some(1),
                    output: Some(1),
                    ..usage()
                },
                prices(),
                Complete,
                Exact(0.00041),
            ),
            (
                "all prices missing",
                usage(),
                Cost::default(),
                Complete,
                Unknown,
            ),
            (
                "reasoning count alone does not price a billed item",
                TokenUsage {
                    reasoning: Some(20),
                    ..Default::default()
                },
                Cost {
                    output: Some(10.0),
                    ..Default::default()
                },
                Partial,
                Unknown,
            ),
            (
                "only an unpriced read count is reported",
                TokenUsage {
                    cache_read: Some(200),
                    ..Default::default()
                },
                Cost {
                    input: Some(2.0),
                    output: Some(10.0),
                    ..Default::default()
                },
                Partial,
                Unknown,
            ),
        ] {
            assert_estimate(name, usage, &cost, kind, expected);
        }
    }

    #[test]
    fn missing_counts_are_partial_only_when_the_matching_price_exists() {
        // 缺读计数时会多算，缺写计数时会少算；估算值不承诺方向。
        for (field, expected) in [(0, 0.0014), (1, 0.0018), (2, 0.0031), (3, 0.0027)] {
            let mut usage = usage();
            let mut cost = prices();
            let count = match field {
                0 => &mut usage.input,
                1 => &mut usage.output,
                2 => &mut usage.cache_read,
                _ => &mut usage.cache_write,
            };
            *count = None;
            assert_estimate(
                &format!("priced field {field}"),
                usage,
                &cost,
                Partial,
                Approx(expected),
            );
            match field {
                0 => cost.input = None,
                1 => cost.output = None,
                2 => cost.cache_read = None,
                _ => cost.cache_write = None,
            }
            assert_estimate(
                &format!("unpriced field {field}"),
                usage,
                &cost,
                Complete,
                Exact(expected),
            );
        }
    }

    #[test]
    fn positive_unpriced_counts_do_not_increase_partial_attempts() {
        let usage = TokenUsage {
            cache_write: Some(5),
            ..usage()
        };
        let cost = assert_estimate(
            "positive unpriced write",
            usage,
            &Cost {
                cache_write: None,
                ..prices()
            },
            Complete,
            Approx(0.00269),
        );
        let summary =
            lattice_conversation::UsageSummary::from_attempts([(Some(usage), Complete, cost)]);
        assert_eq!(summary.kind, Complete);
        assert_eq!(summary.partial_attempts, 0);
        assert!(!summary.cost.known.unwrap().exact);
    }

    #[test]
    fn invalid_prices_for_billed_items_produce_unknown_cost() {
        for invalid in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            for field in 0..4 {
                let mut cost = prices();
                let price = match field {
                    0 => &mut cost.input,
                    1 => &mut cost.output,
                    2 => &mut cost.cache_read,
                    _ => &mut cost.cache_write,
                };
                *price = Some(invalid);
                assert_estimate(
                    &format!("field {field}: {invalid}"),
                    usage(),
                    &cost,
                    Complete,
                    Unknown,
                );
            }
        }
    }

    #[test]
    fn extreme_counts_and_prices_do_not_overflow_integer_or_intermediate_products() {
        let maximal = TokenUsage {
            input: Some(1),
            output: Some(1),
            cache_read: Some(u64::MAX),
            cache_write: Some(u64::MAX),
            reasoning: Some(u64::MAX),
        };
        let unit = Cost {
            input: Some(1.0),
            output: Some(1.0),
            cache_read: Some(1.0),
            cache_write: Some(1.0),
        };
        assert_estimate(
            "maximal counts",
            maximal,
            &unit,
            Complete,
            Exact(36_893_488_147_419.1),
        );
        let expensive = Cost {
            input: Some(f64::MAX),
            ..unit
        };
        let single = TokenUsage {
            input: Some(1),
            output: Some(0),
            cache_read: Some(0),
            cache_write: Some(0),
            ..Default::default()
        };
        for (input, expected) in [
            (1, 1.797_693_134_862_316e302),
            (2, 3.595_386_269_724_632e302),
        ] {
            let (_, estimate) = estimate(
                TokenUsage {
                    input: Some(input),
                    ..single
                },
                &expensive,
            );
            let Estimate::Exact(usd) = estimate else {
                panic!("input={input}: {estimate:?}")
            };
            assert!(
                (usd / expected - 1.0).abs() <= 4.0 * f64::EPSILON,
                "input={input}: {usd} != {expected}"
            );
        }
        assert_estimate(
            "term overflow",
            TokenUsage {
                input: Some(u64::MAX),
                ..single
            },
            &expensive,
            Complete,
            Unknown,
        );
        assert_estimate(
            "sum overflow",
            TokenUsage {
                input: Some(1_000_000),
                output: Some(1_000_000),
                ..single
            },
            &Cost {
                output: Some(f64::MAX),
                ..expensive
            },
            Complete,
            Unknown,
        );
    }
}
