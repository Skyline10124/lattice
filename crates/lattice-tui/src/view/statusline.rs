// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;

pub(super) fn render(frame: &mut Frame, app: &App, area: Rect) {
    frame.render_widget(
        Paragraph::new(line(app, usize::from(area.width))).style(status_surface_style()),
        area,
    );
}

pub(super) fn line(app: &App, width: usize) -> Line<'static> {
    let dim = role_style(Role::DimMeta);
    if width < 2 {
        return Line::from(Span::styled(" ".repeat(width), dim));
    }
    let below = app.viewport.hidden_below();
    let session = session_field(app.ctx.session_id);
    let mut left = [
        Some(session.as_str()),
        Some(app.ctx.active_agent.as_str()),
        app.model.as_deref(),
        app.provider.as_deref(),
    ];
    let mut right = [
        (!app.viewport.following()).then(|| format!("↓ {below} 行")),
        ctrl_c_visible(app).then(|| CTRL_C_HINT.into()),
        (app.queue.is_editing() || (matches!(app.focus, Focus::Dock) && app.dock.shows_input()))
            .then(|| {
                if app.kitty_keyboard {
                    "S-Enter 换行"
                } else {
                    "Alt+Enter 换行"
                }
                .into()
            }),
        app.turn_usage.as_ref().and_then(format_turn),
        app.context_tokens.map(format_context),
        Some(phase_and_connection(app)),
    ];
    let room = width.saturating_sub(2);
    // 先省略两个按键提示，再从最宽的字段开始省略；只剩一个字段时交给下方裁剪。
    for slot in [2, 1] {
        if fits(&left, &right, room) {
            break;
        }
        right[slot] = None;
    }
    while !fits(&left, &right, room) {
        let width_of = |slot: Slot| match slot {
            Slot::Left(index) => left[index].map(display_width),
            Slot::Right(index) => right[index].as_deref().map(display_width),
        };
        let present = TIE_ORDER
            .iter()
            .filter_map(|&slot| width_of(slot).map(|width| (slot, width)))
            .collect::<Vec<_>>();
        let widest = present.iter().map(|&(_, width)| width).max().unwrap_or(0);
        let Some(&(slot, _)) = present
            .iter()
            .find(|&&(_, width)| width == widest)
            .filter(|_| present.len() > 1)
        else {
            break;
        };
        match slot {
            Slot::Left(index) => left[index] = None,
            Slot::Right(index) => right[index] = None,
        }
    }
    let left = left.into_iter().flatten().collect::<Vec<_>>().join(" · ");
    let right_w = joined_width(right.iter().filter_map(|s| s.as_deref()));
    let gap = room.saturating_sub(display_width(&left) + right_w);
    let mut spans = vec![Span::styled(left, dim), Span::styled(" ".repeat(gap), dim)];
    let mut first = true;
    for (index, field) in right.into_iter().enumerate() {
        let Some(text) = field else {
            continue;
        };
        if !first {
            spans.push(Span::styled(" · ", dim));
        }
        first = false;
        if index == 5 {
            let phase = phase_text(app.phase);
            spans.push(Span::styled(phase, role_style(phase_role(app.phase))));
            spans.push(Span::styled(text[phase.len()..].to_string(), dim));
        } else {
            spans.push(Span::styled(text, dim));
        }
    }
    let mut out = vec![Span::styled(" ", dim)];
    let mut remaining = room;
    for span in spans {
        let text = truncate_display(&span.content, remaining);
        remaining = remaining.saturating_sub(display_width(&text));
        let clipped = text.len() < span.content.len();
        out.push(Span::styled(text, span.style));
        if clipped {
            break;
        }
    }
    out.push(Span::styled(" ".repeat(remaining + 1), dim));
    Line::from(out)
}

#[derive(Clone, Copy)]
enum Slot {
    Left(usize),
    Right(usize),
}

/// 等宽字段的省略先后：上下文、本轮用量、提供商、模型、session、agent、翻看提示、phase。
const TIE_ORDER: [Slot; 8] = [
    Slot::Right(4),
    Slot::Right(3),
    Slot::Left(3),
    Slot::Left(2),
    Slot::Left(0),
    Slot::Left(1),
    Slot::Right(0),
    Slot::Right(5),
];

/// SessionId 只显示前 8 个字符；新会话在首个请求被接受前没有身份。
fn session_field(id: Option<lattice_api::local::SessionId>) -> String {
    id.map_or_else(
        || "未接受".into(),
        |id| id.to_string().chars().take(8).collect(),
    )
}

fn fits(left: &[Option<&str>], right: &[Option<String>], room: usize) -> bool {
    let left_w = joined_width(left.iter().copied().flatten());
    let right_w = joined_width(right.iter().filter_map(|s| s.as_deref()));
    left_w + right_w + usize::from(left_w > 0 && right_w > 0) <= room
}

fn format_context(tokens: u64) -> String {
    format!("上下文 {}", format_tokens(tokens))
}

fn format_tokens(tokens: u64) -> String {
    if tokens < 1000 {
        tokens.to_string()
    } else {
        format!("{:.1}k", tokens as f64 / 1000.0)
    }
}

fn format_turn(usage: &lattice_api::UsageSummary) -> Option<String> {
    let mut parts = Vec::new();
    if usage.attempts > 0 {
        for (prefix, metric) in [("↑", &usage.input), ("↓", &usage.output)] {
            if metric.is_complete()
                && let Some(tokens) = metric.known
            {
                parts.push(format!("{prefix}{}", format_tokens(tokens)));
            }
        }
        if usage.input.is_complete()
            && usage.cache_read.is_complete()
            && let (Some(input), Some(cached)) = (usage.input.known, usage.cache_read.known)
            && input > 0
        {
            parts.push(format!(
                "缓存 {:.0}%",
                (cached as f64 / input as f64 * 100.0).round()
            ));
        }
    }
    let cost = &usage.estimated_cost_usd;
    if !cost.overflowed
        && let Some(amount) = cost.known
    {
        let suffix = if cost.is_complete() { "" } else { "+" };
        parts.push(format!("≈${amount:.2}{suffix}"));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn summary(
        attempts: impl IntoIterator<Item = (Value, Option<f64>)>,
    ) -> lattice_api::UsageSummary {
        lattice_api::UsageSummary::from_attempts(
            attempts
                .into_iter()
                .map(|(usage, cost)| (serde_json::from_value(usage).unwrap(), cost)),
        )
    }

    #[test]
    fn turn_fields_have_independent_coverage() {
        let partial = summary([
            (
                json!({"input":1000,"output":100,"cache_read":200}),
                Some(0.024),
            ),
            (json!({"output":20,"cache_read":0}), None),
        ]);
        assert_eq!(format_turn(&partial).as_deref(), Some("↓120 ≈$0.02+"));
        let missing_reasoning = summary([(
            json!({"input":12300,"output":1200,"cache_read":9840}),
            Some(0.04),
        )]);
        assert_eq!(
            format_turn(&missing_reasoning).as_deref(),
            Some("↑12.3k ↓1.2k 缓存 80% ≈$0.04")
        );
    }

    #[test]
    fn empty_unknown_zero_and_cents_remain_distinct() {
        for (attempts, expected) in [
            (vec![], Some("≈$0.00")),
            (vec![(Value::Null, None)], None),
            (
                vec![(json!({"input":0,"output":0,"cache_read":0}), Some(0.0))],
                Some("↑0 ↓0 ≈$0.00"),
            ),
            (vec![(Value::Null, Some(0.000001))], Some("≈$0.00")),
            (
                vec![(Value::Null, Some(0.0)), (Value::Null, None)],
                Some("≈$0.00+"),
            ),
            (
                vec![(Value::Null, Some(0.004)), (Value::Null, Some(0.004))],
                Some("≈$0.01"),
            ),
        ] {
            assert_eq!(format_turn(&summary(attempts)).as_deref(), expected);
        }
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(1000), "1.0k");
        assert_eq!(format_tokens(1250), "1.2k");
    }

    #[test]
    fn cache_ratio_uses_complete_weighted_counts_without_clamping() {
        for (attempts, expected) in [
            (
                vec![
                    json!({"input":100,"cache_read":100}),
                    json!({"input":900,"cache_read":0}),
                ],
                "↑1.0k 缓存 10%",
            ),
            (vec![json!({"input":3,"cache_read":2})], "↑3 缓存 67%"),
            (vec![json!({"input":8,"cache_read":1})], "↑8 缓存 13%"),
            (vec![json!({"input":1,"cache_read":2})], "↑1 缓存 200%"),
            (vec![json!({"input":0,"cache_read":1})], "↑0"),
            (
                vec![json!({"input":10,"cache_read":0}), json!({"input":10})],
                "↑20",
            ),
            (
                vec![json!({"input":10,"cache_read":0}), json!({"cache_read":0})],
                "",
            ),
        ] {
            assert_eq!(
                format_turn(&summary(attempts.into_iter().map(|usage| (usage, None))))
                    .unwrap_or_default(),
                expected
            );
        }
    }

    #[test]
    fn overflow_hides_the_affected_fields() {
        let overflow = summary([
            (
                json!({"input":u64::MAX,"output":1,"cache_read":1}),
                Some(f64::MAX),
            ),
            (json!({"input":1,"output":1,"cache_read":1}), Some(f64::MAX)),
        ]);
        assert_eq!(format_turn(&overflow).as_deref(), Some("↓2"));
        let cache_overflow = summary([
            (json!({"input":1,"cache_read":u64::MAX}), Some(0.02)),
            (json!({"input":1,"cache_read":1}), Some(0.02)),
        ]);
        assert_eq!(format_turn(&cache_overflow).as_deref(), Some("↑2 ≈$0.04"));
    }

    #[tokio::test(start_paused = true)]
    async fn narrow_statusline_omits_whole_usage_and_uses_display_width() {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let mut app = super::super::tests::test_app();
            app.model = Some("m".into());
            app.provider = Some("p".into());
            app.context_tokens = Some(1);
            app.turn_usage = Some(summary([(Value::Null, Some(123.45))]));
            let equal_width = line(&app, 50).to_string();
            assert!(equal_width.contains("≈$123.45"), "{equal_width}");
            assert!(!equal_width.contains("上下文"), "{equal_width}");

            app.ctx.active_agent = "中文 agent".into();
            app.model = Some("模型很长很长很长 e\u{301}".into());
            app.provider = Some("provider".into());
            app.turn_usage = Some(summary([(
                json!({"input":12300,"output":1200,"cache_read":9840}),
                Some(0.04),
            )]));
            app.phase = TurnPhase::Cancelling;
            app.connected = false;
            app.last_ctrl_c = Some(tokio::time::Instant::now());
            let wide = line(&app, 200);
            assert!(wide.to_string().contains("Ctrl+C to quit"));
            assert!(wide.to_string().contains("↑12.3k ↓1.2k 缓存 80% ≈$0.04"));
            assert!(
            wide.spans
                .iter()
                .any(|span| span.content == "cancel" && span.style == role_style(Role::ToolErr))
        );
            assert!(
                wide.spans
                    .iter()
                    .any(|span| span.content.contains("offline")
                        && span.style == role_style(Role::DimMeta))
            );
            let without_newline_hint = line(&app, 140).to_string();
            assert!(
                !without_newline_hint.contains("Alt+Enter 换行"),
                "{without_newline_hint}"
            );
            assert!(
                without_newline_hint.contains("Ctrl+C to quit"),
                "{without_newline_hint}"
            );
            assert!(
                without_newline_hint.contains("↑12.3k ↓1.2k 缓存 80% ≈$0.04"),
                "{without_newline_hint}"
            );
            let without_key_hints = line(&app, 120).to_string();
            assert!(
                !without_key_hints.contains("Alt+Enter 换行"),
                "{without_key_hints}"
            );
            assert!(
                !without_key_hints.contains("Ctrl+C to quit"),
                "{without_key_hints}"
            );
            assert!(
                without_key_hints.contains("↑12.3k ↓1.2k 缓存 80% ≈$0.04"),
                "{without_key_hints}"
            );
            let without_usage = line(&app, 100).to_string();
            for field in ["↑12.3k", "↓1.2k", "缓存", "≈$"] {
                assert!(!without_usage.contains(field), "{without_usage}");
            }
            for width in 0..=120 {
                let text = line(&app, width).to_string();
                assert_eq!(display_width(&text), width, "{width}: {text}");
                if text.contains("↑12.3k") {
                    assert!(text.contains("↓1.2k 缓存 80% ≈$0.04"), "{text}");
                }
            }
            tokio::time::advance(CTRL_C_WINDOW).await;
            assert!(line(&app, 200).to_string().contains("Ctrl+C to quit"));
            tokio::time::advance(std::time::Duration::from_millis(1)).await;
            let expired = line(&app, 200).to_string();
            assert!(!expired.contains("Ctrl+C to quit"), "{expired}");
            assert!(expired.contains("Alt+Enter 换行"), "{expired}");
            assert!(
                expired.contains("↑12.3k ↓1.2k 缓存 80% ≈$0.04"),
                "{expired}"
            );
        })
        .await
        .expect("statusline 时间与裁剪测试超过总上限");
    }
}
