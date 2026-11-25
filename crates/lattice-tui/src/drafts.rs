// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 本客户端的评论草稿与原地评论编辑器。

use crate::blocks::{BlockId, BlockList};
use crate::comment::{Anchor, Comment, Location};
use crate::input::{InputBuffer, KeyAction};
use std::collections::BTreeSet;

/// 由输入缓冲和已验证锚点的草稿组成，不从历史消息恢复可编辑草稿。
#[derive(Clone)]
pub(crate) struct Outgoing {
    pub(crate) input: InputBuffer,
    pub(crate) comments: Vec<Comment>,
}

fn step_present(comment: &Comment, steps: &BTreeSet<lattice_local::StepKey>) -> bool {
    let fragment = &comment.location.fragment;
    steps.contains(&lattice_local::StepKey {
        turn_id: fragment.turn,
        ordinal: fragment.step,
    })
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

struct Editor {
    draft: Draft,
    saved: bool,
    input: InputBuffer,
}

#[derive(Default)]
pub(crate) struct Drafts {
    drafts: Vec<Draft>,
    editor: Option<Editor>,
}

impl Drafts {
    pub(crate) fn is_editing(&self) -> bool {
        self.editor.is_some()
    }

    pub(crate) fn drafts(&self) -> impl Iterator<Item = &Draft> {
        self.drafts
            .iter()
            .chain(self.editor.as_ref().map(|editor| &editor.draft))
    }

    pub(crate) fn draft_count(&self) -> usize {
        let saved = self.editor.as_ref().is_some_and(|editor| editor.saved);
        self.drafts.len().saturating_add(usize::from(saved))
    }

    pub(crate) fn editor(&self, id: BlockId) -> Option<&InputBuffer> {
        self.editor
            .as_ref()
            .filter(|editor| editor.draft.id == id)
            .map(|editor| &editor.input)
    }

    pub(crate) fn start_edit(&mut self, id: BlockId) -> bool {
        if self.is_editing() {
            return false;
        }
        let Some(position) = self.drafts.iter().position(|draft| draft.id == id) else {
            return false;
        };
        let draft = self.drafts.remove(position);
        let mut input = InputBuffer::new();
        input.set_text(&draft.comment.text);
        self.editor = Some(Editor {
            draft,
            saved: true,
            input,
        });
        true
    }

    pub(crate) fn start_comment<'a>(
        &mut self,
        id: BlockId,
        anchor: Anchor,
        blocks: &BlockList,
        in_flight: impl IntoIterator<Item = &'a Comment>,
    ) {
        if self.is_editing() {
            return;
        }
        let sent = blocks
            .iter()
            .filter_map(|block| match block {
                crate::app::Block::CommentedUser(message) => Some(message.comments.iter()),
                _ => None,
            })
            .flatten();
        let max = self
            .drafts()
            .map(|draft| draft.comment.n)
            .chain(sent.map(|comment| comment.n))
            .chain(in_flight.into_iter().map(|comment| comment.n))
            .max()
            .unwrap_or(0);
        let Some(n) = max.checked_add(1) else {
            return;
        };
        self.editor = Some(Editor {
            draft: Draft {
                id,
                comment: anchor.into_comment(n),
            },
            saved: false,
            input: InputBuffer::new(),
        });
    }

    pub(crate) fn finish_edit(&mut self, save: bool) -> Option<Location> {
        let Editor {
            mut draft, input, ..
        } = self.editor.take()?;
        let location = draft.comment.location.clone();
        if save {
            draft.comment.text = input.text();
        }
        if !draft.comment.text.trim().is_empty() {
            self.drafts.push(draft);
        }
        Some(location)
    }

    pub(crate) fn edit(&mut self, action: &KeyAction, width: u16) {
        if let Some(editor) = &mut self.editor {
            editor.input.edit(action, width);
        }
    }

    pub(crate) fn remove_draft(&mut self, id: BlockId) -> Option<Location> {
        let position = self.drafts.iter().position(|draft| draft.id == id)?;
        Some(self.drafts.remove(position).comment.location)
    }

    #[cfg(test)]
    pub(crate) fn draft_comments(&self) -> Vec<Comment> {
        self.draft_comments_in(None)
    }

    pub(crate) fn draft_comments_in(
        &self,
        steps: Option<&BTreeSet<lattice_local::StepKey>>,
    ) -> Vec<Comment> {
        let mut comments: Vec<Comment> = self
            .drafts
            .iter()
            .map(|draft| &draft.comment)
            .filter(|comment| steps.is_none_or(|steps| step_present(comment, steps)))
            .cloned()
            .collect();
        comments.sort_by_key(|comment| comment.n);
        comments
    }

    pub(crate) fn take_message(&mut self, input: InputBuffer) -> Outgoing {
        self.drafts.sort_by_key(|draft| draft.comment.n);
        Outgoing {
            input: normalized_input(input),
            comments: self.drafts.drain(..).map(|draft| draft.comment).collect(),
        }
    }

    pub(crate) fn restore(&mut self, message: Outgoing, blocks: &mut BlockList) -> InputBuffer {
        self.drafts
            .extend(message.comments.into_iter().map(|comment| Draft {
                id: blocks.allocate_id(),
                comment,
            }));
        message.input
    }

    pub(crate) fn retain_steps(&mut self, steps: &BTreeSet<lattice_local::StepKey>) -> Vec<usize> {
        let mut dropped = Vec::new();
        let mut keep = |comment: &Comment| {
            let present = step_present(comment, steps);
            if !present {
                dropped.push(comment.n);
            }
            present
        };
        self.drafts.retain(|draft| keep(&draft.comment));
        if self
            .editor
            .as_ref()
            .is_some_and(|editor| !keep(&editor.draft.comment))
        {
            self.editor = None;
        }
        dropped.sort_unstable();
        dropped.dedup();
        dropped
    }
}
