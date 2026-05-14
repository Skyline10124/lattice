use super::app::App;

impl App {
    pub(super) fn start_search(&mut self, query: String) {
        let query = query.trim().to_string();
        self.activity
            .start_search(&self.messages, self.transcript_mode, query.clone());

        if !self.activity.search.is_active() {
            return;
        }
        if self.activity.search.matches.is_empty() {
            self.scroll_offset = 0;
            self.push_system_message(&format!("No matches for '{query}'."));
        } else {
            self.scroll_to_search_target();
            if let Some((current, total)) = self.activity.search.position() {
                self.push_system_message(&format!("Search: '{query}' ({current}/{total})."));
            }
        }
    }

    pub(super) fn jump_search_next(&mut self) {
        if self.activity.search.matches.is_empty() {
            if self.activity.search.is_active() {
                self.push_system_message(&format!(
                    "No matches for '{}'.",
                    self.activity.search.query
                ));
            }
            return;
        }
        self.activity.jump_search_next();
        self.scroll_to_search_target();
    }

    pub(super) fn jump_search_prev(&mut self) {
        if self.activity.search.matches.is_empty() {
            if self.activity.search.is_active() {
                self.push_system_message(&format!(
                    "No matches for '{}'.",
                    self.activity.search.query
                ));
            }
            return;
        }
        self.activity.jump_search_prev();
        self.scroll_to_search_target();
    }

    pub(super) fn refresh_search_matches(&mut self) {
        self.activity
            .refresh_search_matches(&self.messages, self.transcript_mode);
    }

    pub(super) fn clear_search(&mut self) {
        self.activity.clear_search();
    }

    fn scroll_to_search_target(&mut self) {
        if let Some(offset) = self.activity.target_scroll_offset(&self.messages) {
            self.scroll_offset = offset;
        }
    }
}
