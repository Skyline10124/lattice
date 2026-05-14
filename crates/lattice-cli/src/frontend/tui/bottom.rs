use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    Frame,
};

use super::state::{MenuState, SlashSuggestion};
use super::theme::Theme;
use super::view_model::TuiViewModel;
use super::widgets::{
    composer::{input_height, render_composer, render_help, set_cursor},
    selection::{
        info_height, render_info_pane, render_selection_pane, selection_height, InfoPane,
        SelectionPane,
    },
    statusline::Statusline,
};

const HELP_PANEL_HEIGHT: u16 = 9;
// Preserve the existing one-row cushion reserved by the top-level split when help is open.
const HELP_RESERVED_HEIGHT: u16 = 10;
const STATUSLINE_HEIGHT: u16 = 1;

pub(super) fn bottom_height(
    vm: &TuiViewModel<'_>,
    suggestions: &[SlashSuggestion],
    menu: Option<&MenuState>,
    terminal_height: u16,
) -> u16 {
    let help = if vm.help_open {
        HELP_RESERVED_HEIGHT
    } else {
        0
    };
    let pane = bottom_pane(vm, suggestions, menu);
    let pane_height = pane.as_ref().map_or(0, BottomPane::height);
    let composer = input_height(vm).saturating_add(STATUSLINE_HEIGHT);

    help.saturating_add(pane_height)
        .saturating_add(composer)
        .min(terminal_height.saturating_sub(1).max(4))
}

pub(super) fn render_bottom(
    f: &mut Frame,
    area: Rect,
    vm: &TuiViewModel<'_>,
    theme: &Theme,
    suggestions: &[SlashSuggestion],
    menu: Option<&MenuState>,
) -> Rect {
    let help_height = if vm.help_open { HELP_PANEL_HEIGHT } else { 0 };
    let pane = bottom_pane(vm, suggestions, menu);
    let pane_height = pane.as_ref().map_or(0, BottomPane::height);
    let composer_height = input_height(vm);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(help_height),
            Constraint::Length(pane_height),
            Constraint::Length(composer_height),
            Constraint::Length(STATUSLINE_HEIGHT),
        ])
        .split(area);

    if help_height > 0 {
        render_help(f, chunks[0], theme);
    }
    if let Some(pane) = pane {
        pane.render(f, chunks[1], theme);
    }
    render_composer(f, chunks[2], vm, theme);
    Statusline::new(*theme).render(chunks[3], f.buffer_mut(), vm);

    chunks[2]
}

pub(super) fn place_cursor(f: &mut Frame, input_area: Rect, vm: &TuiViewModel<'_>) {
    set_cursor(f, input_area, vm);
}

fn selection_pane<'a>(
    vm: &TuiViewModel<'_>,
    suggestions: &'a [SlashSuggestion],
    menu: Option<&'a MenuState>,
) -> Option<SelectionPane<'a>> {
    menu.and_then(SelectionPane::from_menu).or_else(|| {
        SelectionPane::from_suggestions(
            suggestions,
            vm.composer.suggestion_index,
            vm.composer.suggestion_window_start,
        )
        .or_else(|| {
            (vm.queue_prompt_active && vm.queued_count > 0)
                .then(|| vm.queue_head_summary.clone())
                .flatten()
                .map(SelectionPane::queued_prompt)
        })
    })
}

enum BottomPane<'a> {
    Selection(SelectionPane<'a>),
    Thinking(InfoPane<'a>),
}

impl<'a> BottomPane<'a> {
    fn height(&self) -> u16 {
        match self {
            Self::Selection(selection) => selection_height(selection.len()),
            Self::Thinking(info) => info_height(info.len()),
        }
    }

    fn render(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        match self {
            Self::Selection(selection) => render_selection_pane(f, area, selection, theme),
            Self::Thinking(info) => render_info_pane(f, area, info, theme),
        }
    }
}

fn bottom_pane<'a>(
    vm: &TuiViewModel<'a>,
    suggestions: &'a [SlashSuggestion],
    menu: Option<&'a MenuState>,
) -> Option<BottomPane<'a>> {
    selection_pane(vm, suggestions, menu)
        .map(BottomPane::Selection)
        .or_else(|| {
            vm.thinking
                .and_then(InfoPane::thinking)
                .map(BottomPane::Thinking)
        })
}
