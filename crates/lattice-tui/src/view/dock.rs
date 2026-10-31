// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::blocks::BlockId;
use lattice_api::{Effort, ModelCandidate, ModelCandidates, local::AuthState};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModelQuery {
    pub(crate) id: u64,
    pub(crate) target: Option<String>,
}

#[derive(Clone)]
pub(crate) enum PanelChoice {
    Model(ModelCandidate),
    Effort(Effort),
}

impl PanelChoice {
    fn label(&self) -> String {
        match self {
            Self::Model(model) => format!("{}/{}", model.provider, model.model),
            Self::Effort(effort) => effort.to_string(),
        }
    }
}

pub(crate) enum Panel {
    Help(Vec<String>),
    Model(Picker),
    Effort(Picker),
}

/// 只保存这次打开的候选和导航；已选意图由 app 持有。
pub(crate) struct Picker {
    query: ModelQuery,
    filter: String,
    items: Vec<PanelChoice>,
    cursor: usize,
    current: Option<String>,
    loading: bool,
    empty: &'static str,
}

const NO_EFFORT: &str =
    "该模型不能选择推理强度，可以在 providers.toml 中补充它的推理参数写法或档位";
const NO_MODEL: &str = "先用 /model 选择模型";

impl Panel {
    pub(crate) fn models(id: u64, current: Option<String>) -> Self {
        Self::Model(Picker::new(ModelQuery { id, target: None }, current, true))
    }

    pub(crate) fn efforts(id: u64, target: Option<String>, current: Option<Effort>) -> Self {
        let loading = target.is_some();
        let mut picker = Picker::new(
            ModelQuery { id, target },
            current.map(|effort| effort.to_string()),
            loading,
        );
        picker.empty = NO_MODEL;
        Self::Effort(picker)
    }

    fn title(&self) -> &'static str {
        match self {
            Self::Help(_) => "HELP",
            Self::Model(_) => "MODEL",
            Self::Effort(_) => "EFFORT",
        }
    }

    fn picker(&self) -> Option<&Picker> {
        match self {
            Self::Model(picker) | Self::Effort(picker) => Some(picker),
            Self::Help(_) => None,
        }
    }

    fn picker_mut(&mut self) -> Option<&mut Picker> {
        match self {
            Self::Model(picker) | Self::Effort(picker) => Some(picker),
            Self::Help(_) => None,
        }
    }
}

impl Picker {
    fn new(query: ModelQuery, current: Option<String>, loading: bool) -> Self {
        Self {
            query,
            filter: String::new(),
            items: Vec::new(),
            cursor: 0,
            current,
            loading,
            empty: "没有可选模型",
        }
    }

    fn matches(&self) -> Vec<usize> {
        let filter = self.filter.to_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                item.label()
                    .to_lowercase()
                    .contains(&filter)
                    .then_some(index)
            })
            .collect()
    }

    fn replace(&mut self, items: Vec<PanelChoice>, current: Option<String>, empty: &'static str) {
        self.items = items;
        self.current = current;
        self.loading = false;
        self.empty = empty;
        let matches = self.matches();
        self.cursor = if self.filter.is_empty() {
            matches
                .iter()
                .position(|&index| Some(self.items[index].label()) == self.current)
                .unwrap_or(0)
        } else {
            0
        };
    }

    fn edit(&mut self, action: crate::input::KeyAction) {
        use crate::input::KeyAction;
        match action {
            KeyAction::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyAction::Down => {
                self.cursor = self
                    .cursor
                    .saturating_add(1)
                    .min(self.matches().len().saturating_sub(1))
            }
            action => {
                match action {
                    KeyAction::Insert(c) if !c.is_control() => self.filter.push(c),
                    KeyAction::Paste(text) => {
                        self.filter.extend(text.chars().filter(|c| !c.is_control()))
                    }
                    KeyAction::Backspace => {
                        self.filter.pop();
                    }
                    KeyAction::ModifiedSpace => self.filter.push(' '),
                    _ => return,
                }
                self.cursor = 0;
            }
        }
    }

    fn chosen(&self) -> Option<PanelChoice> {
        self.matches()
            .get(self.cursor)
            .map(|&index| self.items[index].clone())
    }
}

/// Dock 的持久状态：输入区之上的模块栈。输入区常驻栈底，其上至多一个面板，
/// 再上至多一条暂驻消息。消息记下对话记录里那个状态 block 的身份与一份内容：
/// 所属条目被丢弃时消息继续暂驻，释放后不再出现。
#[derive(Default)]
pub(crate) struct DockState {
    panel: Option<Panel>,
    message: Option<(BlockId, Status)>,
    /// 自上次取走以来栈是否变过，供 viewport 区分 Dock 变化与内容变化。
    changed: bool,
}

impl DockState {
    /// 新状态消息推入时由 Dock 按等级决定是否接住：Warn 与 Error 暂驻，Info 不管。
    /// 消息之间不叠：已暂驻的那条落回对话记录，新消息成为栈顶。
    pub(crate) fn offer(&mut self, id: BlockId, status: &Status) {
        match status.level {
            NoticeLevel::Warn | NoticeLevel::Error => {
                self.message = Some((id, status.clone()));
                self.changed = true;
            }
            NoticeLevel::Info => {}
        }
    }

    pub(crate) fn message_id(&self) -> Option<BlockId> {
        self.message.as_ref().map(|(id, _)| *id)
    }

    /// 对话记录据此跳过暂驻的那一条。重试提示按尝试、结局按 TurnId 识别：
    /// 条目重建后块身份变了，仍是同一条消息；其余按块身份。
    pub(crate) fn parks(&self, id: BlockId, status: &Status) -> bool {
        match &self.message {
            Some((
                _,
                Status {
                    retry: Some(key), ..
                },
            )) => status.retry == Some(*key),
            Some((
                _,
                Status {
                    closing: Some((turn, _)),
                    ..
                },
            )) => status.closing.as_ref().is_some_and(|(id, _)| id == turn),
            Some((parked, _)) => *parked == id,
            None => false,
        }
    }

    pub(crate) fn message(&self) -> Option<&Status> {
        self.message.as_ref().map(|(_, status)| status)
    }

    pub(crate) fn release(&mut self) {
        self.changed |= self.message.take().is_some();
    }

    pub(crate) fn open_panel(&mut self, panel: Panel) {
        self.panel = Some(panel);
        self.changed = true;
    }

    pub(crate) fn close_panel(&mut self) {
        self.changed |= self.panel.take().is_some();
    }

    pub(crate) fn query(&self) -> Option<ModelQuery> {
        match self.panel.as_ref()? {
            Panel::Model(picker) => Some(picker.query.clone()),
            Panel::Effort(picker) if picker.query.target.is_some() => Some(picker.query.clone()),
            _ => None,
        }
    }

    pub(crate) fn effort_target(&self) -> Option<Option<&str>> {
        match self.panel.as_ref()? {
            Panel::Effort(picker) => Some(picker.query.target.as_deref()),
            _ => None,
        }
    }

    pub(crate) fn retarget_effort(
        &mut self,
        id: u64,
        target: Option<String>,
        current: Option<Effort>,
    ) {
        if let Some(Panel::Effort(picker)) = &mut self.panel {
            picker.query = ModelQuery { id, target };
            picker.current = current.map(|effort| effort.to_string());
            picker.items.clear();
            picker.cursor = 0;
            picker.loading = picker.query.target.is_some();
            picker.empty = NO_MODEL;
            self.changed = true;
        }
    }

    pub(crate) fn populate(&mut self, result: ModelCandidates, current: Option<String>) {
        match &mut self.panel {
            Some(Panel::Model(picker)) => picker.replace(
                result.models.into_iter().map(PanelChoice::Model).collect(),
                current,
                "没有可选模型",
            ),
            Some(Panel::Effort(picker)) => picker.replace(
                result
                    .models
                    .into_iter()
                    .flat_map(|model| model.supported_efforts)
                    .map(PanelChoice::Effort)
                    .collect(),
                current,
                NO_EFFORT,
            ),
            _ => return,
        }
        self.changed = true;
    }

    pub(crate) fn edit_panel(&mut self, action: crate::input::KeyAction) {
        if let Some(picker) = self.panel.as_mut().and_then(Panel::picker_mut) {
            picker.edit(action);
            self.changed = true;
        }
    }

    pub(crate) fn update_current(&mut self, model: Option<String>, effort: Option<Effort>) {
        match &mut self.panel {
            Some(Panel::Model(picker)) => picker.current = model,
            Some(Panel::Effort(picker)) => picker.current = effort.map(|effort| effort.to_string()),
            _ => {}
        }
    }

    /// 栈顶是输入区：输入可见且可编辑。
    pub(crate) fn shows_input(&self) -> bool {
        self.panel.is_none() && self.message.is_none()
    }

    pub(crate) fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }
}

/// 一帧内的 Dock：只呈现并解释栈顶模块。
pub(crate) struct Dock<'a> {
    top: Module<'a>,
    frame_h: u16,
}

enum Module<'a> {
    Input(InputModule<'a>),
    Panel(PanelModule<'a>),
    Message(MessageModule<'a>),
}

struct InputModule<'a> {
    input: &'a InputBuffer,
    focused: bool,
    running: bool,
    queue_full: bool,
    comments: usize,
}

struct PanelModule<'a> {
    panel: &'a Panel,
}

struct MessageModule<'a> {
    status: &'a Status,
}

pub(crate) enum DockIntent {
    Edit(crate::input::KeyAction),
    Submit,
    ReleaseStatus,
    ClosePanel,
    EditPanel(crate::input::KeyAction),
    Choose(PanelChoice),
    None,
}

impl<'a> Dock<'a> {
    pub(crate) fn new(app: &'a App) -> Self {
        let top = match (app.dock.message(), &app.dock.panel) {
            (Some(status), _) => Module::Message(MessageModule { status }),
            (None, Some(panel)) => Module::Panel(PanelModule { panel }),
            (None, None) => Module::Input(InputModule {
                input: &app.input,
                focused: matches!(app.focus, Focus::Dock),
                running: app.phase != TurnPhase::Idle,
                queue_full: app.queue.is_full(),
                comments: app.queue.draft_count(),
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
            Module::Panel(module) => module.interpret(action),
            Module::Message(module) => module.interpret(action),
        }
    }

    /// Dock 与对话记录共享 statusline 之上的高度：上限取对话记录留下的空间与一半中的较大者，
    /// 对话记录短时 Dock 可占满其下全部空间，长时对话记录至少留一半。
    pub(crate) fn height(&self, content_w: u16, transcript_rows: usize) -> u16 {
        let rows = match &self.top {
            Module::Input(module) => module.rows(content_w),
            Module::Panel(module) => module.rows(content_w),
            Module::Message(module) => module.rows(content_w),
        };
        let above = self.frame_h.saturating_sub(1);
        let left = above.saturating_sub(u16::try_from(transcript_rows).unwrap_or(u16::MAX));
        rows.saturating_add(2).min(left.max(above / 2))
    }

    pub(crate) fn render(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        // 极小区域先去空行，再去标题；即使没有正文空间也不越界。
        let spacer = u16::from(area.height >= 3);
        let title_h = u16::from(area.height >= 2);
        let title_area = Rect::new(area.x, area.y.saturating_add(spacer), area.width, title_h);
        let content = Rect::new(
            area.x,
            area.y.saturating_add(spacer + title_h),
            area.width,
            area.height.saturating_sub(spacer + title_h),
        );
        match &self.top {
            Module::Input(module) => module.render(frame, title_area, content),
            Module::Panel(module) => module.render(frame, title_area, content),
            Module::Message(module) => module.render(frame, title_area, content),
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
        u16::try_from(input_visual(self.input, content_w).0.len().max(1)).unwrap_or(u16::MAX)
    }

    fn render(&self, frame: &mut Frame, title: Rect, area: Rect) {
        let (visual, cursor_row, cursor_col) = input_visual(self.input, area.width);
        let height = usize::from(area.height);
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
        if self.comments > 0 {
            right.push(format!("{} 条评论待发送", self.comments));
        }
        frame.render_widget(
            Paragraph::new(title_line("USER", Role::User, &right, title.width as usize)),
            title,
        );
        // 输入区不显示提示符，正文与对话记录正文同列。
        let shown = visual
            .iter()
            .skip(scroll)
            .take(height)
            .map(|text| Line::from(Span::styled(text.clone(), role_style(Role::Assistant))))
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(shown), area);
        let y = cursor_row.saturating_sub(scroll) as u16;
        let x = cursor_col as u16;
        if self.focused && y < area.height && x < area.width {
            frame.set_cursor_position((area.x.saturating_add(x), area.y.saturating_add(y)));
        }
    }
}

impl PanelModule<'_> {
    fn interpret(&self, action: crate::input::KeyAction) -> DockIntent {
        match action {
            crate::input::KeyAction::Clear => DockIntent::ClosePanel,
            crate::input::KeyAction::Submit => self
                .panel
                .picker()
                .and_then(Picker::chosen)
                .map_or(DockIntent::None, DockIntent::Choose),
            action if self.panel.picker().is_some() => DockIntent::EditPanel(action),
            _ => DockIntent::None,
        }
    }

    fn rows(&self, content_w: u16) -> u16 {
        let rows = match self.panel {
            Panel::Help(lines) => body(&lines.join("\n"), content_w, Style::default()).len(),
            Panel::Model(picker) | Panel::Effort(picker) => {
                picker.matches().len().max(1).saturating_add(1)
            }
        };
        u16::try_from(rows.max(1)).unwrap_or(u16::MAX)
    }

    fn render(&self, frame: &mut Frame, title: Rect, area: Rect) {
        frame.render_widget(
            Paragraph::new(title_line(
                self.panel.title(),
                Role::Assistant,
                &[if self.panel.picker().is_some() {
                    "Enter 选定 · Esc 取消"
                } else {
                    "Esc 关闭"
                }
                .into()],
                title.width as usize,
            )),
            title,
        );
        if let Panel::Help(lines) = self.panel {
            frame.render_widget(
                Paragraph::new(body(
                    &lines.join("\n"),
                    area.width,
                    role_style(Role::Assistant),
                )),
                area,
            );
            return;
        }
        let picker = self.panel.picker().expect("选择面板");
        let matches = picker.matches();
        let height = usize::from(area.height.saturating_sub(1));
        let start = picker.cursor.saturating_sub(height.saturating_sub(1));
        let mut shown = vec![clip_spans(
            vec![Span::styled(
                format!("过滤：{}", picker.filter),
                role_style(Role::Assistant),
            )],
            area.width,
        )];
        if matches.is_empty() {
            shown.push(clip_spans(
                vec![Span::styled(
                    if picker.loading {
                        "加载中"
                    } else if !picker.items.is_empty() {
                        "没有匹配项"
                    } else {
                        picker.empty
                    },
                    role_style(Role::Assistant),
                )],
                area.width,
            ));
        } else {
            for &index in matches.iter().skip(start).take(height) {
                let item = &picker.items[index];
                let label = item.label();
                let mut spans = vec![Span::styled(label.clone(), role_style(Role::Assistant))];
                if picker.current.as_ref() == Some(&label) {
                    spans.push(Span::styled(" · 当前", role_style(Role::DimMeta)));
                }
                if let PanelChoice::Model(model) = item
                    && model.credential_status != AuthState::Ready
                {
                    spans.push(Span::styled(" · 无凭据", role_style(Role::DimMeta)));
                }
                shown.push(clip_spans(spans, area.width));
            }
        }
        frame.render_widget(Paragraph::new(shown), area);
        let row = if matches.is_empty() {
            0
        } else {
            picker.cursor.saturating_sub(start).saturating_add(1)
        };
        if area.width > 0 && row < usize::from(area.height) {
            frame.set_cursor_position((area.x, area.y.saturating_add(row as u16)));
        }
    }
}

fn clip_spans(spans: Vec<Span<'static>>, width: u16) -> Line<'static> {
    let mut line = Line::default();
    let mut remaining = usize::from(width);
    for span in &spans {
        for glyph in span.styled_graphemes(Style::default()) {
            let width = glyph.symbol.width();
            if width > remaining {
                return line;
            }
            remaining -= width;
            line.spans
                .push(Span::styled(glyph.symbol.to_owned(), glyph.style));
        }
    }
    line
}

impl MessageModule<'_> {
    fn interpret(&self, action: crate::input::KeyAction) -> DockIntent {
        match action {
            crate::input::KeyAction::Insert(' ') => DockIntent::ReleaseStatus,
            _ => DockIntent::None,
        }
    }

    fn rows(&self, content_w: u16) -> u16 {
        let rows = body(&status_text(self.status), content_w, Style::default()).len();
        u16::try_from(rows.max(1)).unwrap_or(u16::MAX)
    }

    fn render(&self, frame: &mut Frame, title: Rect, area: Rect) {
        let (label, title_role, text_role) = match self.status.level {
            NoticeLevel::Warn => ("WARN", Role::Warn, Role::Assistant),
            NoticeLevel::Error => ("ERROR", Role::ToolErr, Role::ToolErr),
            // `DockState::offer` 不接 Info；这一支只为穷尽匹配，沿用对话记录中的暗色。
            NoticeLevel::Info => ("INFO", Role::DimMeta, Role::DimMeta),
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
        let shown = body(&status_text(self.status), area.width, role_style(text_role));
        frame.render_widget(Paragraph::new(shown), area);
    }
}
