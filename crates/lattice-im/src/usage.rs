// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `/usage` 四行会话用量格式化：直接展示会话库派生的 `path_total` 与 `session_total`，不重算各轮累加。

use std::collections::BTreeSet;

use lattice_ipc::{CostSummary, SessionUsage, TokenSummary, UsageCompleteness, UsageSummary};

/// 将会话用量渲染为严格四行的纯文本摘要。
pub(crate) fn format_session_usage(usage: &SessionUsage) -> String {
    let line_session = format!(
        "会话：{}；当前路径：{} 轮",
        usage.session_id,
        usage.turns.len()
    );

    let mut seen = BTreeSet::new();
    let mut models = Vec::new();
    for row in &usage.turns {
        let label = format!("{}/{}", row.provider, row.model);
        if seen.insert(label.clone()) {
            models.push(label);
        }
    }
    let models_text = if models.is_empty() {
        "-".to_owned()
    } else {
        models.join("、")
    };
    let line_models = format!("模型：{models_text}");

    let line_path = format_total_line("当前路径合计", &usage.path_total);
    let line_session_total = format_total_line("全会话合计", &usage.session_total);

    format!("{line_session}\n{line_models}\n{line_path}\n{line_session_total}")
}

fn format_total_line(label: &str, total: &UsageSummary) -> String {
    let input = format_token_metric(&total.input);
    let cache_write = format_token_metric(&total.cache_write);
    let cache_read = format_token_metric(&total.cache_read);
    let output = format_token_metric(&total.output);
    let cache_hit_rate = format_cache_hit_rate(total);
    let cost = format_cost_metric(&total.cost);
    let incomplete_suffix = match total.kind {
        UsageCompleteness::Complete => "",
        UsageCompleteness::Partial => "（不完整）",
    };

    format!(
        "{label}：输入 {input}；缓存写入 {cache_write}；缓存读取 {cache_read}；输出 {output}；缓存命中率 {cache_hit_rate}；费用 {cost}{incomplete_suffix}"
    )
}

fn format_token_metric(metric: &TokenSummary) -> String {
    let Some(value) = metric.known else {
        return "-".into();
    };
    if metric.overflowed {
        format!("{value} (overflow)")
    } else {
        value.to_string()
    }
}

/// 缓存命中率显示条件与 TUI statusline 一致：
/// 仅在有尝试、`input` 与 `cache_read` 均 `complete` 且未溢出、`input > 0` 时显示四舍五入整数百分比，
/// 保留超过 100% 的报告比例而不钳制。
fn format_cache_hit_rate(total: &UsageSummary) -> String {
    if total.attempts > 0
        && total.input.complete
        && total.cache_read.complete
        && !total.input.overflowed
        && !total.cache_read.overflowed
        && let (Some(input), Some(cached)) = (total.input.known, total.cache_read.known)
        && input > 0
    {
        format!("{:.0}%", (cached as f64 / input as f64 * 100.0).round())
    } else {
        "-".into()
    }
}

fn format_cost_metric(cost: &CostSummary) -> String {
    let base = match cost.known {
        None => "$?".to_owned(),
        Some(amount) => {
            let prefix = if amount.exact { "" } else { "≈" };
            if amount.usd > 0.0 && amount.usd < 0.01 {
                format!("{prefix}<$0.01")
            } else {
                format!("{prefix}${:.2}", amount.usd)
            }
        }
    };
    if cost.overflowed {
        format!("{base} (overflow)")
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use lattice_ipc::{CostEstimate, ModelId, NodeId, ProviderId, TurnUsageRow};

    use super::*;
    use crate::identity::{new_session_id, new_turn_id};

    fn token(known: Option<u64>, complete: bool, overflowed: bool) -> TokenSummary {
        TokenSummary {
            known,
            complete,
            overflowed,
        }
    }

    #[test]
    fn formats_four_lines_preserving_dedup_order_and_raw_totals() {
        let sid = new_session_id();
        let p_openai = ProviderId::parse("openai").unwrap();
        let p_anthropic = ProviderId::parse("anthropic").unwrap();
        let m_gpt = ModelId::parse("gpt-5").unwrap();
        let m_claude = ModelId::parse("claude-sonnet-4").unwrap();

        let dummy_row_usage = UsageSummary {
            attempts: 1,
            partial_attempts: 0,
            kind: UsageCompleteness::Complete,
            input: token(Some(1), true, false),
            cache_read: token(Some(0), true, false),
            cache_write: token(Some(0), true, false),
            output: token(Some(1), true, false),
            reasoning: token(Some(0), true, false),
            cost: CostSummary {
                known: Some(CostEstimate {
                    usd: 0.01,
                    exact: true,
                }),
                unknown_attempts: 0,
                overflowed: false,
            },
        };

        // 故意让 path_total 与 session_total 不等于各轮简单相加（如 rewind 后的全会话分支），验证直出原值。
        let usage = SessionUsage {
            session_id: sid,
            position: NodeId(3),
            turns: vec![
                TurnUsageRow {
                    turn_id: new_turn_id(),
                    ordinal: 1,
                    provider: p_openai.clone(),
                    model: m_gpt.clone(),
                    usage: dummy_row_usage.clone(),
                },
                TurnUsageRow {
                    turn_id: new_turn_id(),
                    ordinal: 2,
                    provider: p_anthropic,
                    model: m_claude,
                    usage: dummy_row_usage.clone(),
                },
                TurnUsageRow {
                    turn_id: new_turn_id(),
                    ordinal: 3,
                    provider: p_openai,
                    model: m_gpt,
                    usage: dummy_row_usage,
                },
            ],
            path_total: UsageSummary {
                attempts: 2,
                partial_attempts: 0,
                kind: UsageCompleteness::Complete,
                input: token(Some(1000), true, false),
                cache_write: token(Some(0), true, false),
                cache_read: token(Some(800), true, false),
                output: token(Some(250), true, false),
                reasoning: token(None, false, false),
                cost: CostSummary {
                    known: Some(CostEstimate {
                        usd: 0.004,
                        exact: true,
                    }),
                    unknown_attempts: 0,
                    overflowed: false,
                },
            },
            session_total: UsageSummary {
                attempts: 4,
                partial_attempts: 1,
                kind: UsageCompleteness::Partial,
                input: token(Some(100), true, false),
                cache_write: token(None, false, false),
                cache_read: token(Some(150), true, false),
                output: token(Some(u64::MAX), false, true),
                reasoning: token(None, false, false),
                cost: CostSummary {
                    known: Some(CostEstimate {
                        usd: 0.05,
                        exact: false,
                    }),
                    unknown_attempts: 1,
                    overflowed: false,
                },
            },
        };

        let text = format_session_usage(&usage);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "/usage 回复必须严格为四行: {text}");
        assert_eq!(lines[0], format!("会话：{sid}；当前路径：3 轮"));
        assert_eq!(lines[1], "模型：openai/gpt-5、anthropic/claude-sonnet-4");
        assert_eq!(
            lines[2],
            "当前路径合计：输入 1000；缓存写入 0；缓存读取 800；输出 250；缓存命中率 80%；费用 <$0.01"
        );
        // session_total 同时验证：未报告 `-`、超 100% 命中率 150% 不钳制、溢出 `(overflow)`、估算 `≈` 与 `（不完整）`
        assert_eq!(
            lines[3],
            format!(
                "全会话合计：输入 100；缓存写入 -；缓存读取 150；输出 {} (overflow)；缓存命中率 150%；费用 ≈$0.05（不完整）",
                u64::MAX
            )
        );
    }

    #[test]
    fn hides_cache_ratio_on_incomplete_or_zero_input_and_formats_unknown_cost() {
        let sid = new_session_id();
        let empty = SessionUsage {
            session_id: sid,
            position: NodeId(0),
            turns: Vec::new(),
            path_total: UsageSummary {
                attempts: 0,
                partial_attempts: 0,
                kind: UsageCompleteness::Complete,
                input: token(Some(0), true, false),
                cache_write: token(Some(0), true, false),
                cache_read: token(Some(0), true, false),
                output: token(Some(0), true, false),
                reasoning: token(Some(0), true, false),
                cost: CostSummary {
                    known: Some(CostEstimate {
                        usd: 0.0,
                        exact: true,
                    }),
                    unknown_attempts: 0,
                    overflowed: false,
                },
            },
            session_total: UsageSummary {
                attempts: 2,
                partial_attempts: 1,
                kind: UsageCompleteness::Partial,
                input: token(Some(100), true, false),
                cache_write: token(None, false, false),
                // 第二次尝试未报告 cache_read -> complete 为 false，即使 known 有值也不显示缓存命中率
                cache_read: token(Some(50), false, false),
                output: token(None, false, false),
                reasoning: token(None, false, false),
                cost: CostSummary {
                    known: None,
                    unknown_attempts: 2,
                    overflowed: false,
                },
            },
        };

        let text = format_session_usage(&empty);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], format!("会话：{sid}；当前路径：0 轮"));
        assert_eq!(lines[1], "模型：-");
        assert_eq!(
            lines[2],
            "当前路径合计：输入 0；缓存写入 0；缓存读取 0；输出 0；缓存命中率 -；费用 $0.00"
        );
        assert_eq!(
            lines[3],
            "全会话合计：输入 100；缓存写入 -；缓存读取 50；输出 -；缓存命中率 -；费用 $?（不完整）"
        );
    }
}
