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

#[derive(Clone, Copy)]
enum Slot {
    Left(usize),
    Right(usize),
}

/// 等宽字段的省略先后：上下文、提供商、模型、session、agent、翻看提示、phase。
const TIE_ORDER: [Slot; 7] = [
    Slot::Right(3),
    Slot::Left(3),
    Slot::Left(2),
    Slot::Left(0),
    Slot::Left(1),
    Slot::Right(0),
    Slot::Right(4),
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
    if tokens < 1000 {
        format!("上下文 {tokens}")
    } else {
        format!("上下文 {:.1}k", tokens as f64 / 1000.0)
    }
}
