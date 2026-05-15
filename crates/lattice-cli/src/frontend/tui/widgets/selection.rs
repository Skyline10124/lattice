use std::borrow::Cow;

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::frontend::tui::state::{MenuKind, MenuState, SlashSuggestion, SELECTION_LIST_PAGE_SIZE};
use crate::frontend::tui::theme::Theme;

pub(in crate::frontend::tui) struct SelectionPane<'a> {
    title: &'static str,
    items: Vec<SelectionItem<'a>>,
    selected_index: usize,
    window_start: usize,
}

pub(in crate::frontend::tui) struct InfoPane<'a> {
    title: &'static str,
    lines: Vec<Cow<'a, str>>,
}

struct SelectionItem<'a> {
    label: Cow<'a, str>,
    description: Option<Cow<'a, str>>,
}

impl<'a> SelectionPane<'a> {
    pub(in crate::frontend::tui) fn from_menu(menu: &'a MenuState) -> Option<Self> {
        if menu.options.is_empty() {
            return None;
        }
        Some(Self {
            title: menu_title(&menu.kind),
            items: menu
                .options
                .iter()
                .map(|option| SelectionItem {
                    label: Cow::Borrowed(option.as_str()),
                    description: None,
                })
                .collect(),
            selected_index: menu.index,
            window_start: menu.window_start,
        })
    }

    pub(in crate::frontend::tui) fn from_suggestions(
        suggestions: &'a [SlashSuggestion],
        selected_index: usize,
        window_start: usize,
    ) -> Option<Self> {
        if suggestions.is_empty() {
            return None;
        }
        Some(Self {
            title: " commands ",
            items: suggestions
                .iter()
                .map(|suggestion| SelectionItem {
                    label: Cow::Borrowed(suggestion.command),
                    description: Some(Cow::Borrowed(suggestion.description)),
                })
                .collect(),
            selected_index,
            window_start,
        })
    }

    pub(in crate::frontend::tui) fn queued_prompt(summary: String) -> Self {
        Self {
            title: "queued prompt",
            items: vec![SelectionItem {
                label: Cow::Borrowed("Send queued prompt"),
                description: Some(Cow::Owned(summary)),
            }],
            selected_index: 0,
            window_start: 0,
        }
    }

    pub(in crate::frontend::tui) fn len(&self) -> usize {
        self.items.len()
    }
}

impl<'a> InfoPane<'a> {
    pub(in crate::frontend::tui) fn thinking(reasoning: &'a str) -> Option<Self> {
        let lines = reasoning
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        if lines.is_empty() {
            return None;
        }

        let start = lines.len().saturating_sub(SELECTION_LIST_PAGE_SIZE);
        Some(Self {
            title: " thinking ",
            lines: lines[start..]
                .iter()
                .map(|line| Cow::Borrowed(*line))
                .collect(),
        })
    }

    pub(in crate::frontend::tui) fn len(&self) -> usize {
        self.lines.len()
    }
}

pub(in crate::frontend::tui) fn selection_height(item_count: usize) -> u16 {
    if item_count > 0 {
        item_count.min(SELECTION_LIST_PAGE_SIZE) as u16 + 1
    } else {
        0
    }
}

pub(in crate::frontend::tui) fn info_height(line_count: usize) -> u16 {
    selection_height(line_count)
}

pub(in crate::frontend::tui) fn render_selection_pane(
    f: &mut Frame,
    area: Rect,
    pane: &SelectionPane<'_>,
    theme: &Theme,
) {
    if area.height == 0 {
        return;
    }

    let block = Block::default()
        .title(pane.title)
        .borders(Borders::TOP)
        .border_style(theme.border_style())
        .style(Style::default().bg(theme.bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let visible_count = inner.height as usize;
    if visible_count == 0 {
        return;
    }
    let selected_index = pane.selected_index.min(pane.items.len().saturating_sub(1));
    let first_visible = first_visible_item(
        selected_index,
        pane.window_start,
        visible_count,
        pane.items.len(),
    );
    let lines = pane
        .items
        .iter()
        .enumerate()
        .skip(first_visible)
        .take(visible_count)
        .map(|(idx, item)| selection_line(item, idx == selected_index, theme))
        .collect::<Vec<_>>();

    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

pub(in crate::frontend::tui) fn render_info_pane(
    f: &mut Frame,
    area: Rect,
    pane: &InfoPane<'_>,
    theme: &Theme,
) {
    if area.height == 0 {
        return;
    }

    let block = Block::default()
        .title(pane.title)
        .borders(Borders::TOP)
        .border_style(theme.border_style())
        .style(Style::default().bg(theme.bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines = pane
        .lines
        .iter()
        .take(inner.height as usize)
        .map(|line| {
            Line::from(vec![
                Span::raw("  "),
                Span::styled(line.to_string(), theme.thinking_style()),
            ])
        })
        .collect::<Vec<_>>();

    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn selection_line<'a>(item: &'a SelectionItem<'a>, selected: bool, theme: &Theme) -> Line<'a> {
    let marker = if selected { "▶ " } else { "  " };
    let label_style = if selected {
        Style::default()
            .fg(theme.bg)
            .bg(theme.highlight)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(theme.highlight)
            .add_modifier(Modifier::BOLD)
    };
    let description_style = if selected {
        Style::default().fg(theme.bg).bg(theme.highlight)
    } else {
        Style::default().fg(theme.subtext)
    };

    match item.description.as_deref() {
        Some(description) => Line::from(vec![
            Span::styled(format!("{marker}{:<15}", item.label), label_style),
            Span::styled(description, description_style),
        ]),
        None => Line::from(vec![Span::styled(
            format!("{marker}{}", item.label),
            label_style,
        )]),
    }
}

fn menu_title(kind: &MenuKind) -> &'static str {
    match kind {
        MenuKind::Model => " models ",
        MenuKind::Provider => " providers ",
        MenuKind::Permissions => " permissions ",
    }
}

fn first_visible_item(
    selected_index: usize,
    window_start: usize,
    visible_count: usize,
    item_count: usize,
) -> usize {
    let max_start = item_count.saturating_sub(visible_count);
    let mut first_visible = window_start.min(max_start);
    if selected_index < first_visible {
        first_visible = selected_index;
    } else if selected_index >= first_visible + visible_count {
        first_visible = selected_index + 1 - visible_count;
    }
    first_visible
}
