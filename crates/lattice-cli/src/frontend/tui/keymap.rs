use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct KeyBinding {
    code: KeyCode,
    modifiers: KeyModifiers,
}

impl KeyBinding {
    pub(super) const fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }

    fn matches(self, key: KeyEvent) -> bool {
        key.code == self.code && key.modifiers == self.modifiers
    }
}

#[derive(Debug, Clone)]
pub(super) struct RuntimeKeymap {
    pub(super) app: AppKeymap,
    pub(super) composer: ComposerKeymap,
    pub(super) pager: PagerKeymap,
    pub(super) list: ListKeymap,
}

#[derive(Debug, Clone)]
pub(super) struct AppKeymap {
    pub(super) quit: Vec<KeyBinding>,
    pub(super) clear: Vec<KeyBinding>,
    pub(super) new_session: Vec<KeyBinding>,
    pub(super) toggle_trace: Vec<KeyBinding>,
    pub(super) find: Vec<KeyBinding>,
    pub(super) next_match: Vec<KeyBinding>,
    pub(super) toggle_tool: Vec<KeyBinding>,
    pub(super) copy: Vec<KeyBinding>,
    pub(super) open_transcript: Vec<KeyBinding>,
    pub(super) open_tool_detail: Vec<KeyBinding>,
    pub(super) toggle_help: Vec<KeyBinding>,
}

#[derive(Debug, Clone)]
pub(super) struct ComposerKeymap {
    pub(super) submit: Vec<KeyBinding>,
    pub(super) newline: Vec<KeyBinding>,
    pub(super) accept_suggestion: Vec<KeyBinding>,
}

#[derive(Debug, Clone)]
pub(super) struct PagerKeymap {
    pub(super) scroll_up: Vec<KeyBinding>,
    pub(super) scroll_down: Vec<KeyBinding>,
    pub(super) page_up: Vec<KeyBinding>,
    pub(super) page_down: Vec<KeyBinding>,
    pub(super) half_page_up: Vec<KeyBinding>,
    pub(super) half_page_down: Vec<KeyBinding>,
    pub(super) jump_top: Vec<KeyBinding>,
    pub(super) jump_bottom: Vec<KeyBinding>,
    pub(super) close: Vec<KeyBinding>,
}

#[derive(Debug, Clone)]
pub(super) struct ListKeymap {
    pub(super) up: Vec<KeyBinding>,
    pub(super) down: Vec<KeyBinding>,
    pub(super) accept: Vec<KeyBinding>,
    pub(super) close: Vec<KeyBinding>,
}

impl Default for RuntimeKeymap {
    fn default() -> Self {
        Self {
            app: AppKeymap {
                quit: vec![ctrl('c')],
                clear: vec![ctrl('l')],
                new_session: vec![ctrl('n')],
                toggle_trace: vec![ctrl('o')],
                find: vec![ctrl('f')],
                next_match: vec![ctrl('g')],
                toggle_tool: vec![ctrl('e')],
                copy: vec![ctrl('y')],
                open_transcript: vec![ctrl('t')],
                open_tool_detail: vec![ctrl('d')],
                toggle_help: vec![plain('?')],
            },
            composer: ComposerKeymap {
                submit: vec![plain_key(KeyCode::Enter)],
                newline: vec![shift_key(KeyCode::Enter)],
                accept_suggestion: vec![plain_key(KeyCode::Tab)],
            },
            pager: PagerKeymap {
                scroll_up: vec![plain_key(KeyCode::Up), plain('k')],
                scroll_down: vec![plain_key(KeyCode::Down), plain('j')],
                page_up: vec![plain_key(KeyCode::PageUp)],
                page_down: vec![plain_key(KeyCode::PageDown)],
                half_page_up: vec![ctrl('u')],
                half_page_down: vec![ctrl('d')],
                jump_top: vec![plain('g')],
                jump_bottom: vec![plain_key(KeyCode::End), plain('G')],
                close: vec![plain_key(KeyCode::Esc), plain('q')],
            },
            list: ListKeymap {
                up: vec![plain_key(KeyCode::Up)],
                down: vec![plain_key(KeyCode::Down)],
                accept: vec![plain_key(KeyCode::Enter)],
                close: vec![plain_key(KeyCode::Esc), plain(' ')],
            },
        }
    }
}

impl RuntimeKeymap {
    pub(super) fn new() -> Self {
        Self::default()
    }
}

pub(super) trait KeyBindingsMatch {
    fn matches_key(&self, key: KeyEvent) -> bool;
}

impl KeyBindingsMatch for [KeyBinding] {
    fn matches_key(&self, key: KeyEvent) -> bool {
        self.iter().any(|binding| binding.matches(key))
    }
}

fn plain(ch: char) -> KeyBinding {
    KeyBinding::new(KeyCode::Char(ch), KeyModifiers::NONE)
}

fn ctrl(ch: char) -> KeyBinding {
    KeyBinding::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
}

fn plain_key(code: KeyCode) -> KeyBinding {
    KeyBinding::new(code, KeyModifiers::NONE)
}

fn shift_key(code: KeyCode) -> KeyBinding {
    KeyBinding::new(code, KeyModifiers::SHIFT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_keymap_matches_named_actions() {
        let keymap = RuntimeKeymap::new();

        assert!(keymap
            .app
            .open_transcript
            .matches_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL,)));
        assert!(keymap
            .pager
            .close
            .matches_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE,)));
    }
}
