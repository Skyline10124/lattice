// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 本客户端的待发送内容。编辑器持有目标，草稿和排队不能同时编辑。

use crate::blocks::{BlockId, BlockList};
use crate::comment::{Anchor, Comment, Location, Message};
use crate::input::{InputBuffer, KeyAction};
use std::collections::VecDeque;

const LIMIT: usize = 5;

/// 仅由输入文字和已验证锚点的草稿组成，不从历史消息恢复可编辑草稿。
#[derive(Clone)]
pub(crate) struct Outgoing(Message);
impl std::ops::Deref for Outgoing {
    type Target = Message;
    fn deref(&self) -> &Message {
        &self.0
    }
}

pub(crate) struct QueuedMessage {
    pub(crate) id: BlockId,
    message: Outgoing,
}
impl std::ops::Deref for QueuedMessage {
    type Target = Message;
    fn deref(&self) -> &Message {
        &self.message
    }
}

pub(crate) struct Draft {
    pub(crate) id: BlockId,
    comment: Comment,
}
impl Draft {
    pub(crate) fn comment(&self) -> &Comment {
        &self.comment
    }
}

enum EditTarget {
    Queued {
        message: QueuedMessage,
        position: usize,
    },
    NewComment(Draft),
    SavedComment(Draft),
}
struct Editor {
    target: EditTarget,
    input: InputBuffer,
}

#[derive(Default)]
pub(crate) struct Queue {
    messages: VecDeque<QueuedMessage>,
    // 已离开可编辑列表，收到接受或未知回执前仍占名额。
    awaiting: Option<(lattice_api::local::TurnId, QueuedMessage)>,
    // 结果未知、已让出名额的一条，留到结果确定：之后确定未被接受时照常放回队首。
    unknown: Option<(lattice_api::local::TurnId, QueuedMessage)>,
    paused: bool,
    drafts: Vec<Draft>,
    editor: Option<Editor>,
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
    pub(crate) fn is_editing(&self) -> bool {
        self.editor.is_some()
    }

    pub(crate) fn start_send(&mut self, turn: lattice_api::local::TurnId) -> Option<Outgoing> {
        if self.paused || self.is_editing() || self.awaiting.is_some() {
            return None;
        }
        let message = self.messages.pop_front()?;
        let payload = message.message.clone();
        self.awaiting = Some((turn, message));
        Some(payload)
    }
    pub(crate) fn release(&mut self, turn: lattice_api::local::TurnId) -> bool {
        if self.awaiting.as_ref().is_some_and(|(id, _)| *id == turn) {
            self.awaiting = None;
            true
        } else {
            false
        }
    }
    /// 提交结果未知：让出名额，原文留到结果确定。
    pub(crate) fn set_unknown(&mut self, turn: lattice_api::local::TurnId) -> bool {
        if self.awaiting.as_ref().is_some_and(|(id, _)| *id == turn) {
            self.unknown = self.awaiting.take();
            true
        } else {
            false
        }
    }
    pub(crate) fn reject(&mut self, turn: lattice_api::local::TurnId) -> bool {
        let Some(slot) = [&mut self.awaiting, &mut self.unknown]
            .into_iter()
            .find(|slot| slot.as_ref().is_some_and(|(id, _)| *id == turn))
        else {
            return false;
        };
        if let Some((_, message)) = slot.take() {
            if let Some(Editor {
                target: EditTarget::Queued { position, .. },
                ..
            }) = &mut self.editor
            {
                *position = position.saturating_add(1);
            }
            self.messages.push_front(message);
            self.pause();
        }
        true
    }
    pub(crate) fn messages(&self) -> impl Iterator<Item = &QueuedMessage> {
        let (position, edited) = match self.editor.as_ref().map(|editor| &editor.target) {
            Some(EditTarget::Queued { message, position }) => (*position, Some(message)),
            _ => (self.messages.len(), None),
        };
        self.messages
            .iter()
            .take(position)
            .chain(edited)
            .chain(self.messages.iter().skip(position))
    }
    pub(crate) fn drafts(&self) -> impl Iterator<Item = &Draft> {
        let edited = match self.editor.as_ref().map(|editor| &editor.target) {
            Some(EditTarget::NewComment(draft) | EditTarget::SavedComment(draft)) => Some(draft),
            _ => None,
        };
        self.drafts.iter().chain(edited)
    }
    pub(crate) fn draft_count(&self) -> usize {
        let saved = self
            .editor
            .as_ref()
            .is_some_and(|e| matches!(e.target, EditTarget::SavedComment(_)));
        self.drafts.len().saturating_add(usize::from(saved))
    }
    pub(crate) fn pending_comment_id(&self, n: usize) -> Option<BlockId> {
        self.drafts()
            .find(|draft| draft.comment.n == n)
            .map(|draft| draft.id)
            .or_else(|| {
                self.messages()
                    .find(|message| message.comments.iter().any(|c| c.n == n))
                    .map(|m| m.id.comment(n, 0))
            })
    }
    pub(crate) fn editor(&self, id: BlockId) -> Option<&InputBuffer> {
        let editor = self.editor.as_ref()?;
        let target = match &editor.target {
            EditTarget::Queued { message, .. } => message.id,
            EditTarget::NewComment(draft) | EditTarget::SavedComment(draft) => draft.id,
        };
        (target == id).then_some(&editor.input)
    }
    pub(crate) fn start_edit(&mut self, id: BlockId) -> bool {
        if self.is_editing() {
            return false;
        }
        let target = if let Some(position) = self.messages.iter().position(|m| m.id == id) {
            let Some(message) = self.messages.remove(position) else {
                return false;
            };
            EditTarget::Queued { message, position }
        } else if let Some(position) = self.drafts.iter().position(|d| d.id == id) {
            EditTarget::SavedComment(self.drafts.remove(position))
        } else {
            return false;
        };
        let text = match &target {
            EditTarget::Queued { message, .. } => &message.text,
            EditTarget::NewComment(draft) | EditTarget::SavedComment(draft) => &draft.comment.text,
        };
        let mut input = InputBuffer::new();
        input.set_text(text);
        self.editor = Some(Editor { target, input });
        true
    }
    pub(crate) fn start_comment(&mut self, id: BlockId, anchor: Anchor, blocks: &BlockList) {
        if self.is_editing() {
            return;
        }
        let sent = blocks
            .iter()
            .filter_map(|block| match block {
                crate::app::Block::CommentedUser(message)
                | crate::app::Block::RejectedUser(message) => Some(message.comments.iter()),
                _ => None,
            })
            .flatten();
        let max = self
            .drafts()
            .map(|d| d.comment.n)
            .chain(self.messages().flat_map(|m| m.comments.iter().map(|c| c.n)))
            .chain(
                self.awaiting
                    .iter()
                    .flat_map(|(_, m)| m.comments.iter().map(|c| c.n)),
            )
            .chain(sent.map(|c| c.n))
            .max()
            .unwrap_or(0);
        let Some(n) = max.checked_add(1) else {
            return;
        };
        self.editor = Some(Editor {
            target: EditTarget::NewComment(Draft {
                id,
                comment: anchor.into_comment(n),
            }),
            input: InputBuffer::new(),
        });
    }
    pub(crate) fn finish_edit(&mut self, save: bool) -> Option<Location> {
        let Editor { target, input } = self.editor.take()?;
        match target {
            EditTarget::Queued {
                mut message,
                position,
            } => {
                if save {
                    message.message.0.text = input.text();
                    if message.text.trim().is_empty() {
                        message.message.0.text.clear();
                    }
                }
                if !message.is_empty() {
                    self.messages
                        .insert(position.min(self.messages.len()), message);
                }
                None
            }
            EditTarget::NewComment(mut draft) => {
                let location = draft.comment.location.clone();
                if save && !input.text().trim().is_empty() {
                    draft.comment.text = input.text();
                    self.drafts.push(draft);
                }
                Some(location)
            }
            EditTarget::SavedComment(mut draft) => {
                let location = draft.comment.location.clone();
                if save {
                    draft.comment.text = input.text();
                }
                if !draft.comment.text.trim().is_empty() {
                    self.drafts.push(draft);
                }
                Some(location)
            }
        }
    }
    pub(crate) fn edit(&mut self, action: &KeyAction, width: u16) {
        if let Some(editor) = &mut self.editor {
            editor.input.edit(action, width);
        }
    }
    pub(crate) fn remove_draft(&mut self, id: BlockId) -> Option<Location> {
        let position = self.drafts.iter().position(|d| d.id == id)?;
        Some(self.drafts.remove(position).comment.location)
    }
    pub(crate) fn remove(&mut self, id: BlockId) -> bool {
        let Some(position) = self.messages.iter().position(|m| m.id == id) else {
            return false;
        };
        self.messages.remove(position);
        true
    }
    pub(crate) fn is_full(&self) -> bool {
        self.messages()
            .count()
            .saturating_add(usize::from(self.awaiting.is_some()))
            >= LIMIT
    }
    pub(crate) fn take_message(&mut self, text: String) -> Outgoing {
        self.drafts.sort_by_key(|d| d.comment.n);
        Outgoing(Message {
            text: if text.trim().is_empty() {
                String::new()
            } else {
                text
            },
            comments: self.drafts.drain(..).map(|d| d.comment).collect(),
        })
    }
    pub(crate) fn enqueue(&mut self, id: BlockId, text: String) -> bool {
        if self.is_full() || (text.trim().is_empty() && self.drafts.is_empty()) {
            return false;
        }
        let message = self.take_message(text);
        self.messages.push_back(QueuedMessage { id, message });
        true
    }
    pub(crate) fn restore(&mut self, message: Outgoing, blocks: &mut BlockList) -> String {
        self.drafts
            .extend(message.0.comments.into_iter().map(|comment| Draft {
                id: blocks.allocate_id(),
                comment,
            }));
        message.0.text
    }
    pub(crate) fn retain_steps(
        &mut self,
        steps: &std::collections::BTreeSet<lattice_api::local::StepKey>,
    ) -> Vec<usize> {
        let mut dropped = Vec::new();
        let mut keep = |comment: &Comment| {
            let fragment = &comment.location.fragment;
            let present = steps.contains(&lattice_api::local::StepKey {
                turn_id: fragment.turn,
                ordinal: fragment.step,
            });
            if !present {
                dropped.push(comment.n);
            }
            present
        };
        self.drafts.retain(|d| keep(&d.comment));
        for message in &mut self.messages {
            message.message.0.comments.retain(&mut keep);
        }
        let edited_position = self
            .editor
            .as_ref()
            .and_then(|editor| match &editor.target {
                EditTarget::Queued { position, .. } => Some(
                    self.messages
                        .iter()
                        .take(*position)
                        .filter(|m| !m.is_empty())
                        .count(),
                ),
                _ => None,
            });
        self.messages.retain(|m| !m.is_empty());
        let discard_editor = if let Some(editor) = &mut self.editor {
            match &mut editor.target {
                EditTarget::NewComment(draft) | EditTarget::SavedComment(draft) => {
                    !keep(&draft.comment)
                }
                EditTarget::Queued { message, position } => {
                    message.message.0.comments.retain(&mut keep);
                    if let Some(retained_before) = edited_position {
                        *position = retained_before;
                    }
                    false
                }
            }
        } else {
            false
        };
        if discard_editor {
            self.editor = None;
        }
        dropped.sort_unstable();
        dropped.dedup();
        dropped
    }
    pub(crate) fn drain(&mut self) -> impl Iterator<Item = String> + '_ {
        self.finish_edit(false);
        self.messages.drain(..).map(|m| m.message.0.text)
    }
}
