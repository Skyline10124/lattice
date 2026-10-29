// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 在终态交付前读取有效价格，费用未知不改变尝试的真实结局。

use std::path::{Path, PathBuf};

use futures::{StreamExt, stream};

use crate::metadata::{Cost, Metadata};
use crate::provider::config::ProviderRegistry;
use crate::{LlmEvent, LlmStream, ModelTarget, TokenUsage};

pub(super) fn with_estimated_cost(
    input: LlmStream,
    directory: PathBuf,
    target: ModelTarget,
) -> LlmStream {
    Box::pin(stream::unfold(
        (input, directory, target),
        |(mut input, directory, target)| async move {
            let mut event = input.next().await?;
            if let LlmEvent::Finished { report, .. } = &mut event
                && let Some(usage) = report.usage
            {
                let directory = directory.clone();
                let target = target.clone();
                let estimate = tokio::task::spawn_blocking(move || {
                    estimate_at_finish(&directory, &target, usage)
                })
                .await
                .unwrap_or(Err("cost estimation task failed"));
                match estimate {
                    Ok(usd) => report.estimated_cost_usd = Some(usd),
                    // 原始配置错误可能带有秘密；诊断只使用固定原因。
                    Err(reason) => tracing::warn!(reason, "attempt cost estimate unavailable"),
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
) -> Result<f64, &'static str> {
    let registry = ProviderRegistry::load(directory.join("providers.toml"))
        .map_err(|_| "provider configuration unavailable")?;
    let provider = registry
        .get(&target.provider)
        .ok_or("provider unavailable")?;
    let models = Metadata::load(directory).merge(provider);
    let cost = models
        .get(&target.model)
        .and_then(|model| model.cost.as_ref())
        .ok_or("model price unavailable")?;
    estimate(usage, cost)
}

fn estimate(usage: TokenUsage, cost: &Cost) -> Result<f64, &'static str> {
    let input = usage.input.ok_or("input usage unavailable")?;
    let output = usage.output.ok_or("output usage unavailable")?;
    let input_price = cost.input.ok_or("input price unavailable")?;
    let output_price = cost.output.ok_or("output price unavailable")?;
    let prices = [
        input_price,
        cost.cache_read.unwrap_or(input_price),
        cost.cache_write.unwrap_or(input_price),
        output_price,
        cost.reasoning.unwrap_or(output_price),
    ];
    if prices
        .iter()
        .any(|price| !price.is_finite() || *price < 0.0)
    {
        return Err("invalid model price");
    }
    let read = usage.cache_read.unwrap_or(0);
    let write = usage.cache_write.unwrap_or(0);
    let reasoning = usage.reasoning.unwrap_or(0);
    // 只钳制计费差额；保留 provider 报告的缓存与推理计数。
    let counts = [
        input.saturating_sub(read).saturating_sub(write),
        read,
        write,
        output.saturating_sub(reasoning),
        reasoning,
    ];
    let usd: f64 = counts
        .into_iter()
        .zip(prices)
        // 先换算计数单位，避免乘积在最终金额仍可表示时溢出。
        .map(|(count, price)| (count as f64 / 1_000_000.0) * price)
        .sum();
    if usd.is_finite() && usd >= 0.0 {
        Ok(usd)
    } else {
        Err("cost estimate exceeds finite range")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            reasoning: Some(20.0),
        }
    }

    #[test]
    fn token_classes_use_their_own_prices_and_only_missing_details_fall_back() {
        let base_usage = usage();
        let base_prices = prices();
        for (name, usage, cost, expected) in [
            ("all classes", base_usage, base_prices.clone(), 0.003),
            (
                "read price missing",
                base_usage,
                Cost {
                    cache_read: None,
                    ..prices()
                },
                0.0033,
            ),
            (
                "write price missing",
                base_usage,
                Cost {
                    cache_write: None,
                    ..prices()
                },
                0.0029,
            ),
            (
                "reasoning price missing",
                base_usage,
                Cost {
                    reasoning: None,
                    ..prices()
                },
                0.0028,
            ),
            (
                "read count missing",
                TokenUsage {
                    cache_read: None,
                    ..usage()
                },
                base_prices.clone(),
                0.0033,
            ),
            (
                "write count missing",
                TokenUsage {
                    cache_write: None,
                    ..usage()
                },
                base_prices.clone(),
                0.0029,
            ),
            (
                "reasoning count missing",
                TokenUsage {
                    reasoning: None,
                    ..usage()
                },
                base_prices.clone(),
                0.0028,
            ),
            (
                "zero detail prices",
                base_usage,
                Cost {
                    cache_read: Some(0.0),
                    cache_write: Some(0.0),
                    reasoning: Some(0.0),
                    ..prices()
                },
                0.0022,
            ),
            (
                "zero base prices",
                base_usage,
                Cost {
                    input: Some(0.0),
                    output: Some(0.0),
                    ..Default::default()
                },
                0.0,
            ),
            (
                "zero counts",
                TokenUsage {
                    input: Some(0),
                    output: Some(0),
                    ..Default::default()
                },
                base_prices.clone(),
                0.0,
            ),
            (
                "small amount",
                TokenUsage {
                    input: Some(1),
                    output: Some(0),
                    ..Default::default()
                },
                Cost {
                    input: Some(0.000001),
                    ..prices()
                },
                1e-12,
            ),
            (
                "inconsistent counts",
                TokenUsage {
                    input: Some(1),
                    output: Some(1),
                    ..usage()
                },
                base_prices,
                0.0008,
            ),
        ] {
            let actual = estimate(usage, &cost).unwrap_or_else(|error| panic!("{name}: {error}"));
            assert!(
                (actual - expected).abs() <= expected * 1e-12,
                "{name}: {actual} != {expected}"
            );
        }
    }

    #[test]
    fn missing_base_values_and_invalid_prices_do_not_produce_an_estimate() {
        for incomplete in [
            TokenUsage {
                input: None,
                ..usage()
            },
            TokenUsage {
                output: None,
                ..usage()
            },
        ] {
            assert!(estimate(incomplete, &prices()).is_err());
        }
        for incomplete in [
            Cost {
                input: None,
                ..prices()
            },
            Cost {
                output: None,
                ..prices()
            },
        ] {
            assert!(estimate(usage(), &incomplete).is_err());
        }
        for invalid in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            for field in 0..5 {
                let mut cost = prices();
                let fields = [
                    &mut cost.input,
                    &mut cost.output,
                    &mut cost.cache_read,
                    &mut cost.cache_write,
                    &mut cost.reasoning,
                ];
                *fields.into_iter().nth(field).unwrap() = Some(invalid);
                assert!(
                    estimate(usage(), &cost).is_err(),
                    "field {field}: {invalid}"
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
            ..Default::default()
        };
        let usd = estimate(maximal, &unit).unwrap();
        assert!((usd - 55_340_232_221_128.65).abs() < 0.01);
        let expensive = Cost {
            input: Some(f64::MAX),
            ..unit
        };
        let single = TokenUsage {
            input: Some(1),
            output: Some(0),
            ..Default::default()
        };
        for (input, expected) in [
            (1, 1.797_693_134_862_316e302),
            (2, 3.595_386_269_724_632e302),
        ] {
            let usd = estimate(
                TokenUsage {
                    input: Some(input),
                    ..single
                },
                &expensive,
            )
            .unwrap();
            assert!(
                (usd / expected - 1.0).abs() <= 4.0 * f64::EPSILON,
                "input={input}: {usd} != {expected}"
            );
        }
        assert!(
            estimate(
                TokenUsage {
                    input: Some(u64::MAX),
                    ..single
                },
                &expensive
            )
            .is_err()
        );
        assert!(
            estimate(
                TokenUsage {
                    input: Some(1_000_000),
                    output: Some(1_000_000),
                    ..single
                },
                &Cost {
                    output: Some(f64::MAX),
                    ..expensive
                }
            )
            .is_err()
        );
    }
}
