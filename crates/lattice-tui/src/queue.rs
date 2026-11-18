// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 本客户端的待发送内容。编辑器持有目标，草稿和排队不能同时编辑。

use crate::blocks::{BlockId, BlockList};
use crate::comment::{Anchor, Comment, Location};
use crate::input::{InputBuffer, KeyAction};
use std::collections::VecDeque;

const LIMIT: usize = 5;

/// 排队消息对应的预期设置版本：入队时固定的快照，或绑定入队时在途 configure 的代次。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueuedSettings {
    Snapshot(lattice_local::SettingsVersion),
    PendingConfigure {
        generation: u64,
        fallback: lattice_local::SettingsVersion,
    },
}

/// 由输入缓冲和已验证锚点的草稿组成，不从历史消息恢复可编辑草稿。
#[derive(Clone)]
pub(crate) struct Outgoing {
    pub(crate) input: InputBuffer,
    pub(crate) comments: Vec<Comment>,
    pub(crate) expected_settings: lattice_local::SettingsVersion,
}

pub(crate) struct QueuedMessage {
    pub(crate) id: BlockId,
    pub(crate) input: InputBuffer,
    pub(crate) comments: Vec<Comment>,
    settings: QueuedSettings,
}

impl QueuedMessage {
    fn is_empty(&self) -> bool {
        is_blank_parts(&self.input, &self.comments)
    }
}

fn is_blank_parts(input: &InputBuffer, comments: &[Comment]) -> bool {
    input.is_blank() && comments.is_empty()
}

fn step_present(
    comment: &Comment,
    steps: &std::collections::BTreeSet<lattice_local::StepKey>,
) -> bool {
    let fragment = &comment.location.fragment;
    steps.contains(&lattice_local::StepKey {
        turn_id: fragment.turn,
        ordinal: fragment.step,
    })
}

fn retain_comments(
    comments: &[Comment],
    steps: Option<&std::collections::BTreeSet<lattice_local::StepKey>>,
) -> Vec<Comment> {
    comments
        .iter()
        .filter(|comment| steps.is_none_or(|steps| step_present(comment, steps)))
        .cloned()
        .collect()
}

fn normalized_input(mut input: InputBuffer) -> InputBuffer {
    if input.is_blank() {
        input.clear();
    }
    input
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
    awaiting: Option<(lattice_local::TurnId, QueuedMessage)>,
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

    pub(crate) fn send_candidate(
        &self,
        steps: Option<&std::collections::BTreeSet<lattice_local::StepKey>>,
    ) -> Option<Outgoing> {
        if self.paused || self.is_editing() || self.awaiting.is_some() {
            return None;
        }
        if !matches!(self.messages.front()?.settings, QueuedSettings::Snapshot(_)) {
            return None;
        }
        for message in &self.messages {
            let comments = retain_comments(&message.comments, steps);
            if is_blank_parts(&message.input, &comments) {
                continue;
            }
            let QueuedSettings::Snapshot(expected_settings) = message.settings else {
                return None;
            };
            return Some(Outgoing {
                input: message.input.clone(),
                comments,
                expected_settings,
            });
        }
        None
    }

    pub(crate) fn start_send(&mut self, turn: lattice_local::TurnId) -> Option<Outgoing> {
        if self.paused || self.is_editing() || self.awaiting.is_some() {
            return None;
        }
        let QueuedSettings::Snapshot(expected_settings) = self.messages.front()?.settings else {
            return None;
        };
        let message = self.messages.pop_front()?;
        let payload = Outgoing {
            input: message.input.clone(),
            comments: message.comments.clone(),
            expected_settings,
        };
        self.awaiting = Some((turn, message));
        Some(payload)
    }
    pub(crate) fn release(&mut self, turn: lattice_local::TurnId) -> bool {
        if self.awaiting.as_ref().is_some_and(|(id, _)| *id == turn) {
            self.awaiting = None;
            true
        } else {
            false
        }
    }
    pub(crate) fn reject(&mut self, turn: lattice_local::TurnId) -> bool {
        if self.awaiting.as_ref().is_none_or(|(id, _)| *id != turn) {
            return false;
        }
        if let Some((_, message)) = self.awaiting.take() {
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
        let input = match &target {
            EditTarget::Queued { message, .. } => {
                let mut input = message.input.clone();
                input.move_to_end();
                input
            }
            EditTarget::NewComment(draft) | EditTarget::SavedComment(draft) => {
                let mut input = InputBuffer::new();
                input.set_text(&draft.comment.text);
                input
            }
        };
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
                    message.input = normalized_input(input);
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
    pub(crate) fn queued_editor_mut(&mut self) -> Option<&mut InputBuffer> {
        let editor = self.editor.as_mut()?;
        matches!(editor.target, EditTarget::Queued { .. }).then_some(&mut editor.input)
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
    pub(crate) fn draft_comments(&self) -> Vec<Comment> {
        self.draft_comments_in(None)
    }
    pub(crate) fn draft_comments_in(
        &self,
        steps: Option<&std::collections::BTreeSet<lattice_local::StepKey>>,
    ) -> Vec<Comment> {
        let mut comments: Vec<Comment> = self
            .drafts
            .iter()
            .map(|d| &d.comment)
            .filter(|comment| steps.is_none_or(|steps| step_present(comment, steps)))
            .cloned()
            .collect();
        comments.sort_by_key(|c| c.n);
        comments
    }
    pub(crate) fn take_message(
        &mut self,
        input: InputBuffer,
        expected_settings: lattice_local::SettingsVersion,
    ) -> Outgoing {
        self.drafts.sort_by_key(|d| d.comment.n);
        Outgoing {
            input: normalized_input(input),
            comments: self.drafts.drain(..).map(|d| d.comment).collect(),
            expected_settings,
        }
    }
    pub(crate) fn enqueue(
        &mut self,
        id: BlockId,
        input: InputBuffer,
        settings: QueuedSettings,
    ) -> bool {
        if self.is_full() || (input.is_blank() && self.drafts.is_empty()) {
            return false;
        }
        self.drafts.sort_by_key(|d| d.comment.n);
        self.messages.push_back(QueuedMessage {
            id,
            input: normalized_input(input),
            comments: self.drafts.drain(..).map(|d| d.comment).collect(),
            settings,
        });
        true
    }
    pub(crate) fn resolve_configure(
        &mut self,
        target: u64,
        version: lattice_local::SettingsVersion,
    ) -> bool {
        self.resolve_configure_with(target, |_| version)
    }
    pub(crate) fn fail_configure(&mut self, target: u64) -> bool {
        self.resolve_configure_with(target, |fallback| fallback)
    }
    fn resolve_configure_with(
        &mut self,
        target: u64,
        pick: impl Fn(lattice_local::SettingsVersion) -> lattice_local::SettingsVersion,
    ) -> bool {
        let mut resolved = false;
        for message in &mut self.messages {
            if let QueuedSettings::PendingConfigure {
                generation,
                fallback,
            } = message.settings
                && generation == target
            {
                message.settings = QueuedSettings::Snapshot(pick(fallback));
                resolved = true;
            }
        }
        if let Some(Editor {
            target: EditTarget::Queued { message, .. },
            ..
        }) = &mut self.editor
            && let QueuedSettings::PendingConfigure {
                generation,
                fallback,
            } = message.settings
            && generation == target
        {
            message.settings = QueuedSettings::Snapshot(pick(fallback));
            resolved = true;
        }
        resolved
    }
    pub(crate) fn restore(&mut self, message: Outgoing, blocks: &mut BlockList) -> InputBuffer {
        self.drafts
            .extend(message.comments.into_iter().map(|comment| Draft {
                id: blocks.allocate_id(),
                comment,
            }));
        message.input
    }
    pub(crate) fn retain_steps(
        &mut self,
        steps: &std::collections::BTreeSet<lattice_local::StepKey>,
    ) -> Vec<usize> {
        let mut dropped = Vec::new();
        let mut keep = |comment: &Comment| {
            let present = step_present(comment, steps);
            if !present {
                dropped.push(comment.n);
            }
            present
        };
        self.drafts.retain(|d| keep(&d.comment));
        for message in &mut self.messages {
            message.comments.retain(&mut keep);
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
                    message.comments.retain(&mut keep);
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
        self.messages
            .drain(..)
            .map(|m| m.input.text())
            .filter(|text| !text.trim().is_empty())
    }
}
