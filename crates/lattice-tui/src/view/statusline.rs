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
    let mut left = [
        Some(app.ctx.active_agent.as_str()),
        app.model.as_deref(),
        app.provider.as_deref(),
    ];
    let mut right = [
        (!app.viewport.following()).then(|| format!("↓ {below} 行")),
        ctrl_c_visible(app).then(|| CTRL_C_HINT.into()),
        app.dock_status().is_none().then(|| {
            if app.kitty_keyboard {
                "S-Enter 换行"
            } else {
                "Alt+Enter 换行"
            }
            .into()
        }),
        app.context_tokens.map(format_context),
        Some(phase_and_connection(app)),
    ];
    let room = width.saturating_sub(2);
    for omitted in 0..5 {
        let left_w = joined_width(left.iter().copied().flatten());
        let right_w = joined_width(right.iter().filter_map(|s| s.as_deref()));
        if left_w + right_w + usize::from(left_w > 0 && right_w > 0) <= room {
            break;
        }
        match omitted {
            0 => right[2] = None,
            1 => right[3] = None,
            2 => left[2] = None,
            3 => left[1] = None,
            _ => left[0] = None,
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
        if index == 4 {
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

fn format_context(tokens: u64) -> String {
    if tokens < 1000 {
        format!("上下文 {tokens}")
    } else {
        format!("上下文 {:.1}k", tokens as f64 / 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::StatusPlacement;

    #[test]
    fn fields_omit_in_contract_order_and_keep_scroll_quit_phase() {
        let mut app = super::super::tests::test_app();
        app.ctx.active_agent = "a".into();
        app.model = Some("m".into());
        app.provider = Some("p".into());
        app.context_tokens = Some(12300);
        app.kitty_keyboard = true;
        app.phase = TurnPhase::Streaming;
        app.last_ctrl_c = Some(std::time::Instant::now());
        app.blocks.push(Block::Status {
            kind: StatusKind::Notice(vec!["row".into(); 32]),
            placement: StatusPlacement::Transcript,
        });
        app.viewport.on_content_changed(32, 20);
        app.viewport.page_up(20);
        for (width, expected) in [
            (
                75,
                " a · m · p ↓ 12 行 · Ctrl+C to quit · S-Enter 换行 · 上下文 12.3k · stream ",
            ),
            (
                60,
                " a · m · p ↓ 12 行 · Ctrl+C to quit · 上下文 12.3k · stream ",
            ),
            (45, " a · m · p ↓ 12 行 · Ctrl+C to quit · stream "),
        ] {
            assert_eq!(line(&app, width).to_string(), expected);
        }
        for (width, left, absent) in [
            (74, "a · m · p", "换行"),
            (59, "a · m · p", "上下文"),
            (44, "a · m", " · p"),
            (40, "a", " · m"),
            (36, "", " a"),
        ] {
            let text = line(&app, width).to_string();
            assert!(text.starts_with(&format!(" {left}")), "{width}: {text}");
            assert!(!text.contains(absent), "{width}: {text}");
            assert!(text.contains("↓ 12 行 · Ctrl+C to quit"), "{width}: {text}");
            assert!(text.ends_with("stream "), "{width}: {text}");
        }
        for width in 0..36 {
            let text = line(&app, width);
            assert_eq!(text.width(), width);
        }
        app.viewport.to_bottom(32, 20);
        assert!(!line(&app, 100).to_string().contains("↓"));
    }

    #[test]
    fn context_usage_formats_the_request_size() {
        for (tokens, expected) in [
            (0, "上下文 0"),
            (999, "上下文 999"),
            (1000, "上下文 1.0k"),
            (12345, "上下文 12.3k"),
        ] {
            assert_eq!(format_context(tokens), expected);
        }
    }

    #[test]
    fn only_phase_uses_phase_color_and_expired_quit_hint_disappears() {
        let mut app = super::super::tests::test_app();
        app.last_ctrl_c = Some(std::time::Instant::now());
        app.connected = false;
        for (phase, role) in [
            (TurnPhase::Idle, Role::DimMeta),
            (TurnPhase::Streaming, Role::User),
            (TurnPhase::Cancelling, Role::ToolErr),
        ] {
            app.phase = phase;
            let line = line(&app, 100);
            for span in line.spans {
                assert_eq!(
                    span.style.fg,
                    Some(role_color(if span.content == phase_text(phase) {
                        role
                    } else {
                        Role::DimMeta
                    }))
                );
            }
        }
        app.last_ctrl_c =
            Some(std::time::Instant::now() - CTRL_C_WINDOW - std::time::Duration::from_secs(1));
        assert!(!line(&app, 100).to_string().contains(CTRL_C_HINT));
    }
}
