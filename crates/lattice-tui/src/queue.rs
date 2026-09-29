// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 本客户端内存中的排队消息，不属于持久对话。

use std::collections::VecDeque;

use crate::blocks::BlockId;

const LIMIT: usize = 5;

pub(crate) struct QueuedMessage {
    pub(crate) id: BlockId,
    pub(crate) text: String,
    editor: Option<crate::input::InputBuffer>,
}

impl QueuedMessage {
    pub(crate) fn editor(&self) -> Option<&crate::input::InputBuffer> {
        self.editor.as_ref()
    }
}

#[derive(Default)]
pub(crate) struct Queue {
    messages: VecDeque<QueuedMessage>,
    // 已离开可编辑列表，收到接受或未知回执前仍占名额。
    awaiting: Option<(lattice_api::local::TurnId, QueuedMessage)>,
    paused: bool,
}

impl Queue {
    pub(crate) fn resume(&mut self) {
        self.paused = false;
    }

    pub(crate) fn pause(&mut self) {
        self.paused = true;
    }

    pub(crate) fn is_paused(&self) -> bool {
        self.paused
    }

    pub(crate) fn reject(&mut self, turn_id: lattice_api::local::TurnId) -> bool {
        if self.awaiting.as_ref().is_none_or(|(id, _)| *id != turn_id) {
            return false;
        }
        if let Some((_, message)) = self.awaiting.take() {
            self.messages.push_front(message);
            self.pause();
        }
        true
    }

    pub(crate) fn start_send(&mut self, turn_id: lattice_api::local::TurnId) -> Option<String> {
        if self.paused || self.is_editing() || self.awaiting.is_some() {
            return None;
        }
        let message = self.messages.pop_front()?;
        let text = message.text.clone();
        self.awaiting = Some((turn_id, message));
        Some(text)
    }

    pub(crate) fn release(&mut self, turn_id: lattice_api::local::TurnId) -> bool {
        if self.awaiting.as_ref().is_some_and(|(id, _)| *id == turn_id) {
            self.awaiting = None;
            true
        } else {
            false
        }
    }

    pub(crate) fn is_editing(&self) -> bool {
        self.messages.iter().any(|message| message.editor.is_some())
    }

    pub(crate) fn start_edit(&mut self, id: BlockId) {
        if self.is_editing() {
            return;
        }
        if let Some(message) = self.messages.iter_mut().find(|message| message.id == id) {
            let mut editor = crate::input::InputBuffer::new();
            editor.set_text(&message.text);
            message.editor = Some(editor);
        }
    }

    pub(crate) fn finish_edit(&mut self, save: bool) {
        if let Some(message) = self
            .messages
            .iter_mut()
            .find(|message| message.editor.is_some())
            && let Some(editor) = message.editor.take()
            && save
        {
            message.text = editor.text();
        }
        self.messages
            .retain(|message| !message.text.trim().is_empty());
    }

    pub(crate) fn edit(&mut self, action: &crate::input::KeyAction, width: u16) {
        if let Some(editor) = self
            .messages
            .iter_mut()
            .find_map(|message| message.editor.as_mut())
        {
            editor.edit(action, width);
        }
    }

    pub(crate) fn remove(&mut self, id: BlockId) -> bool {
        let Some(index) = self
            .messages
            .iter()
            .position(|message| message.id == id && message.editor.is_none())
        else {
            return false;
        };
        self.messages.remove(index);
        true
    }

    pub(crate) fn is_full(&self) -> bool {
        self.messages
            .len()
            .saturating_add(usize::from(self.awaiting.is_some()))
            >= LIMIT
    }

    pub(crate) fn enqueue(&mut self, id: BlockId, text: String) -> bool {
        if self.is_full() || text.trim().is_empty() {
            return false;
        }
        self.messages.push_back(QueuedMessage {
            id,
            text,
            editor: None,
        });
        true
    }

    pub(crate) fn drain(&mut self) -> impl Iterator<Item = String> + '_ {
        self.messages.drain(..).map(|message| message.text)
    }

    pub(crate) fn messages(&self) -> impl Iterator<Item = &QueuedMessage> {
        self.messages.iter()
    }
}
