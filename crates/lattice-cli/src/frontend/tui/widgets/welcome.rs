use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Paragraph, Wrap},
    Frame,
};

use crate::frontend::tui::app::App;
use crate::frontend::tui::theme::Theme;

pub(in crate::frontend::tui) fn render_welcome(
    f: &mut Frame,
    area: Rect,
    app: &App,
    theme: &Theme,
) {
    let inner = inset(area, 2, 1);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let provider = if app.current_provider.is_empty() {
        "unresolved"
    } else {
        app.current_provider.as_str()
    };

    let ban_style = Style::default().fg(theme.user_accent);
    let banner = if inner.width >= 40 {
        vec![
            Line::styled(
                "  ██╗      █████╗ ████████╗████████╗██╗ ██████╗███████╗  ",
                ban_style,
            ),
            Line::styled(
                "  ██║     ██╔══██╗╚══██╔══╝╚══██╔══╝██║██╔════╝██╔════╝  ",
                ban_style,
            ),
            Line::styled(
                "  ██║     ███████║   ██║      ██║   ██║██║     █████╗    ",
                ban_style,
            ),
            Line::styled(
                "  ██║     ██╔══██║   ██║      ██║   ██║██║     ██╔══╝    ",
                ban_style,
            ),
            Line::styled(
                "  ███████╗██║  ██║   ██║      ██║   ██║╚██████╗███████╗  ",
                ban_style,
            ),
            Line::styled(
                "  ╚══════╝╚═╝  ╚═╝   ╚═╝      ╚═╝   ╚═╝ ╚═════╝╚══════╝  ",
                ban_style,
            ),
        ]
    } else {
        vec![Line::from(vec![
            Span::styled(
                "●",
                Style::default()
                    .fg(theme.assistant_accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                "LATTICE",
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" · agent CLI", Style::default().fg(theme.subtext)),
        ])]
    };

    let tip = welcome_tip();

    let mut lines: Vec<Line> = Vec::new();
    lines.extend(banner);
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![Span::styled(
        format!("  model  {} @ {}", app.current_model.as_str(), provider),
        Style::default().fg(theme.subtext),
    )]));
    lines.push(Line::from(vec![Span::styled(
        format!(
            "  runtime  sandbox:{} · plugins:{}",
            app.security.mode_label,
            app.plugin_count()
        ),
        Style::default().fg(theme.subtext),
    )]));
    if let Some(ref e) = app.thinking_effort {
        lines.push(Line::from(vec![Span::styled(
            format!("  effort  {}", e),
            Style::default().fg(theme.subtext),
        )]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(
            "  Tip: ",
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(tip, Style::default().fg(theme.subtext)),
    ]));
    lines.push(Line::raw(""));
    lines.extend(render_welcome_shortcuts(theme));

    f.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        inner,
    );
}

#[cfg(test)]
fn welcome_tip() -> &'static str {
    "Type / to see all available commands."
}

#[cfg(not(test))]
fn welcome_tip() -> &'static str {
    use std::sync::LazyLock;
    static TIP: LazyLock<&str> = LazyLock::new(|| {
        let tips = [
            "Type / to see all available commands.",
            "Use ! before a command to run it in bash (e.g. !ls).",
            "Ctrl+O expands or collapses the latest thinking block.",
            "Ctrl+E expands collapsed tool outputs.",
            "Ctrl+Y copies the last assistant response.",
            "Click and drag to select text — auto-copied.",
            "Use /effort <level> to control thinking depth.",
            "Up/Down arrows navigate input history.",
            "/model <name> switches the LLM for the next turn.",
            "/permissions switches the runtime sandbox for later turns.",
            "/plugins lists loaded official and local runtime plugins.",
        ];
        let ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        tips[(ns as usize) % tips.len()]
    });
    *TIP
}

fn render_welcome_shortcuts(theme: &Theme) -> Vec<Line<'static>> {
    let rows: Vec<(&str, &str)> = vec![
        ("Enter", "send message"),
        ("? /help", "show shortcuts"),
        ("/commands", "slash command menu"),
        ("Ctrl+N", "new session"),
        ("Ctrl+L", "clear transcript"),
        ("Ctrl+O", "thinking"),
        ("Ctrl+E", "expand tool output"),
        ("Ctrl+Y", "copy last response"),
        ("Ctrl+C", "quit"),
    ];
    rows.into_iter()
        .map(|(key, desc)| {
            Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("{:<14}", key), Style::default().fg(theme.highlight)),
                Span::styled(desc, Style::default().fg(theme.subtext)),
            ])
        })
        .collect()
}

fn inset(area: Rect, horizontal: u16, vertical: u16) -> Rect {
    Rect {
        x: area.x.saturating_add(horizontal),
        y: area.y.saturating_add(vertical),
        width: area.width.saturating_sub(horizontal.saturating_mul(2)),
        height: area.height.saturating_sub(vertical.saturating_mul(2)),
    }
}
