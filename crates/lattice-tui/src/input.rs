// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

pub(crate) struct InputBuffer {
    pub(crate) lines: Vec<String>,
    /// (line index, char index within that line)
    pub(crate) cursor: (usize, usize),
}

#[rustfmt::skip]
impl InputBuffer {
    pub(crate) fn new() -> Self { Self { lines: vec![String::new()], cursor: (0, 0) } }
    pub(crate) fn is_empty(&self) -> bool { self.text().is_empty() }
    pub(crate) fn text(&self) -> String { self.lines.join("\n") }
    pub(crate) fn set_text(&mut self, text: &str) {
        self.lines = if text.is_empty() {
            vec![String::new()]
        } else {
            text.split('\n').map(str::to_string).collect()
        };
        let last = self.lines.len() - 1;
        self.cursor = (last, char_len(&self.lines[last]));
    }
    pub(crate) fn clear(&mut self) { self.lines = vec![String::new()]; self.cursor = (0, 0); }
    pub(crate) fn insert_char(&mut self, c: char) {
        let (row, col) = self.cursor;
        let line = &mut self.lines[row];
        let (left, right) = split_at_char(line, col);
        let mut next = String::with_capacity(left.len() + c.len_utf8() + right.len());
        next.push_str(left); next.push(c); next.push_str(right);
        *line = next;
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
        if let Some(first) = parts.next() { for c in first.chars() { self.insert_char(c); } }
        for part in parts { self.newline(); for c in part.chars() { self.insert_char(c); } }
    }
    pub(crate) fn newline(&mut self) {
        let (row, col) = self.cursor;
        let (left, right) = split_at_char(&self.lines[row], col);
        let right = right.to_string();
        self.lines[row] = left.to_string();
        self.lines.insert(row + 1, right);
        self.cursor = (row + 1, 0);
    }
    pub(crate) fn backspace(&mut self) {
        let (row, col) = self.cursor;
        if col > 0 {
            let (left, right) = split_at_char(&self.lines[row], col);
            let mut left = left.to_string();
            left.pop();
            left.push_str(right);
            self.lines[row] = left;
            self.cursor.1 -= 1;
        } else if row > 0 {
            let current = self.lines.remove(row);
            self.cursor.0 -= 1;
            self.cursor.1 = char_len(&self.lines[self.cursor.0]);
            self.lines[self.cursor.0].push_str(&current);
        }
    }
    pub(crate) fn delete(&mut self) {
        let (row, col) = self.cursor;
        if col < char_len(&self.lines[row]) {
            let (left, right) = split_at_char(&self.lines[row], col);
            let mut right = right.to_string();
            if !right.is_empty() { right.remove(0); }
            self.lines[row] = format!("{left}{right}");
        } else if row + 1 < self.lines.len() {
            let next = self.lines.remove(row + 1);
            self.lines[row].push_str(&next);
        }
    }
    pub(crate) fn move_left(&mut self) { if self.cursor.1 > 0 { self.cursor.1 -= 1; } }
    pub(crate) fn move_right(&mut self) {
        if self.cursor.1 < char_len(&self.lines[self.cursor.0]) { self.cursor.1 += 1; }
    }
    pub(crate) fn home(&mut self) { self.cursor.1 = 0; }
    pub(crate) fn end(&mut self) { self.cursor.1 = char_len(&self.lines[self.cursor.0]); }
    /// Logical line count. The draw path deliberately does **not** use this:
    /// it reserves *wrapped* rows via `Dock::height`, so a long
    /// single line still gets the height it actually occupies.
    #[cfg(test)]
    pub(crate) fn rows(&self) -> usize { self.lines.len().max(1) }
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

    #[test]
    fn control_arrows_select_history_actions() {
        assert!(matches!(
            resolve_key(key(KeyCode::Up, KeyModifiers::CONTROL), false, false),
            KeyAction::HistoryPrev
        ));
        assert!(matches!(
            resolve_key(key(KeyCode::Down, KeyModifiers::CONTROL), false, false),
            KeyAction::HistoryNext
        ));
        let dir = tempfile::tempdir().expect("tempdir");
        let mut history = History::load(dir.path());
        history.push("first");
        history.push("second");
        assert_eq!(history.prev(), Some("second"), "newest entry first");
        assert_eq!(history.prev(), Some("first"));
        assert_eq!(history.next(), Some("second"), "down walks back forward");
    }
}
