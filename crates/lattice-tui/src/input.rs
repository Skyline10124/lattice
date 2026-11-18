// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::ffi::OsString;
use std::io::Write;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use lattice_local::{ImageRef, UserInputPart};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ImageSource {
    File(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingImage {
    pub(crate) meta: ImageRef,
    pub(crate) source: ImageSource,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct InputBuffer {
    pub(crate) lines: Vec<Vec<UserInputPart<PendingImage>>>,
    /// `(行下标, 行内逻辑光标位置)`：文字每字符计 1，图片原子计 1。
    pub(crate) cursor: (usize, usize),
}

impl Default for InputBuffer {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn push_part<I>(parts: &mut Vec<UserInputPart<I>>, part: UserInputPart<I>) {
    match part {
        UserInputPart::Text { text } if text.is_empty() => {}
        UserInputPart::Text { text } => {
            if let Some(UserInputPart::Text { text: previous }) = parts.last_mut() {
                previous.push_str(&text);
            } else {
                parts.push(UserInputPart::Text { text });
            }
        }
        image @ UserInputPart::Image { .. } => parts.push(image),
    }
}

fn line_len(line: &[UserInputPart<PendingImage>]) -> usize {
    line.iter()
        .map(|part| match part {
            UserInputPart::Text { text } => char_len(text),
            UserInputPart::Image { .. } => 1,
        })
        .sum()
}

fn split_line(
    line: &[UserInputPart<PendingImage>],
    col: usize,
) -> (
    Vec<UserInputPart<PendingImage>>,
    Vec<UserInputPart<PendingImage>>,
) {
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut remaining = col;
    for part in line {
        if remaining == 0 {
            push_part(&mut right, part.clone());
            continue;
        }
        match part {
            UserInputPart::Text { text } => {
                let len = char_len(text);
                if remaining >= len {
                    remaining -= len;
                    push_part(&mut left, UserInputPart::Text { text: text.clone() });
                } else {
                    let (l, r) = split_at_char(text, remaining);
                    remaining = 0;
                    push_part(&mut left, UserInputPart::Text { text: l.to_owned() });
                    push_part(&mut right, UserInputPart::Text { text: r.to_owned() });
                }
            }
            UserInputPart::Image { image } => {
                remaining -= 1;
                left.push(UserInputPart::Image {
                    image: image.clone(),
                });
            }
        }
    }
    (left, right)
}

impl InputBuffer {
    pub(crate) fn new() -> Self {
        Self {
            lines: vec![Vec::new()],
            cursor: (0, 0),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    pub(crate) fn is_blank(&self) -> bool {
        self.image_count() == 0 && self.text().trim().is_empty()
    }

    pub(crate) fn image_count(&self) -> usize {
        self.lines
            .iter()
            .flatten()
            .filter(|part| matches!(part, UserInputPart::Image { .. }))
            .count()
    }

    pub(crate) fn text(&self) -> String {
        self.lines
            .iter()
            .map(|line| {
                let mut out = String::new();
                for part in line {
                    if let UserInputPart::Text { text } = part {
                        out.push_str(text);
                    }
                }
                out
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(crate) fn parts(&self) -> Vec<UserInputPart<PendingImage>> {
        let mut out = Vec::new();
        for (index, line) in self.lines.iter().enumerate() {
            if index > 0 {
                push_part(
                    &mut out,
                    UserInputPart::Text {
                        text: "\n".to_owned(),
                    },
                );
            }
            for part in line {
                push_part(&mut out, part.clone());
            }
        }
        out
    }

    pub(crate) fn set_text(&mut self, text: &str) {
        self.lines = if text.is_empty() {
            vec![Vec::new()]
        } else {
            text.split('\n')
                .map(|line| {
                    if line.is_empty() {
                        Vec::new()
                    } else {
                        vec![UserInputPart::Text {
                            text: line.to_owned(),
                        }]
                    }
                })
                .collect()
        };
        self.move_to_end();
    }

    pub(crate) fn move_to_end(&mut self) {
        let last = self.lines.len() - 1;
        self.cursor = (last, line_len(&self.lines[last]));
    }

    pub(crate) fn clear(&mut self) {
        self.lines = vec![Vec::new()];
        self.cursor = (0, 0);
    }

    pub(crate) fn insert_char(&mut self, c: char) {
        let (row, col) = self.cursor;
        let (mut left, right) = split_line(&self.lines[row], col);
        push_part(
            &mut left,
            UserInputPart::Text {
                text: c.to_string(),
            },
        );
        for part in right {
            push_part(&mut left, part);
        }
        self.lines[row] = left;
        self.cursor.1 += 1;
    }

    pub(crate) fn insert_image(&mut self, image: PendingImage) {
        let (row, col) = self.cursor;
        let (mut left, right) = split_line(&self.lines[row], col);
        left.push(UserInputPart::Image { image });
        for part in right {
            push_part(&mut left, part);
        }
        self.lines[row] = left;
        self.cursor.1 += 1;
    }

    /// Bracketed-paste payload. Inserts verbatim, newlines included, and
    /// **never** submits.
    pub(crate) fn insert_paste(&mut self, text: &str) {
        // Normalise line endings instead of deleting carriage returns: a
        // terminal may deliver pasted newlines as CRLF or as a bare CR, and
        // stripping CR outright collapses such a paste into a single line.
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let mut parts = text.split('\n');
        if let Some(first) = parts.next() {
            for c in first.chars() {
                self.insert_char(c);
            }
        }
        for part in parts {
            self.newline();
            for c in part.chars() {
                self.insert_char(c);
            }
        }
    }

    /// 主输入区与排队编辑器的粘贴入口：可识别为本机受支持图片文件时插入图片原子，
    /// 图片长度超限时返回错误，其余按普通文字粘贴。
    pub(crate) fn paste(&mut self, raw: &str) -> lattice_local::Result<()> {
        if let Some(path) = pasted_path(raw) {
            let ordinal = self.image_count().saturating_add(1);
            if let Some(meta) = lattice_local::probe_image_file(ordinal, &path)? {
                self.insert_image(PendingImage {
                    meta,
                    source: ImageSource::File(path),
                });
                return Ok(());
            }
        }
        self.insert_paste(raw);
        Ok(())
    }

    pub(crate) fn newline(&mut self) {
        let (row, col) = self.cursor;
        let (left, right) = split_line(&self.lines[row], col);
        self.lines[row] = left;
        self.lines.insert(row + 1, right);
        self.cursor = (row + 1, 0);
    }

    pub(crate) fn backspace(&mut self) {
        let (row, col) = self.cursor;
        if col > 0 {
            let (mut left, right) = split_line(&self.lines[row], col);
            if let Some(UserInputPart::Text { mut text }) = left.pop() {
                text.pop();
                push_part(&mut left, UserInputPart::Text { text });
            }
            for part in right {
                push_part(&mut left, part);
            }
            self.lines[row] = left;
            self.cursor.1 -= 1;
        } else if row > 0 {
            let current = self.lines.remove(row);
            self.cursor.0 -= 1;
            self.cursor.1 = line_len(&self.lines[self.cursor.0]);
            for part in current {
                push_part(&mut self.lines[self.cursor.0], part);
            }
        }
    }

    pub(crate) fn delete(&mut self) {
        let (row, col) = self.cursor;
        if col < line_len(&self.lines[row]) {
            let (mut left, right) = split_line(&self.lines[row], col);
            let mut iter = right.into_iter();
            if let Some(UserInputPart::Text { text }) = iter.next() {
                let (_, rest) = split_at_char(&text, 1);
                push_part(&mut left, UserInputPart::Text { text: rest.into() });
            }
            for part in iter {
                push_part(&mut left, part);
            }
            self.lines[row] = left;
        } else if row + 1 < self.lines.len() {
            let next = self.lines.remove(row + 1);
            for part in next {
                push_part(&mut self.lines[row], part);
            }
        }
    }

    pub(crate) fn move_left(&mut self) {
        if self.cursor.1 > 0 {
            self.cursor.1 -= 1;
        }
    }

    pub(crate) fn move_right(&mut self) {
        if self.cursor.1 < line_len(&self.lines[self.cursor.0]) {
            self.cursor.1 += 1;
        }
    }

    pub(crate) fn home(&mut self) {
        self.cursor.1 = 0;
    }

    pub(crate) fn end(&mut self) {
        self.cursor.1 = line_len(&self.lines[self.cursor.0]);
    }

    /// Logical line count. The draw path deliberately does **not** use this:
    /// it reserves *wrapped* rows via `Dock::height`, so a long
    /// single line still gets the height it actually occupies.
    #[cfg(test)]
    pub(crate) fn rows(&self) -> usize {
        self.lines.len().max(1)
    }
}

impl InputBuffer {
    /// 纯文本编辑，不解释提交、焦点、历史或草稿归属；输入区与排队编辑器共用。
    pub(crate) fn edit(&mut self, action: &KeyAction, width: u16) {
        match action {
            KeyAction::Insert(c) => self.insert_char(*c),
            KeyAction::ModifiedSpace => self.insert_char(' '),
            KeyAction::Paste(text) => self.insert_paste(text),
            KeyAction::Newline => self.newline(),
            KeyAction::Backspace => self.backspace(),
            KeyAction::Delete => self.delete(),
            KeyAction::Left => self.move_left(),
            KeyAction::Right => self.move_right(),
            KeyAction::Home => self.home(),
            KeyAction::End => self.end(),
            KeyAction::Up | KeyAction::Down => {
                crate::view::move_input_vertical(self, width, matches!(action, KeyAction::Down));
            }
            _ => {}
        }
    }
}

pub(crate) struct History {
    entries: Vec<String>,
    /// `None` = not currently browsing history.
    cursor: Option<usize>,
    path: PathBuf,
}

impl History {
    /// Loads `session_dir/repl_history.txt` — same path and plain-line format
    /// the rustyline REPL used, so existing history survives the migration.
    pub(crate) fn load(session_dir: &Path) -> Self {
        let path = session_dir.join("repl_history.txt");
        let entries = std::fs::read_to_string(&path)
            .map(|s| {
                s.lines()
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            entries,
            cursor: None,
            path,
        }
    }

    pub(crate) fn push(&mut self, entry: &str) {
        if entry.is_empty() {
            self.reset();
            return;
        }
        self.entries.push(entry.to_string());
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let line = entry.replace(['\n', '\r'], " ");
            let _ = writeln!(file, "{line}");
        }
        self.reset();
    }

    pub(crate) fn prev(&mut self) -> Option<&str> {
        if self.entries.is_empty() {
            return None;
        }
        let idx = match self.cursor {
            None => self.entries.len() - 1,
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.cursor = Some(idx);
        self.entries.get(idx).map(String::as_str)
    }

    pub(crate) fn next(&mut self) -> Option<&str> {
        let idx = self.cursor?;
        if idx + 1 < self.entries.len() {
            self.cursor = Some(idx + 1);
            self.entries.get(idx + 1).map(String::as_str)
        } else {
            self.cursor = None;
            Some("")
        }
    }

    pub(crate) fn reset(&mut self) {
        self.cursor = None;
    }
}

pub(crate) enum KeyAction {
    Insert(char),
    ModifiedSpace,
    Paste(String),
    Newline,
    Submit,
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    Up,
    Down,
    HistoryPrev,
    HistoryNext,
    ScrollPageUp,
    ScrollPageDown,
    ScrollToBottom,
    Clear,
    /// First Ctrl+C.
    CancelOrClear,
    /// Second Ctrl+C within `CTRL_C_WINDOW`, or Ctrl+D on an empty buffer.
    Quit,
    Ignored,
}

pub(crate) const CTRL_C_WINDOW: Duration = Duration::from_secs(2);

/// Pure key → action resolution. `kitty` selects whether `Shift+Enter` is
/// distinguishable from `Enter` (§8).
pub(crate) fn resolve_key(key: KeyEvent, kitty: bool, buffer_empty: bool) -> KeyAction {
    if key.kind != KeyEventKind::Press {
        return KeyAction::Ignored;
    }
    let mods = key.modifiers;
    match key.code {
        KeyCode::Enter if mods.contains(KeyModifiers::ALT) => KeyAction::Newline,
        KeyCode::Enter if mods.contains(KeyModifiers::SHIFT) => {
            if kitty {
                KeyAction::Newline
            } else {
                KeyAction::Submit
            }
        }
        KeyCode::Enter => KeyAction::Submit,
        KeyCode::Char('\u{3}') => KeyAction::CancelOrClear,
        KeyCode::Char('\u{4}') => ctrl_d(buffer_empty),
        KeyCode::Char(c) if mods.contains(KeyModifiers::CONTROL) => match c {
            'c' | 'C' => KeyAction::CancelOrClear,
            'd' | 'D' => ctrl_d(buffer_empty),
            _ => KeyAction::Ignored,
        },
        KeyCode::Char(_) if mods.contains(KeyModifiers::ALT) => KeyAction::Ignored,
        KeyCode::Char(' ') if !mods.is_empty() => KeyAction::ModifiedSpace,
        KeyCode::Char(c) if !c.is_control() => KeyAction::Insert(c),
        KeyCode::Backspace => KeyAction::Backspace,
        KeyCode::Delete => KeyAction::Delete,
        KeyCode::Left => KeyAction::Left,
        KeyCode::Right => KeyAction::Right,
        KeyCode::Home => KeyAction::Home,
        KeyCode::End if mods.contains(KeyModifiers::CONTROL) => KeyAction::ScrollToBottom,
        KeyCode::End => KeyAction::End,
        KeyCode::Up if mods.contains(KeyModifiers::CONTROL) => KeyAction::HistoryPrev,
        KeyCode::Down if mods.contains(KeyModifiers::CONTROL) => KeyAction::HistoryNext,
        KeyCode::Up => KeyAction::Up,
        KeyCode::Down => KeyAction::Down,
        KeyCode::PageUp => KeyAction::ScrollPageUp,
        KeyCode::PageDown => KeyAction::ScrollPageDown,
        KeyCode::Esc => KeyAction::Clear,
        _ => KeyAction::Ignored,
    }
}

fn ctrl_d(buffer_empty: bool) -> KeyAction {
    if buffer_empty {
        KeyAction::Quit
    } else {
        KeyAction::Ignored
    }
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn split_at_char(s: &str, idx: usize) -> (&str, &str) {
    match s.char_indices().nth(idx) {
        Some((byte, _)) => s.split_at(byte),
        None => (s, ""),
    }
}

fn pasted_path(raw: &str) -> Option<PathBuf> {
    let candidate = raw.trim();
    if candidate.is_empty() || candidate.contains(['\n', '\r']) {
        return None;
    }
    if let Some(rest) = candidate.strip_prefix("file://") {
        if !rest.starts_with('/') {
            return None;
        }
        return decode_percent_path(rest.as_bytes());
    }
    if let Some(inner) = candidate
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
    {
        if inner.is_empty() {
            return None;
        }
        return Some(PathBuf::from(inner));
    }
    let mut unescaped = String::with_capacity(candidate.len());
    let mut chars = candidate.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some(escaped) => unescaped.push(escaped),
                None => unescaped.push('\\'),
            }
        } else {
            unescaped.push(ch);
        }
    }
    Some(PathBuf::from(unescaped))
}

fn decode_percent_path(bytes: &[u8]) -> Option<PathBuf> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hi = hex_nibble(*bytes.get(index + 1)?)?;
            let lo = hex_nibble(*bytes.get(index + 2)?)?;
            let decoded = (hi << 4) | lo;
            if decoded == 0 {
                return None;
            }
            out.push(decoded);
            index += 3;
        } else {
            if bytes[index] == 0 {
                return None;
            }
            out.push(bytes[index]);
            index += 1;
        }
    }
    Some(PathBuf::from(OsString::from_vec(out)))
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn paste_with_newlines_never_submits() {
        // Drive the real paste path: a bracketed-paste payload must reach the
        // buffer as `KeyAction::Paste`, never as `Submit`, however many line
        // separators it carries and whichever convention they use.
        for payload in ["a\nb\nc", "a\r\nb\r\nc", "a\rb\rc"] {
            let mut buf = InputBuffer::new();
            buf.insert_paste(payload);
            assert_eq!(buf.rows(), 3, "paste {payload:?} must stay three lines");
            assert_eq!(buf.text(), "a\nb\nc", "paste {payload:?} normalises to LF");
        }

        // The newline characters inside a paste are payload, not keystrokes:
        // no key event derived from them may resolve to Submit.
        for code in [KeyCode::Char('\n'), KeyCode::Char('\r')] {
            assert!(
                !matches!(
                    resolve_key(key(code, KeyModifiers::NONE), false, false),
                    KeyAction::Submit
                ),
                "{code:?} inside a paste must not submit"
            );
        }
    }

    #[test]
    fn enter_modifiers_resolve_submit_or_newline() {
        for (name, modifiers, kitty, buffer_empty, expected) in [
            (
                "plain_enter_submits",
                KeyModifiers::NONE,
                false,
                true,
                KeyAction::Submit,
            ),
            (
                "alt_enter_newlines",
                KeyModifiers::ALT,
                false,
                true,
                KeyAction::Newline,
            ),
            (
                "shift_enter_without_kitty_submits",
                KeyModifiers::SHIFT,
                false,
                true,
                KeyAction::Submit,
            ),
            (
                "shift_enter_with_kitty_newlines",
                KeyModifiers::SHIFT,
                true,
                true,
                KeyAction::Newline,
            ),
        ] {
            let actual = resolve_key(key(KeyCode::Enter, modifiers), kitty, buffer_empty);
            assert!(
                matches!(
                    (actual, expected),
                    (KeyAction::Submit, KeyAction::Submit)
                        | (KeyAction::Newline, KeyAction::Newline)
                ),
                "{name}"
            );
        }
    }

    fn sample_pending_image(
        seed: u8,
        media_type: lattice_local::ImageMediaType,
        byte_length: u64,
        name: &str,
    ) -> PendingImage {
        PendingImage {
            meta: ImageRef {
                sha256: [seed; 32],
                media_type,
                byte_length,
            },
            source: ImageSource::File(PathBuf::from(name)),
        }
    }

    #[test]
    fn image_atoms_move_and_delete_whole_and_typed_marker_edits_char_by_char() {
        let mut buf = InputBuffer::new();
        for ch in "甲乙".chars() {
            buf.insert_char(ch);
        }
        let img1 = sample_pending_image(0x11, lattice_local::ImageMediaType::Png, 1024, "a.png");
        let img2 = sample_pending_image(0x22, lattice_local::ImageMediaType::Jpeg, 2048, "b.jpg");
        buf.insert_image(img1.clone());
        buf.insert_image(img2.clone());
        buf.newline();
        buf.insert_char('丙');

        assert!(!buf.is_empty(), "包含图片与文字时缓冲不为空");
        assert_eq!(buf.image_count(), 2, "应记录两张待发图片");
        assert_eq!(buf.text(), "甲乙\n丙", "text() 只拼接文字与换行");
        assert_eq!(
            crate::view::input_visual(&buf, 200).0,
            vec![
                "甲乙[IMAGE 1 · png · 1 KiB][IMAGE 2 · jpg · 2 KiB]".to_owned(),
                "丙".to_owned(),
            ],
            "两张图片应按顺序渲染为 1 和 2"
        );

        // 光标移回第一行末尾，验证 ← / → 一步跨过整张图片原子。
        buf.cursor = (0, 4);
        buf.move_left();
        assert_eq!(buf.cursor, (0, 3), "向左一步应整块跨过第二张图片");
        buf.move_left();
        assert_eq!(buf.cursor, (0, 2), "再向左一步应整块跨过第一张图片");
        buf.move_right();
        assert_eq!(buf.cursor, (0, 3), "向右一步应整块跨过第一张图片");

        // 光标在第一张图片右侧时按 Backspace 整块删除，剩余图片序号自动变为 1。
        buf.backspace();
        assert_eq!(buf.cursor, (0, 2), "Backspace 删除图片后光标应回到图片左侧");
        assert_eq!(buf.image_count(), 1, "删除后应剩一张图片");
        assert_eq!(
            crate::view::input_visual(&buf, 200).0,
            vec!["甲乙[IMAGE 1 · jpg · 2 KiB]".to_owned(), "丙".to_owned()],
            "Backspace 删除首图后，剩余图片的序号应更新为 1"
        );

        // 重新插回首图，再将光标停在首图左侧按 Delete 整块删除，序号同样更新为 1。
        buf.insert_image(img1);
        assert_eq!(
            crate::view::input_visual(&buf, 200).0,
            vec![
                "甲乙[IMAGE 1 · png · 1 KiB][IMAGE 2 · jpg · 2 KiB]".to_owned(),
                "丙".to_owned(),
            ],
            "重新插入后两张图片序号应恢复为 1 和 2"
        );
        buf.move_left();
        assert_eq!(buf.cursor, (0, 2));
        buf.delete();
        assert_eq!(buf.cursor, (0, 2), "Delete 删除图片后光标位置保持不变");
        assert_eq!(buf.image_count(), 1);
        assert_eq!(
            crate::view::input_visual(&buf, 200).0,
            vec!["甲乙[IMAGE 1 · jpg · 2 KiB]".to_owned(), "丙".to_owned()],
            "Delete 删除首图后，剩余图片的序号应更新为 1"
        );

        // 第二行行首按 Backspace 接回上一行，保持文字与图片的交错顺序。
        buf.cursor = (1, 0);
        buf.backspace();
        assert_eq!(buf.rows(), 1, "跨行删除应合并回单行");
        assert!(
            buf.parts()
                == vec![
                    UserInputPart::Text {
                        text: "甲乙".to_owned(),
                    },
                    UserInputPart::Image { image: img2 },
                    UserInputPart::Text {
                        text: "丙".to_owned(),
                    },
                ],
            "跨行合并后应保留图文顺序"
        );

        // 手打的 `[IMAGE …]` 文字仍按普通文字逐字符移动和删除。
        let mut typed = InputBuffer::new();
        let literal = "[IMAGE 1 · png · 1 KiB]";
        for ch in literal.chars() {
            typed.insert_char(ch);
        }
        assert_eq!(typed.image_count(), 0, "手打标记文字不应识别为图片原子");
        typed.move_left();
        assert_eq!(
            typed.cursor,
            (0, char_len(literal) - 1),
            "手打标记向左只移动一个字符"
        );
        typed.delete();
        typed.backspace();
        assert_eq!(
            typed.text(),
            "[IMAGE 1 · png · 1 Ki",
            "手打标记应逐字符删除"
        );
    }

    #[test]
    fn paste_recognizes_image_paths_falls_back_to_text_and_reports_oversized_images() {
        let dir = tempfile::tempdir().expect("创建粘贴测试临时目录");
        let escaped_png = dir.path().join("has space.png");
        std::fs::write(&escaped_png, b"\x89PNG\r\n\x1a\npayload").expect("写入 PNG");
        let quoted_webp = dir.path().join("quoted file.webp");
        std::fs::write(&quoted_webp, b"RIFF\x24\x00\x00\x00WEBPVP8 ").expect("写入 WebP");
        let uri_gif = dir.path().join("uri file.gif");
        std::fs::write(&uri_gif, b"GIF89a\x01\x00\x01\x00").expect("写入 GIF");
        let misnamed_jpg = dir.path().join("disguised.txt");
        std::fs::write(&misnamed_jpg, b"\xff\xd8\xff\xe0jpeg-body").expect("写入伪装扩展名 JPEG");

        let mut buf = InputBuffer::new();
        buf.paste(&escaped_png.to_str().unwrap().replace(' ', "\\ "))
            .expect("反斜杠转义空格路径应成功粘贴图片");
        buf.paste(&format!("  '{}'  ", quoted_webp.display()))
            .expect("单引号包裹路径应成功粘贴图片");
        buf.paste(&format!(
            "file://{}",
            uri_gif.to_str().unwrap().replace(' ', "%20")
        ))
        .expect("file:/// 加 %20 路径应成功粘贴图片");
        buf.paste(misnamed_jpg.to_str().unwrap())
            .expect("扩展名与内容不符的图片应按魔数识别");

        assert_eq!(buf.image_count(), 4, "四种路径形式均应插入图片原子");
        assert!(buf.text().is_empty(), "识别为图片时不应插入路径文字");
        let media_types: Vec<_> = buf
            .parts()
            .into_iter()
            .filter_map(|part| match part {
                UserInputPart::Image { image } => Some(image.meta.media_type),
                UserInputPart::Text { .. } => None,
            })
            .collect();
        assert_eq!(
            media_types,
            vec![
                lattice_local::ImageMediaType::Png,
                lattice_local::ImageMediaType::Webp,
                lattice_local::ImageMediaType::Gif,
                lattice_local::ImageMediaType::Jpeg,
            ],
            "识别出的图片媒体类型顺序应与文件头魔数一致"
        );

        // 不存在的路径、目录、普通文本文件、超过 3.75 MiB 的普通文件、多行文字都按文字插入。
        let missing = dir.path().join("missing.png");
        let folder = dir.path().join("sub_folder");
        std::fs::create_dir(&folder).expect("创建子目录");
        let plain_text = dir.path().join("plain.png");
        std::fs::write(&plain_text, b"not an image at all").expect("写入普通文本文件");
        let oversized_plain = dir.path().join("oversized.bin");
        std::fs::write(
            &oversized_plain,
            vec![b'x'; lattice_local::MAX_IMAGE_BYTES + 128],
        )
        .expect("写入超限普通文件");
        let multiline = format!("{}\n第二行说明", escaped_png.display());

        for raw in [
            missing.to_str().unwrap().to_owned(),
            folder.to_str().unwrap().to_owned(),
            plain_text.to_str().unwrap().to_owned(),
            oversized_plain.to_str().unwrap().to_owned(),
            multiline,
        ] {
            let mut text_buf = InputBuffer::new();
            text_buf
                .paste(&raw)
                .expect("非图片与不可读路径应回退按普通文字粘贴");
            assert_eq!(text_buf.image_count(), 0, "回退为普通文字时不应插入图片");
            assert_eq!(text_buf.text(), raw, "回退为普通文字时应保留原始粘贴内容");
        }

        // 超过 3.75 MiB 的真图片：推 Error 状态消息且不向输入区插入任何内容。
        let oversized_png = dir.path().join("too_large.png");
        let mut big_png = vec![0xAB; lattice_local::MAX_IMAGE_BYTES + 1];
        big_png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        std::fs::write(&oversized_png, &big_png).expect("写入超限 PNG");

        let session_id =
            lattice_local::SessionId::parse("11111111111141118111111111111111").unwrap();
        let mut app = crate::app::App::new(
            crate::session::SessionContext {
                session_id,
                initial_head: lattice_local::SessionHead {
                    session_id,
                    position: lattice_local::NodeId(0),
                    agent: "coder".into(),
                    model: None,
                    provider: None,
                    effort: None,
                    settings_version: serde_json::from_value(serde_json::json!("11".repeat(32)))
                        .unwrap(),
                    turn_id: None,
                    owner: false,
                    diagnostic: None,
                    path_replaced: false,
                },
                initial_adjustment: None,
            },
            false,
            &dir.path().join("history"),
            dir.path().to_path_buf(),
        );
        app.apply_action(KeyAction::Paste(oversized_png.to_str().unwrap().to_owned()));
        assert!(app.input.is_empty(), "超限真图片粘贴失败后输入区应保持为空");
        assert_eq!(app.input.image_count(), 0, "超限真图片不应插入输入缓冲");
        let parked = app
            .dock
            .message()
            .expect("超限真图片应在 Dock 暂驻一条错误状态");
        assert_eq!(parked.level, lattice_local::NoticeLevel::Error);
        assert!(
            parked.text.contains("3,932,160"),
            "错误状态应写明单图字节上限：{}",
            parked.text
        );
    }
}
