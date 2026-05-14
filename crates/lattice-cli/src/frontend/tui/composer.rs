use super::state::{SlashSuggestion, SELECTION_LIST_PAGE_SIZE, SLASH_COMMANDS};

/// Owns input editing, cursor movement, history, and slash-command suggestions.
#[derive(Debug, Clone, Default)]
pub(super) struct ComposerState {
    pub(super) input: String,
    pub(super) input_cursor: usize,
    pub(super) suggestion_index: usize,
    pub(super) suggestion_window_start: usize,
    input_history: Vec<String>,
    history_index: Option<usize>,
    draft_before_history: String,
}

impl ComposerState {
    pub(super) fn set_input(&mut self, input: impl Into<String>) {
        self.input = input.into();
        self.input_cursor = self.input.len();
        self.suggestion_index = 0;
        self.suggestion_window_start = 0;
        self.history_index = None;
        self.draft_before_history.clear();
    }

    pub(super) fn clear(&mut self) {
        self.input.clear();
        self.input_cursor = 0;
        self.suggestion_index = 0;
        self.suggestion_window_start = 0;
        self.exit_history_mode();
    }

    pub(super) fn is_empty(&self) -> bool {
        self.input.is_empty()
    }

    pub(super) fn has_history_mode(&self) -> bool {
        self.history_index.is_some()
    }

    pub(super) fn has_history(&self) -> bool {
        !self.input_history.is_empty()
    }

    pub(super) fn insert_char(&mut self, ch: char) {
        self.exit_history_mode();
        self.input.insert(self.input_cursor, ch);
        self.input_cursor += ch.len_utf8();
        self.clamp_suggestion_index();
    }

    pub(super) fn insert_text(&mut self, text: &str) {
        self.exit_history_mode();
        for ch in text.chars() {
            self.input.insert(self.input_cursor, ch);
            self.input_cursor += ch.len_utf8();
        }
        self.clamp_suggestion_index();
    }

    pub(super) fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    pub(super) fn backspace(&mut self) {
        if self.input_cursor == 0 {
            return;
        }
        let prev = prev_char_boundary(&self.input, self.input_cursor);
        self.input.remove(prev);
        self.input_cursor = prev;
        self.clamp_suggestion_index();
    }

    pub(super) fn delete(&mut self) {
        if self.input_cursor < self.input.len() && self.input.is_char_boundary(self.input_cursor) {
            self.input.remove(self.input_cursor);
            self.clamp_suggestion_index();
        }
    }

    pub(super) fn move_left(&mut self) {
        if self.input_cursor > 0 {
            self.input_cursor = prev_char_boundary(&self.input, self.input_cursor);
        }
    }

    pub(super) fn move_right(&mut self) {
        if self.input_cursor < self.input.len() {
            let len = char_byte_len(&self.input, self.input_cursor);
            self.input_cursor = (self.input_cursor + len).min(self.input.len());
        }
    }

    pub(super) fn move_home(&mut self) {
        self.input_cursor = 0;
    }

    pub(super) fn move_end(&mut self) {
        self.input_cursor = self.input.len();
    }

    pub(super) fn add_to_history(&mut self, text: String) {
        if self.input_history.last().map(|s| s.as_str()) != Some(text.as_str()) {
            self.input_history.push(text);
        }
        self.history_index = None;
    }

    pub(super) fn navigate_history_prev(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        let idx = match self.history_index {
            None => {
                self.draft_before_history = std::mem::take(&mut self.input);
                self.input_history.len().saturating_sub(1)
            }
            Some(i) if i > 0 => i - 1,
            _ => return,
        };
        self.history_index = Some(idx);
        self.input = self.input_history[idx].clone();
        self.input_cursor = self.input.len();
    }

    pub(super) fn navigate_history_next(&mut self) {
        match self.history_index {
            Some(i) if i + 1 < self.input_history.len() => {
                self.history_index = Some(i + 1);
                self.input = self.input_history[i + 1].clone();
                self.input_cursor = self.input.len();
            }
            Some(_) => {
                self.history_index = None;
                self.input = std::mem::take(&mut self.draft_before_history);
                self.input_cursor = self.input.len();
            }
            None => {}
        }
    }

    pub(super) fn exit_history_mode(&mut self) {
        if self.history_index.is_some() {
            self.history_index = None;
            self.draft_before_history.clear();
        }
    }

    pub(super) fn slash_suggestions(&self) -> Vec<SlashSuggestion> {
        let Some(query) = self.input.strip_prefix('/') else {
            return Vec::new();
        };
        if self.input.chars().any(char::is_whitespace) {
            return Vec::new();
        }

        let query = query.to_ascii_lowercase();
        let mut scored = SLASH_COMMANDS
            .iter()
            .copied()
            .filter_map(|suggestion| {
                let name = suggestion.command.trim_start_matches('/');
                slash_match_score(name, &query).map(|score| (score, suggestion))
            })
            .collect::<Vec<_>>();
        scored.sort_by_key(|(score, suggestion)| (*score, suggestion.name()));
        scored
            .into_iter()
            .map(|(_, suggestion)| suggestion)
            .collect()
    }

    pub(super) fn clamp_suggestion_index(&mut self) {
        let len = self.slash_suggestions().len();
        if len == 0 {
            self.suggestion_index = 0;
            self.suggestion_window_start = 0;
        } else if self.suggestion_index >= len {
            self.suggestion_index = len - 1;
        }
        self.clamp_suggestion_window(len);
    }

    pub(super) fn suggestion_up(&mut self) {
        if self.suggestion_index > 0 {
            self.suggestion_index -= 1;
        }
        if self.suggestion_index < self.suggestion_window_start {
            self.suggestion_window_start = self.suggestion_index;
        }
    }

    pub(super) fn suggestion_down(&mut self) {
        let len = self.slash_suggestions().len();
        if self.suggestion_index + 1 < len {
            self.suggestion_index += 1;
        }
        if self.suggestion_index >= self.suggestion_window_start + SELECTION_LIST_PAGE_SIZE {
            self.suggestion_window_start = self.suggestion_index + 1 - SELECTION_LIST_PAGE_SIZE;
        }
    }

    pub(super) fn accept_selected_suggestion(&mut self) {
        let suggestions = self.slash_suggestions();
        let Some(suggestion) = suggestions.get(self.suggestion_index).copied() else {
            return;
        };
        self.set_input(suggestion.completion());
    }

    fn clamp_suggestion_window(&mut self, suggestion_count: usize) {
        if suggestion_count == 0 {
            self.suggestion_window_start = 0;
            return;
        }

        let max_start = suggestion_count.saturating_sub(SELECTION_LIST_PAGE_SIZE);
        self.suggestion_window_start = self.suggestion_window_start.min(max_start);
        if self.suggestion_index < self.suggestion_window_start {
            self.suggestion_window_start = self.suggestion_index;
        } else if self.suggestion_index >= self.suggestion_window_start + SELECTION_LIST_PAGE_SIZE {
            self.suggestion_window_start = self.suggestion_index + 1 - SELECTION_LIST_PAGE_SIZE;
        }
    }
}

fn prev_char_boundary(s: &str, cursor: usize) -> usize {
    s[..cursor]
        .char_indices()
        .last()
        .map(|(i, _)| i)
        .unwrap_or(0)
}

fn char_byte_len(s: &str, cursor: usize) -> usize {
    s[cursor..]
        .chars()
        .next()
        .map(|c| c.len_utf8())
        .unwrap_or(1)
}

fn slash_match_score(command_name: &str, query: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    if command_name.starts_with(query) {
        return Some(0);
    }
    let words = command_name.split(['-', '_']).collect::<Vec<_>>();
    if words.iter().any(|word| word.starts_with(query)) {
        return Some(1);
    }
    if fuzzy_subsequence(command_name, query) {
        return Some(2);
    }
    None
}

fn fuzzy_subsequence(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let mut chars = needle.chars();
    let mut current = chars.next();
    for ch in haystack.chars() {
        if Some(ch) == current {
            current = chars.next();
            if current.is_none() {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_suggestions_filter_prefixes() {
        let mut composer = ComposerState::default();
        composer.set_input("/st");

        let suggestions = composer.slash_suggestions();

        assert!(suggestions.iter().any(|s| s.command == "/status"));
        assert!(!suggestions.iter().any(|s| s.command == "/clear"));
    }

    #[test]
    fn slash_suggestions_fuzzy_match_command_name() {
        let mut composer = ComposerState::default();
        composer.set_input("/trn");

        let suggestions = composer.slash_suggestions();

        assert!(suggestions.iter().any(|s| s.command == "/transcript"));
    }

    #[test]
    fn accept_selected_suggestion_completes_command_name() {
        let mut composer = ComposerState::default();
        composer.set_input("/f");

        composer.accept_selected_suggestion();

        assert_eq!(composer.input, "/find ");
        assert_eq!(composer.input_cursor, composer.input.len());
    }

    #[test]
    fn suggestion_window_scrolls_only_at_visible_edges() {
        let mut composer = ComposerState::default();
        composer.set_input("/");

        for _ in 0..SELECTION_LIST_PAGE_SIZE - 1 {
            composer.suggestion_down();
        }
        assert_eq!(composer.suggestion_index, SELECTION_LIST_PAGE_SIZE - 1);
        assert_eq!(composer.suggestion_window_start, 0);

        composer.suggestion_down();
        assert_eq!(composer.suggestion_index, SELECTION_LIST_PAGE_SIZE);
        assert_eq!(composer.suggestion_window_start, 1);

        composer.suggestion_up();
        assert_eq!(composer.suggestion_index, SELECTION_LIST_PAGE_SIZE - 1);
        assert_eq!(composer.suggestion_window_start, 1);

        for _ in 0..SELECTION_LIST_PAGE_SIZE - 2 {
            composer.suggestion_up();
        }
        assert_eq!(composer.suggestion_index, 1);
        assert_eq!(composer.suggestion_window_start, 1);

        composer.suggestion_up();
        assert_eq!(composer.suggestion_index, 0);
        assert_eq!(composer.suggestion_window_start, 0);
    }

    #[test]
    fn suggestion_down_reaches_last_command() {
        let mut composer = ComposerState::default();
        composer.set_input("/");
        let suggestions = composer.slash_suggestions();

        for _ in 0..suggestions.len().saturating_sub(1) {
            composer.suggestion_down();
        }

        assert_eq!(composer.suggestion_index, suggestions.len() - 1);
        assert_eq!(
            composer.slash_suggestions()[composer.suggestion_index].command,
            suggestions.last().unwrap().command
        );
    }

    #[test]
    fn unicode_cursor_movement_uses_char_boundaries() {
        let mut composer = ComposerState::default();
        composer.set_input("a你b");

        composer.move_left();
        composer.backspace();

        assert_eq!(composer.input, "ab");
        assert_eq!(composer.input_cursor, "a".len());
    }

    #[test]
    fn history_navigation_restores_draft() {
        let mut composer = ComposerState::default();
        composer.add_to_history("first".into());
        composer.set_input("draft");

        composer.navigate_history_prev();
        assert_eq!(composer.input, "first");

        composer.navigate_history_next();
        assert_eq!(composer.input, "draft");
    }
}
