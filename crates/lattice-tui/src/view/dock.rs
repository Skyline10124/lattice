// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// 输入区常驻栈底；唯一暂驻状态由 Block placement 持有，不复制状态栈。
pub(crate) struct Dock<'a> {
    top: Module<'a>,
    frame_h: u16,
}

enum Module<'a> {
    Input(InputModule<'a>),
    Status(StatusModule<'a>),
}

struct InputModule<'a> {
    input: &'a InputBuffer,
    focused: bool,
    running: bool,
    queue_full: bool,
}

struct StatusModule<'a> {
    kind: &'a StatusKind,
    severity: DockSeverity,
}
pub(crate) enum DockIntent {
    Edit(crate::input::KeyAction),
    Submit,
    ReleaseStatus,
    None,
}

impl<'a> Dock<'a> {
    pub(crate) fn new(app: &'a App) -> Self {
        let top = match app.dock_status() {
            Some((kind, severity)) => Module::Status(StatusModule { kind, severity }),
            None => Module::Input(InputModule {
                input: &app.input,
                focused: matches!(app.focus, Focus::Dock),
                running: app.phase != TurnPhase::Idle,
                queue_full: app.queue.is_full(),
            }),
        };
        Self {
            top,
            frame_h: app.term_height,
        }
    }

    pub(crate) fn route(&self, action: crate::input::KeyAction) -> DockIntent {
        match &self.top {
            Module::Input(module) => module.interpret(action),
            Module::Status(module) => module.interpret(action),
        }
    }

    pub(crate) fn height(&self, content_w: u16) -> u16 {
        let rows = match &self.top {
            Module::Input(module) => module.rows(content_w),
            Module::Status(module) => module.rows(content_w),
        };
        (2 + rows).min(self.frame_h.saturating_sub(1) / 2)
    }

    pub(crate) fn render(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        // 极小区域先去空行，再去标题；即使没有正文空间也不越界。
        let spacer = u16::from(area.height >= 3);
        let title_h = u16::from(area.height >= 2);
        let title_area = Rect::new(
            area.x.saturating_add(GUTTER.min(area.width)),
            area.y.saturating_add(spacer),
            area.width.saturating_sub(GUTTER),
            title_h,
        );
        let content = Rect::new(
            area.x,
            area.y.saturating_add(spacer + title_h),
            area.width,
            area.height.saturating_sub(spacer + title_h),
        );
        match &self.top {
            Module::Input(module) => module.render(frame, title_area, content),
            Module::Status(module) => module.render(frame, title_area, content),
        }
    }
}

impl InputModule<'_> {
    fn interpret(&self, action: crate::input::KeyAction) -> DockIntent {
        use crate::input::KeyAction;
        match action {
            KeyAction::Submit => DockIntent::Submit,
            KeyAction::ModifiedSpace => DockIntent::Edit(KeyAction::Insert(' ')),
            KeyAction::Ignored => DockIntent::None,
            action => DockIntent::Edit(action),
        }
    }

    fn rows(&self, content_w: u16) -> u16 {
        input_visual(self.input, content_w).0.len().clamp(1, 10) as u16
    }

    fn render(&self, frame: &mut Frame, title: Rect, area: Rect) {
        let (visual, cursor_row, cursor_col) =
            input_visual(self.input, area.width.saturating_sub(GUTTER));
        let height = usize::from(area.height).min(10);
        let scroll = cursor_row.saturating_sub(height.saturating_sub(1));
        let below = visual.len().saturating_sub(scroll + height);
        let mut right = Vec::new();
        if self.running {
            right.push("Enter 排队".into());
        }
        if self.queue_full {
            right.push("排队已满".into());
        }
        if scroll > 0 {
            right.push(format!("↑ {scroll} 行"));
        }
        if below > 0 {
            right.push(format!("↓ {below} 行"));
        }
        frame.render_widget(
            Paragraph::new(title_line("USER", Role::User, &right, title.width as usize)),
            title,
        );
        // 输入区不显示提示符，gutter 留空，正文与对话记录正文同列。
        let shown = visual
            .iter()
            .skip(scroll)
            .take(height)
            .map(|text| {
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(text.clone(), role_style(Role::Assistant)),
                ])
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(shown), area);
        let y = cursor_row.saturating_sub(scroll) as u16;
        let x = GUTTER.saturating_add(cursor_col as u16);
        if self.focused && y < area.height && x < area.width {
            frame.set_cursor_position((area.x.saturating_add(x), area.y.saturating_add(y)));
        }
    }
}

impl StatusModule<'_> {
    fn interpret(&self, action: crate::input::KeyAction) -> DockIntent {
        match action {
            crate::input::KeyAction::Insert(' ') => DockIntent::ReleaseStatus,
            _ => DockIntent::None,
        }
    }

    fn rows(&self, content_w: u16) -> u16 {
        body(&status_text(self.kind), content_w, Style::default())
            .len()
            .clamp(1, 5) as u16
    }

    fn render(&self, frame: &mut Frame, title: Rect, area: Rect) {
        let (label, title_role, text_role) = match self.severity {
            DockSeverity::Warn => ("WARN", Role::Warn, Role::Assistant),
            DockSeverity::Error => ("ERROR", Role::ToolErr, Role::ToolErr),
        };
        frame.render_widget(
            Paragraph::new(title_line(
                label,
                title_role,
                &["Space 关闭".into()],
                title.width as usize,
            )),
            title,
        );
        let shown = body(
            &status_text(self.kind),
            area.width.saturating_sub(GUTTER),
            role_style(text_role),
        )
        .into_iter()
        .take(5)
        .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(shown), area);
    }
}
