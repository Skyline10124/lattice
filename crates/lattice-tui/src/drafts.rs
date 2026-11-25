// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 本客户端的评论草稿与原地评论编辑器。

use crate::blocks::{BlockId, BlockList};
use crate::comment::{Anchor, DraftComment, Location};
use crate::input::{InputBuffer, KeyAction};

/// 由输入缓冲和本客户端评论草稿组成，不从历史消息恢复可编辑草稿。
#[derive(Clone)]
pub(crate) struct Outgoing {
    pub(crate) input: InputBuffer,
    pub(crate) comments: Vec<DraftComment>,
}

fn normalized_input(mut input: InputBuffer) -> InputBuffer {
    if input.is_blank() {
        input.clear();
    }
    input
}

pub(crate) struct Draft {
    pub(crate) id: BlockId,
    comment: DraftComment,
}

impl Draft {
    pub(crate) fn comment(&self) -> &DraftComment {
        &self.comment
    }
}

/// 结束评论编辑后的焦点恢复依据：保留原锚点位置及已保存草稿的块身份。
pub(crate) struct FinishEdit {
    pub(crate) location: Location,
    pub(crate) saved_id: Option<BlockId>,
}

struct Editor {
    draft: Draft,
    /// 编辑已有草稿时记录其原索引，保存或放弃时原位放回；新建草稿为 `None`。
    saved_index: Option<usize>,
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

    /// 按当前展示顺序返回全部草稿及正在编辑的草稿，并动态赋予局部序号 `#1..`。
    /// 编辑已有草稿时保持原序号；新建未保存草稿排在末尾（`#<drafts.len() + 1>`）。
    pub(crate) fn numbered_drafts(&self) -> Vec<(usize, &Draft)> {
        let mut ordered: Vec<&Draft> = self.drafts.iter().collect();
        if let Some(editor) = &self.editor {
            match editor.saved_index {
                Some(idx) => ordered.insert(idx.min(ordered.len()), &editor.draft),
                None => ordered.push(&editor.draft),
            }
        }
        ordered
            .into_iter()
            .enumerate()
            .map(|(i, draft)| (i.saturating_add(1), draft))
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn drafts(&self) -> impl Iterator<Item = &Draft> {
        self.drafts
            .iter()
            .chain(self.editor.as_ref().map(|editor| &editor.draft))
    }

    pub(crate) fn draft_count(&self) -> usize {
        let saved = self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.saved_index.is_some());
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
            saved_index: Some(position),
            input,
        });
        true
    }

    pub(crate) fn start_comment(&mut self, id: BlockId, anchor: Anchor) {
        if self.is_editing() {
            return;
        }
        self.editor = Some(Editor {
            draft: Draft {
                id,
                comment: anchor.into_draft(),
            },
            saved_index: None,
            input: InputBuffer::new(),
        });
    }

    pub(crate) fn finish_edit(&mut self, save: bool) -> Option<FinishEdit> {
        let Editor {
            mut draft,
            saved_index,
            input,
        } = self.editor.take()?;
        let location = draft.comment.location.clone();
        if save {
            draft.comment.text = input.text();
        }
        let saved_id = if !draft.comment.text.trim().is_empty() {
            let id = draft.id;
            let insert_at = saved_index.map_or(self.drafts.len(), |idx| idx.min(self.drafts.len()));
            self.drafts.insert(insert_at, draft);
            Some(id)
        } else {
            None
        };
        Some(FinishEdit { location, saved_id })
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

    pub(crate) fn draft_comments(&self) -> Vec<DraftComment> {
        self.drafts
            .iter()
            .map(|draft| draft.comment.clone())
            .collect()
    }

    pub(crate) fn take_message(&mut self, input: InputBuffer) -> Outgoing {
        Outgoing {
            input: normalized_input(input),
            comments: self.drafts.drain(..).map(|draft| draft.comment).collect(),
        }
    }

    pub(crate) fn restore(&mut self, message: Outgoing, blocks: &mut BlockList) -> InputBuffer {
        let restored = message.comments.into_iter().map(|comment| Draft {
            id: blocks.allocate_id(),
            comment,
        });
        self.drafts.splice(0..0, restored);
        message.input
    }
}
